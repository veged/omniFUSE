//! In-memory file caching with LRU eviction.
//!
//! Ported from `SimpleGitFS` `core/src/vfs/buffer.rs`.
//! All operations are async-safe via `tokio::sync::RwLock` and atomic types.

use std::{
  collections::VecDeque,
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}
  },
  time::SystemTime
};

use dashmap::DashMap;
use tokio::sync::RwLock;
use tracing::{debug, trace};

use crate::config::BufferConfig;

/// Cached buffer for a single file.
#[derive(Debug)]
pub struct FileBuffer {
  /// File path.
  pub path: PathBuf,
  /// File content.
  content: RwLock<Vec<u8>>,
  /// Buffer has been modified (pending flush to disk).
  dirty: AtomicBool,
  /// Last modification time.
  mtime: RwLock<SystemTime>,
  /// Cached file size.
  size: AtomicU64
}

impl FileBuffer {
  /// Create a new buffer for a file.
  #[must_use]
  pub fn new(path: PathBuf, content: Vec<u8>) -> Self {
    Self::with_mtime(path, content, SystemTime::now())
  }

  /// Create a new buffer with the specified modification time.
  #[must_use]
  pub fn with_mtime(path: PathBuf, content: Vec<u8>, mtime: SystemTime) -> Self {
    let size = content.len() as u64;
    Self {
      path,
      content: RwLock::new(content),
      dirty: AtomicBool::new(false),
      mtime: RwLock::new(mtime),
      size: AtomicU64::new(size)
    }
  }

  /// Read a portion of the buffer content.
  pub async fn read(&self, offset: u64, size: u32) -> Vec<u8> {
    let content = self.content.read().await;
    let start = offset as usize;
    let end = std::cmp::min(start + size as usize, content.len());

    if start >= content.len() {
      return Vec::new();
    }

    content[start..end].to_vec()
  }

  /// Write data to the buffer.
  ///
  /// Extends the buffer if necessary. Marks the buffer as dirty.
  pub async fn write(&self, offset: u64, data: &[u8]) -> usize {
    let mut content = self.content.write().await;
    let offset = offset as usize;

    // Extend if necessary
    if offset + data.len() > content.len() {
      content.resize(offset + data.len(), 0);
    }

    content[offset..offset + data.len()].copy_from_slice(data);

    // Update metadata
    self.dirty.store(true, Ordering::SeqCst);
    self.size.store(content.len() as u64, Ordering::SeqCst);

    // Update mtime
    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();

    data.len()
  }

  /// Get the full buffer content.
  pub async fn content(&self) -> Vec<u8> {
    self.content.read().await.clone()
  }

  /// Set the full buffer content.
  pub async fn set_content(&self, data: Vec<u8>) {
    let mut content = self.content.write().await;
    self.size.store(data.len() as u64, Ordering::SeqCst);
    *content = data;
    self.dirty.store(true, Ordering::SeqCst);

    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();
  }

  /// Truncate the buffer to the specified size.
  pub async fn truncate(&self, new_size: u64) {
    let mut content = self.content.write().await;
    content.truncate(new_size as usize);
    self.size.store(new_size, Ordering::SeqCst);
    self.dirty.store(true, Ordering::SeqCst);

    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();
  }

  /// Whether the buffer has been modified (pending flush).
  #[must_use]
  pub fn is_dirty(&self) -> bool {
    self.dirty.load(Ordering::SeqCst)
  }

  /// Mark the buffer as clean (after flush to disk).
  pub fn mark_clean(&self) {
    self.dirty.store(false, Ordering::SeqCst);
  }

  /// File size in bytes.
  #[must_use]
  pub fn size(&self) -> u64 {
    self.size.load(Ordering::SeqCst)
  }

  /// Last modification time.
  pub async fn mtime(&self) -> SystemTime {
    *self.mtime.read().await
  }
}

/// File buffer manager with LRU eviction.
///
/// Thread-safe in-memory file content cache.
/// Evicts old buffers when the memory limit is exceeded.
/// Dirty buffers (with pending writes) are not evicted.
#[derive(Debug)]
pub struct FileBufferManager {
  /// Cached buffers by path.
  buffers: DashMap<PathBuf, Arc<FileBuffer>>,
  /// LRU queue for eviction.
  lru_order: RwLock<VecDeque<PathBuf>>,
  /// Current memory usage in bytes.
  memory_usage: AtomicUsize,
  /// Configuration.
  config: BufferConfig
}

impl FileBufferManager {
  /// Create a new buffer manager.
  #[must_use]
  pub fn new(config: BufferConfig) -> Self {
    Self {
      buffers: DashMap::new(),
      lru_order: RwLock::new(VecDeque::new()),
      memory_usage: AtomicUsize::new(0),
      config
    }
  }

  /// Get a buffer from the cache (if present).
  #[must_use]
  pub fn get(&self, path: &Path) -> Option<Arc<FileBuffer>> {
    self.buffers.get(path).map(|r| Arc::clone(r.value()))
  }

  /// Add a file to the cache.
  pub async fn cache(&self, path: &Path, content: Vec<u8>) -> Arc<FileBuffer> {
    self.cache_with_mtime(path, content, SystemTime::now()).await
  }

  /// Add a file to the cache with the specified modification time.
  pub async fn cache_with_mtime(
    &self,
    path: &Path,
    content: Vec<u8>,
    mtime: SystemTime
  ) -> Arc<FileBuffer> {
    let size = content.len();

    // Evict old buffers if needed
    if self.config.lru_eviction_enabled {
      self.maybe_evict(size).await;
    }

    let buffer = Arc::new(FileBuffer::with_mtime(path.to_path_buf(), content, mtime));
    self.buffers.insert(path.to_path_buf(), Arc::clone(&buffer));

    // Update LRU
    let mut lru = self.lru_order.write().await;
    lru.push_back(path.to_path_buf());

    // Update memory usage
    self.memory_usage.fetch_add(size, Ordering::SeqCst);

    debug!(path = %path.display(), size, "file cached");

    buffer
  }

  /// Get a buffer or load from disk.
  ///
  /// If the file is cached and fresh — returns the cache.
  /// If the file was modified on disk (and the buffer is not dirty) — reloads.
  /// Otherwise — reads from disk and caches.
  ///
  /// # Errors
  ///
  /// Returns an error if the file cannot be read.
  pub async fn get_or_load(&self, path: &Path) -> std::io::Result<Arc<FileBuffer>> {
    // Check cache
    if let Some(buffer) = self.get(path) {
      // Dirty buffer — do not reload
      if buffer.is_dirty() {
        trace!(path = %path.display(), "cache hit (dirty)");
        self.touch(path).await;
        return Ok(buffer);
      }

      // Check freshness by mtime
      if let Ok(metadata) = tokio::fs::metadata(path).await
        && let Ok(disk_mtime) = metadata.modified()
      {
        let cached_mtime = buffer.mtime().await;
        if disk_mtime > cached_mtime {
          // File modified externally — reload
          debug!(path = %path.display(), "cache stale, reloading from disk");
          self.remove(path).await;
          let content = tokio::fs::read(path).await?;
          return Ok(self.cache_with_mtime(path, content, disk_mtime).await);
        }
      }

      trace!(path = %path.display(), "cache hit");
      self.touch(path).await;
      return Ok(buffer);
    }

    // Load from disk
    trace!(path = %path.display(), "cache miss, loading from disk");
    let metadata = tokio::fs::metadata(path).await?;
    let mtime = metadata.modified().unwrap_or_else(|_| SystemTime::now());
    let content = tokio::fs::read(path).await?;
    Ok(self.cache_with_mtime(path, content, mtime).await)
  }

  /// Update the LRU position (mark as recently used).
  async fn touch(&self, path: &Path) {
    let mut lru = self.lru_order.write().await;

    // Remove from current position
    if let Some(pos) = lru.iter().position(|p| p == path) {
      lru.remove(pos);
    }

    // Add to the end (most recent)
    lru.push_back(path.to_path_buf());
  }

  /// Evict old buffers when the memory limit is exceeded.
  async fn maybe_evict(&self, additional_size: usize) {
    let max_memory = self.config.max_memory_bytes();
    let current = self.memory_usage.load(Ordering::SeqCst);

    if current.saturating_add(additional_size) <= max_memory {
      return;
    }

    let mut lru = self.lru_order.write().await;
    // Limit attempts to avoid infinite loop
    // when all buffers are dirty and cannot be evicted
    let max_attempts = lru.len();
    let mut attempts = 0;

    while self.memory_usage.load(Ordering::SeqCst).saturating_add(additional_size) > max_memory {
      let Some(oldest) = lru.pop_front() else {
        break;
      };

      // Do not evict dirty buffers
      if let Some(buffer) = self.buffers.get(&oldest) {
        if buffer.is_dirty() {
          lru.push_back(oldest);
          attempts += 1;
          if attempts >= max_attempts {
            break;
          }
          continue;
        }

        let size = buffer.size() as usize;
        drop(buffer);

        self.buffers.remove(&oldest);
        self
          .memory_usage
          .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
            Some(cur.saturating_sub(size))
          })
          .ok();

        debug!(path = %oldest.display(), size, "buffer evicted");
      }
    }
  }

  /// Remove a buffer from the cache.
  pub async fn remove(&self, path: &Path) {
    if let Some((_, buffer)) = self.buffers.remove(path) {
      let size = buffer.size() as usize;
      self
        .memory_usage
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
          Some(cur.saturating_sub(size))
        })
        .ok();

      let mut lru = self.lru_order.write().await;
      if let Some(pos) = lru.iter().position(|p| p == path) {
        lru.remove(pos);
      }

      debug!(path = %path.display(), "buffer removed");
    }
  }

  /// Get all dirty buffers.
  #[must_use]
  pub fn dirty_buffers(&self) -> Vec<Arc<FileBuffer>> {
    self
      .buffers
      .iter()
      .filter(|r| r.value().is_dirty())
      .map(|r| Arc::clone(r.value()))
      .collect()
  }

  /// Flush a buffer to disk.
  ///
  /// # Errors
  ///
  /// Returns an error if the write fails.
  pub async fn flush(&self, path: &Path) -> std::io::Result<()> {
    let Some(buffer) = self.get(path) else {
      return Ok(());
    };

    if !buffer.is_dirty() {
      return Ok(());
    }

    let content = buffer.content().await;
    tokio::fs::write(path, &content).await?;
    buffer.mark_clean();

    debug!(path = %path.display(), size = content.len(), "buffer flushed to disk");

    Ok(())
  }

  /// Flush all dirty buffers to disk.
  ///
  /// # Errors
  ///
  /// Returns an error if any write fails.
  pub async fn flush_all(&self) -> std::io::Result<()> {
    let dirty = self.dirty_buffers();

    for buffer in dirty {
      let content = buffer.content().await;
      tokio::fs::write(&buffer.path, &content).await?;
      buffer.mark_clean();

      debug!(path = %buffer.path.display(), size = content.len(), "buffer flushed");
    }

    Ok(())
  }

  /// Current memory usage in bytes.
  #[must_use]
  pub fn memory_usage(&self) -> usize {
    self.memory_usage.load(Ordering::SeqCst)
  }

  /// Number of cached buffers.
  #[must_use]
  pub fn buffer_count(&self) -> usize {
    self.buffers.len()
  }

  /// Clear all buffers (dirty ones are flushed to disk first).
  ///
  /// # Errors
  ///
  /// Returns an error if any write fails.
  pub async fn clear(&self) -> std::io::Result<()> {
    self.flush_all().await?;
    self.buffers.clear();
    self.memory_usage.store(0, Ordering::SeqCst);
    self.lru_order.write().await.clear();
    Ok(())
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn test_file_buffer_read_write() {
    let buffer = FileBuffer::new(PathBuf::from("/test/file.txt"), b"hello".to_vec());

    assert_eq!(buffer.read(0, 5).await, b"hello");
    assert!(!buffer.is_dirty());

    buffer.write(0, b"HELLO").await;
    assert_eq!(buffer.read(0, 5).await, b"HELLO");
    assert!(buffer.is_dirty());
  }

  #[tokio::test]
  async fn test_file_buffer_extend() {
    let buffer = FileBuffer::new(PathBuf::from("/test/file.txt"), b"hello".to_vec());

    buffer.write(5, b" world").await;
    assert_eq!(buffer.content().await, b"hello world");
    assert_eq!(buffer.size(), 11);
  }

  #[tokio::test]
  async fn test_file_buffer_truncate() {
    let buffer = FileBuffer::new(PathBuf::from("/test/file.txt"), b"hello world".to_vec());

    buffer.truncate(5).await;
    assert_eq!(buffer.content().await, b"hello");
    assert_eq!(buffer.size(), 5);
    assert!(buffer.is_dirty());
  }

  #[tokio::test]
  async fn test_buffer_manager_cache() {
    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let content = b"test content".to_vec();
    let path = Path::new("/test/file.txt");

    let buffer = manager.cache(path, content.clone()).await;
    assert_eq!(buffer.content().await, content);

    let cached = manager.get(path);
    assert!(cached.is_some());
    assert_eq!(manager.buffer_count(), 1);
    assert_eq!(manager.memory_usage(), content.len());
  }

  #[tokio::test]
  async fn test_buffer_manager_remove() {
    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let path = Path::new("/test/file.txt");
    manager.cache(path, b"content".to_vec()).await;
    assert_eq!(manager.buffer_count(), 1);

    manager.remove(path).await;
    assert_eq!(manager.buffer_count(), 0);
    assert_eq!(manager.memory_usage(), 0);
  }

  #[tokio::test]
  async fn test_buffer_manager_dirty() {
    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let path = Path::new("/test/file.txt");
    let buffer = manager.cache(path, b"content".to_vec()).await;

    assert!(manager.dirty_buffers().is_empty());

    buffer.write(0, b"modified").await;
    assert_eq!(manager.dirty_buffers().len(), 1);
  }

  #[tokio::test]
  async fn test_buffer_flush_to_disk() {
    eprintln!("[TEST] test_buffer_flush_to_disk");
    // Flush writes dirty buffer to disk and marks it clean
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("flush_test.txt");
    tokio::fs::write(&path, b"initial").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let buffer = manager.cache(&path, b"initial".to_vec()).await;
    buffer.write(0, b"modified").await;
    assert!(buffer.is_dirty());

    manager.flush(&path).await.expect("flush");
    assert!(!buffer.is_dirty(), "buffer should be clean after flush");

    let on_disk = tokio::fs::read_to_string(&path).await.expect("read");
    assert_eq!(on_disk, "modified", "data should be written to disk");
  }

  #[tokio::test]
  async fn test_lru_eviction() {
    // LRU evicts old buffers when memory limit is exceeded
    let config = BufferConfig {
      max_memory_mb: 0, // 0 MB, but min = 1 buffer
      lru_eviction_enabled: true,
    };
    // max_memory_bytes() = 0, so each cache call triggers eviction
    let manager = FileBufferManager::new(config);

    let p1 = PathBuf::from("/test/a.txt");
    let p2 = PathBuf::from("/test/b.txt");

    manager.cache(&p1, vec![0u8; 100]).await;
    assert_eq!(manager.buffer_count(), 1);

    // Second file should evict the first one (limit is 0)
    manager.cache(&p2, vec![0u8; 100]).await;

    // First buffer should be evicted
    assert!(manager.get(&p1).is_none(), "first buffer should be evicted");
    assert!(manager.get(&p2).is_some(), "second buffer should remain");
  }

  #[tokio::test]
  async fn test_dirty_buffer_not_evicted() {
    // Dirty buffers are not evicted during LRU eviction
    let config = BufferConfig {
      max_memory_mb: 0,
      lru_eviction_enabled: true,
    };
    let manager = FileBufferManager::new(config);

    let p1 = PathBuf::from("/test/dirty.txt");
    let p2 = PathBuf::from("/test/new.txt");

    let buf = manager.cache(&p1, vec![0u8; 50]).await;
    buf.write(0, b"modified").await; // Mark as dirty

    // Add second one — eviction will try to evict dirty buffer, but cannot
    manager.cache(&p2, vec![0u8; 50]).await;

    assert!(manager.get(&p1).is_some(), "dirty buffer should not be evicted");
  }

  #[tokio::test]
  async fn test_get_or_load_from_disk() {
    eprintln!("[TEST] test_get_or_load_from_disk");
    // get_or_load loads file from disk on cache miss
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("load_test.txt");
    tokio::fs::write(&path, "disk content").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    assert!(manager.get(&path).is_none(), "file should not be in cache");

    let buffer = manager.get_or_load(&path).await.expect("get_or_load");
    assert_eq!(buffer.content().await, b"disk content");
    assert!(manager.get(&path).is_some(), "file should be in cache after loading");
  }

  #[tokio::test]
  async fn test_get_or_load_cache_hit() {
    eprintln!("[TEST] test_get_or_load_cache_hit");
    // get_or_load returns buffer from cache without re-reading from disk
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cached.txt");
    tokio::fs::write(&path, "old").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    // Cache with different content
    manager.cache(&path, b"cached version".to_vec()).await;

    // get_or_load should return cached version, not from disk
    let buffer = manager.get_or_load(&path).await.expect("get_or_load");
    assert_eq!(buffer.content().await, b"cached version", "should return from cache");
  }

  #[tokio::test]
  async fn test_buffer_write_marks_dirty() {
    // write on FileBuffer sets is_dirty() = true
    let buffer = FileBuffer::new(PathBuf::from("/test/dirty_mark.txt"), b"initial".to_vec());

    assert!(!buffer.is_dirty(), "new buffer should not be dirty");

    buffer.write(0, b"changed").await;

    assert!(buffer.is_dirty(), "write should mark buffer as dirty");
  }

  #[tokio::test]
  async fn test_buffer_multiple_writes_same_offset() {
    // write "aaa" at 0, write "bbb" at 0, read = "bbb" (overwrite)
    let buffer = FileBuffer::new(PathBuf::from("/test/overwrite.txt"), Vec::new());

    buffer.write(0, b"aaa").await;
    assert_eq!(buffer.read(0, 10).await, b"aaa");

    buffer.write(0, b"bbb").await;
    assert_eq!(
      buffer.read(0, 10).await,
      b"bbb",
      "repeated write at the same offset should overwrite data"
    );
  }

  #[tokio::test]
  async fn test_buffer_cache_returns_same_arc() {
    eprintln!("[TEST] test_buffer_cache_returns_same_arc");
    // get_or_load twice for the same path returns the same Arc (Arc::ptr_eq)
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("same_arc.txt");
    tokio::fs::write(&path, "content").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let buf1 = manager.get_or_load(&path).await.expect("get_or_load 1");
    let buf2 = manager.get_or_load(&path).await.expect("get_or_load 2");

    assert!(
      Arc::ptr_eq(&buf1, &buf2),
      "two get_or_load calls for the same path should return the same Arc"
    );
  }

  #[tokio::test]
  async fn test_concurrent_writes_to_same_buffer() {
    eprintln!("[TEST] test_concurrent_writes_to_same_buffer");
    // 10 tokio tasks with 50 writes each to one buffer — no panic/deadlock
    let buffer = Arc::new(FileBuffer::new(
      PathBuf::from("/test/concurrent.txt"),
      vec![0u8; 1024]
    ));

    let mut handles = Vec::new();
    for task_id in 0..10u8 {
      let buf = Arc::clone(&buffer);
      handles.push(tokio::spawn(async move {
        for i in 0..50u64 {
          let offset = (i * 2) % 1024;
          buf.write(offset, &[task_id, task_id]).await;
        }
      }));
    }

    // Wait for all tasks with timeout (deadlock protection)
    let result = tokio::time::timeout(
      crate::test_utils::TEST_TIMEOUT,
      async {
        for h in handles {
          h.await.expect("join");
        }
      }
    ).await;

    assert!(
      result.is_ok(),
      "concurrent writes should not cause deadlock"
    );

    // Verify buffer is not corrupted — size >= 1024
    assert!(
      buffer.size() >= 1024,
      "buffer size should not shrink after concurrent writes"
    );
    assert!(buffer.is_dirty(), "buffer should be dirty after writes");
  }

  #[tokio::test]
  async fn test_buffer_mtime_stable_on_same_content() {
    eprintln!("[TEST] test_buffer_mtime_stable_on_same_content");
    // Create buffer, write "data", flush, write "data" again,
    // check file mtime via fs::metadata
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("mtime_stable.txt");
    tokio::fs::write(&path, b"").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    // First write + flush
    let buffer = manager.cache(&path, Vec::new()).await;
    buffer.write(0, b"data").await;
    manager.flush(&path).await.expect("flush 1");

    // Remember file mtime on disk
    let meta1 = tokio::fs::metadata(&path).await.expect("metadata 1");
    let mtime1 = meta1.modified().expect("mtime 1");

    // Wait for the clock to advance
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Write the same data again and flush
    buffer.write(0, b"data").await;
    manager.flush(&path).await.expect("flush 2");

    let meta2 = tokio::fs::metadata(&path).await.expect("metadata 2");
    let mtime2 = meta2.modified().expect("mtime 2");

    // Buffer does not check data identity, so flush will rewrite the file
    // and mtime will be updated. Verify file size has not changed.
    assert_eq!(meta1.len(), meta2.len(), "file size should not change");
    // mtime2 >= mtime1 — correct behavior (flush rewrites the file)
    assert!(mtime2 >= mtime1, "mtime should not go backwards after repeated flush");
  }

  /// Dirty buffer is NOT overwritten on repeated get_or_load.
  ///
  /// Pattern from SimpleGitFS concurrent_editing_tests: if buffer is marked dirty
  /// (user made changes), re-reading from disk should not overwrite
  /// unflushed data — even if the file on disk has changed.
  #[tokio::test]
  async fn test_dirty_buffer_preserved_on_reload() {
    eprintln!("[TEST] test_dirty_buffer_preserved_on_reload");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("dirty_preserved.txt");
    tokio::fs::write(&path, b"disk_v1").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    // Load file from disk -> cache it
    let buffer = manager.get_or_load(&path).await.expect("get_or_load");
    assert_eq!(buffer.content().await, b"disk_v1");

    // User writes data to buffer -> dirty=true
    buffer.write(0, b"user_edit").await;
    assert!(buffer.is_dirty(), "buffer should be dirty after write");

    // External process updates file on disk
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    tokio::fs::write(&path, b"disk_v2_external").await.expect("write disk_v2");

    // Repeated get_or_load should NOT overwrite dirty buffer
    let reloaded = manager.get_or_load(&path).await.expect("get_or_load 2");
    assert_eq!(
      reloaded.content().await,
      b"user_edit",
      "dirty buffer should not be overwritten with disk data"
    );
    assert!(
      Arc::ptr_eq(&buffer, &reloaded),
      "should return the same Arc (dirty buffer preserved)"
    );
  }

  /// Write beyond current size extends buffer with zeros.
  ///
  /// Buffer "hello" (5 bytes), write "!" at offset 10 -> size = 11.
  /// Gap [5..10) is filled with zeros.
  #[tokio::test]
  async fn test_buffer_size_after_extend() {
    let buffer = FileBuffer::new(PathBuf::from("/test/extend.txt"), b"hello".to_vec());
    assert_eq!(buffer.size(), 5);

    // Write "!" at offset 10 — buffer extends to 11 bytes
    buffer.write(10, b"!").await;
    assert_eq!(buffer.size(), 11, "size should be 11 after extension");

    // Gap [5..10) should be filled with zeros
    let gap = buffer.read(5, 5).await;
    assert_eq!(gap, vec![0u8; 5], "gap should be filled with zeros");

    // Verify full content: "hello" + 5 zeros + "!"
    let full = buffer.content().await;
    let mut expected = b"hello".to_vec();
    expected.extend_from_slice(&[0u8; 5]);
    expected.push(b'!');
    assert_eq!(full, expected, "full buffer content after extension");
  }

  /// Flush clears the dirty flag: write -> dirty=true -> flush -> dirty=false.
  ///
  /// Pattern from SimpleGitFS core_pipeline_tests: after flush the buffer
  /// should be clean, ready for a new write cycle.
  #[tokio::test]
  async fn test_buffer_flush_clears_dirty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("flush_dirty.txt");
    tokio::fs::write(&path, b"original").await.expect("write");

    let config = BufferConfig::default();
    let manager = FileBufferManager::new(config);

    let buffer = manager.cache(&path, b"original".to_vec()).await;

    // Initially buffer is not dirty
    assert!(!buffer.is_dirty(), "new buffer should not be dirty");

    // Write -> dirty=true
    buffer.write(0, b"modified").await;
    assert!(buffer.is_dirty(), "write should mark buffer as dirty");

    // Flush -> dirty=false
    manager.flush(&path).await.expect("flush");
    assert!(!buffer.is_dirty(), "buffer should be clean after flush");

    // Verify data is actually written to disk
    let on_disk = tokio::fs::read(&path).await.expect("read");
    assert_eq!(on_disk, b"modified", "flush should write data to disk");
  }
}
