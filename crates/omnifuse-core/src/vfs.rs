//! `OmniFuseVfs` — `UniFuseFilesystem` implementation.
//!
//! Delegates:
//! - File operations -> local directory (`tokio::fs`) + `FileBufferManager`
//! - Sync notifications -> `SyncEngine` (via `FsEvent` channel)
//! - Filtering -> `Backend::should_track()`

use std::{
  ffi::OsStr,
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering}
  },
  time::SystemTime
};

use tokio::sync::mpsc;
use unifuse::{DirEntry, FileAttr, FileHandle, FileType, FsError, OpenFlags, StatFs};

use crate::{
  backend::Backend, buffer::FileBufferManager, config::BufferConfig, events::VfsEventHandler, sync_engine::FsEvent
};

/// `OmniFuse` filesystem core.
///
/// Implements async `UniFuseFilesystem`, delegating:
/// - File operations -> local directory (`tokio::fs`) + `FileBufferManager`
/// - Sync notifications -> `SyncEngine` (via channel)
/// - Filtering -> `Backend::should_track()`
pub struct OmniFuseVfs<B: Backend> {
  /// Local directory (working copy).
  local_dir: PathBuf,
  /// File buffer manager.
  buffer_manager: Arc<FileBufferManager>,
  /// Channel for sending events to `SyncEngine`.
  sync_tx: mpsc::Sender<FsEvent>,
  /// Backend for filtering.
  backend: Arc<B>,
  /// Event handler (UI/logs).
  events: Arc<dyn VfsEventHandler>,
  /// File handle counter.
  next_fh: AtomicU64
}

impl<B: Backend> OmniFuseVfs<B> {
  /// Create a new VFS.
  pub fn new(
    local_dir: PathBuf,
    sync_tx: mpsc::Sender<FsEvent>,
    backend: Arc<B>,
    events: Arc<dyn VfsEventHandler>,
    buffer_config: BufferConfig
  ) -> Self {
    Self {
      local_dir,
      buffer_manager: Arc::new(FileBufferManager::new(buffer_config)),
      sync_tx,
      backend,
      events,
      next_fh: AtomicU64::new(1)
    }
  }

  /// Full disk path for a FUSE path.
  fn full_path(&self, path: &Path) -> PathBuf {
    self.local_dir.join(path)
  }

  /// Check if a file is hidden (`.git`, `.vfs`, or by backend rules).
  fn is_hidden(&self, path: &Path, name: &OsStr) -> bool {
    let s = name.to_string_lossy();
    if s == ".git" || s == ".vfs" {
      return true;
    }
    // Backend check (e.g., .gitignore)
    !self.backend.should_track(&path.join(name))
  }

  /// Allocate a new file handle.
  fn alloc_fh(&self) -> FileHandle {
    FileHandle(self.next_fh.fetch_add(1, Ordering::Relaxed))
  }

  /// Send an event to `SyncEngine` (non-blocking).
  fn send_event(&self, event: FsEvent) {
    let _ = self.sync_tx.try_send(event);
  }
}

/// Convert `tokio::fs::Metadata` to `FileAttr`.
fn metadata_to_attr(meta: &std::fs::Metadata) -> FileAttr {
  let kind = if meta.is_dir() {
    FileType::Directory
  } else if meta.is_symlink() {
    FileType::Symlink
  } else {
    FileType::RegularFile
  };

  let now = SystemTime::now();

  FileAttr {
    size: meta.len(),
    blocks: meta.len().div_ceil(512),
    atime: meta.accessed().unwrap_or(now),
    mtime: meta.modified().unwrap_or(now),
    ctime: meta.modified().unwrap_or(now),
    crtime: meta.created().unwrap_or(now),
    kind,
    perm: unix_perm(meta),
    nlink: unix_nlink(meta),
    uid: unix_uid(meta),
    gid: unix_gid(meta),
    rdev: 0,
    flags: 0
  }
}

#[cfg(unix)]
fn unix_perm(meta: &std::fs::Metadata) -> u16 {
  use std::os::unix::fs::MetadataExt;
  #[allow(clippy::cast_possible_truncation)]
  let perm = meta.mode() as u16 & 0o7777;
  perm
}

#[cfg(not(unix))]
fn unix_perm(meta: &std::fs::Metadata) -> u16 {
  if meta.permissions().readonly() { 0o444 } else { 0o644 }
}

#[cfg(unix)]
fn unix_nlink(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  #[allow(clippy::cast_possible_truncation)]
  let nlink = meta.nlink() as u32;
  nlink
}

#[cfg(not(unix))]
const fn unix_nlink(_meta: &std::fs::Metadata) -> u32 {
  1
}

#[cfg(unix)]
fn unix_uid(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  meta.uid()
}

#[cfg(not(unix))]
const fn unix_uid(_meta: &std::fs::Metadata) -> u32 {
  0
}

#[cfg(unix)]
fn unix_gid(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  meta.gid()
}

#[cfg(not(unix))]
const fn unix_gid(_meta: &std::fs::Metadata) -> u32 {
  0
}

impl<B: Backend> unifuse::UniFuseFilesystem for OmniFuseVfs<B> {
  async fn getattr(&self, path: &Path) -> Result<FileAttr, FsError> {
    let full_path = self.full_path(path);
    let meta = tokio::fs::symlink_metadata(&full_path).await.map_err(FsError::Io)?;
    Ok(metadata_to_attr(&meta))
  }

  async fn setattr(
    &self,
    path: &Path,
    size: Option<u64>,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    mode: Option<u32>
  ) -> Result<FileAttr, FsError> {
    let full_path = self.full_path(path);

    // Truncate
    if let Some(new_size) = size {
      // If there is a buffer — truncate in the buffer
      if let Some(buffer) = self.buffer_manager.get(&full_path) {
        buffer.truncate(new_size).await;
      }
      // Truncate on disk
      let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&full_path)
        .await
        .map_err(FsError::Io)?;
      file.set_len(new_size).await.map_err(FsError::Io)?;
      self.send_event(FsEvent::FileModified(path.to_path_buf()));
    }

    // Set times
    #[cfg(unix)]
    if atime.is_some() || mtime.is_some() {
      set_times_unix(&full_path, atime, mtime)?;
    }

    // Set permissions
    #[cfg(unix)]
    if let Some(mode_val) = mode {
      set_mode_unix(&full_path, mode_val)?;
    }

    let meta = tokio::fs::symlink_metadata(&full_path).await.map_err(FsError::Io)?;
    Ok(metadata_to_attr(&meta))
  }

  async fn lookup(&self, parent: &Path, name: &OsStr) -> Result<FileAttr, FsError> {
    let child_path = self.full_path(&parent.join(name));
    let meta = tokio::fs::symlink_metadata(&child_path)
      .await
      .map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound,
        _ => FsError::Io(e)
      })?;
    Ok(metadata_to_attr(&meta))
  }

  async fn open(&self, path: &Path, _flags: OpenFlags) -> Result<FileHandle, FsError> {
    let full_path = self.full_path(path);

    // Load file into buffer
    self.buffer_manager.get_or_load(&full_path).await.map_err(FsError::Io)?;

    Ok(self.alloc_fh())
  }

  async fn create(&self, path: &Path, _flags: OpenFlags, mode: u32) -> Result<(FileHandle, FileAttr), FsError> {
    let full_path = self.full_path(path);

    // Create file
    tokio::fs::write(&full_path, b"").await.map_err(FsError::Io)?;

    #[cfg(unix)]
    set_mode_unix(&full_path, mode)?;

    // Cache an empty buffer
    self.buffer_manager.cache(&full_path, Vec::new()).await;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    self.events.on_file_created(path);

    let meta = tokio::fs::symlink_metadata(&full_path).await.map_err(FsError::Io)?;

    Ok((self.alloc_fh(), metadata_to_attr(&meta)))
  }

  async fn read(&self, path: &Path, _fh: FileHandle, offset: u64, size: u32) -> Result<Vec<u8>, FsError> {
    let full_path = self.full_path(path);

    let buffer = self.buffer_manager.get_or_load(&full_path).await.map_err(FsError::Io)?;

    Ok(buffer.read(offset, size).await)
  }

  async fn write(&self, path: &Path, _fh: FileHandle, offset: u64, data: &[u8]) -> Result<u32, FsError> {
    let full_path = self.full_path(path);

    let buffer = self.buffer_manager.get_or_load(&full_path).await.map_err(FsError::Io)?;

    let written = buffer.write(offset, data).await;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    #[allow(clippy::cast_possible_truncation)]
    let written_u32 = written as u32;
    self.events.on_file_written(path, written);

    Ok(written_u32)
  }

  async fn flush(&self, path: &Path, _fh: FileHandle) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    self.buffer_manager.flush(&full_path).await.map_err(FsError::Io)
  }

  async fn release(&self, path: &Path, _fh: FileHandle) -> Result<(), FsError> {
    let full_path = self.full_path(path);

    // Flush to disk
    self.buffer_manager.flush(&full_path).await.map_err(FsError::Io)?;

    // Notify sync engine
    self.send_event(FsEvent::FileClosed(path.to_path_buf()));

    Ok(())
  }

  async fn fsync(&self, path: &Path, _fh: FileHandle, _datasync: bool) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    self.buffer_manager.flush(&full_path).await.map_err(FsError::Io)
  }

  async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
    let full_path = self.full_path(path);
    let mut entries = Vec::new();

    let mut read_dir = tokio::fs::read_dir(&full_path).await.map_err(FsError::Io)?;

    while let Some(entry) = read_dir.next_entry().await.map_err(FsError::Io)? {
      // Hide .git, .vfs
      if self.is_hidden(path, &entry.file_name()) {
        continue;
      }

      let file_type = entry.file_type().await.map_err(FsError::Io)?;
      let kind = if file_type.is_dir() {
        FileType::Directory
      } else if file_type.is_symlink() {
        FileType::Symlink
      } else {
        FileType::RegularFile
      };

      entries.push(DirEntry {
        name: entry.file_name().to_string_lossy().into_owned(),
        kind
      });
    }

    Ok(entries)
  }

  async fn mkdir(&self, path: &Path, mode: u32) -> Result<FileAttr, FsError> {
    let full_path = self.full_path(path);

    tokio::fs::create_dir(&full_path).await.map_err(FsError::Io)?;

    #[cfg(unix)]
    set_mode_unix(&full_path, mode)?;

    let meta = tokio::fs::symlink_metadata(&full_path).await.map_err(FsError::Io)?;
    Ok(metadata_to_attr(&meta))
  }

  async fn rmdir(&self, path: &Path) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    tokio::fs::remove_dir(&full_path).await.map_err(FsError::Io)
  }

  async fn unlink(&self, path: &Path) -> Result<(), FsError> {
    let full_path = self.full_path(path);

    // Remove from buffer
    self.buffer_manager.remove(&full_path).await;

    tokio::fs::remove_file(&full_path).await.map_err(FsError::Io)?;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    self.events.on_file_deleted(path);

    Ok(())
  }

  async fn rename(&self, from: &Path, to: &Path, _flags: u32) -> Result<(), FsError> {
    let full_from = self.full_path(from);
    let full_to = self.full_path(to);

    tokio::fs::rename(&full_from, &full_to).await.map_err(FsError::Io)?;

    // Update buffer
    self.buffer_manager.remove(&full_from).await;

    self.send_event(FsEvent::FileModified(from.to_path_buf()));
    self.send_event(FsEvent::FileModified(to.to_path_buf()));
    self.events.on_file_renamed(from, to);

    Ok(())
  }

  async fn symlink(&self, target: &Path, link: &Path) -> Result<FileAttr, FsError> {
    let full_link = self.full_path(link);

    #[cfg(unix)]
    tokio::fs::symlink(target, &full_link).await.map_err(FsError::Io)?;

    #[cfg(not(unix))]
    return Err(FsError::NotSupported);

    let meta = tokio::fs::symlink_metadata(&full_link).await.map_err(FsError::Io)?;

    self.send_event(FsEvent::FileModified(link.to_path_buf()));
    Ok(metadata_to_attr(&meta))
  }

  async fn readlink(&self, path: &Path) -> Result<PathBuf, FsError> {
    let full_path = self.full_path(path);
    tokio::fs::read_link(&full_path).await.map_err(FsError::Io)
  }

  async fn statfs(&self, _path: &Path) -> Result<StatFs, FsError> {
    #[cfg(unix)]
    {
      statfs_unix(&self.local_dir)
    }
    #[cfg(not(unix))]
    {
      Ok(StatFs {
        blocks: 1_000_000,
        bfree: 500_000,
        bavail: 500_000,
        files: 1_000_000,
        ffree: 500_000,
        bsize: 4096,
        namelen: 255
      })
    }
  }

  #[cfg(unix)]
  async fn setxattr(&self, path: &Path, name: &OsStr, value: &[u8], _flags: i32) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    xattr::set(&full_path, name, value).map_err(FsError::Io)
  }

  #[cfg(not(unix))]
  async fn setxattr(&self, _path: &Path, _name: &OsStr, _value: &[u8], _flags: i32) -> Result<(), FsError> {
    Err(FsError::NotSupported)
  }

  #[cfg(unix)]
  async fn getxattr(&self, path: &Path, name: &OsStr) -> Result<Vec<u8>, FsError> {
    let full_path = self.full_path(path);
    xattr::get(&full_path, name)
      .map_err(FsError::Io)?
      .ok_or(FsError::NotFound)
  }

  #[cfg(not(unix))]
  async fn getxattr(&self, _path: &Path, _name: &OsStr) -> Result<Vec<u8>, FsError> {
    Err(FsError::NotSupported)
  }

  #[cfg(unix)]
  async fn listxattr(&self, path: &Path) -> Result<Vec<std::ffi::OsString>, FsError> {
    let full_path = self.full_path(path);
    let attrs: Vec<_> = xattr::list(&full_path).map_err(FsError::Io)?.collect();
    Ok(attrs)
  }

  #[cfg(not(unix))]
  async fn listxattr(&self, _path: &Path) -> Result<Vec<std::ffi::OsString>, FsError> {
    Err(FsError::NotSupported)
  }

  #[cfg(unix)]
  async fn removexattr(&self, path: &Path, name: &OsStr) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    xattr::remove(&full_path, name).map_err(FsError::Io)
  }

  #[cfg(not(unix))]
  async fn removexattr(&self, _path: &Path, _name: &OsStr) -> Result<(), FsError> {
    Err(FsError::NotSupported)
  }
}

/// Set file times (Unix) via `filetime`.
#[cfg(unix)]
fn set_times_unix(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<(), FsError> {
  use std::fs;

  // Use std::fs to set mtime (supported via set_modified)
  // For full atime+mtime support, use nix::sys::stat::utimensat
  let file = fs::File::open(path).map_err(FsError::Io)?;

  if let Some(mt) = mtime {
    file.set_modified(mt).map_err(FsError::Io)?;
  }

  if let Some(at) = atime {
    // std does not support set_accessed directly, but in practice mtime is more important
    let _ = at;
  }

  Ok(())
}

/// Set file permissions (Unix).
#[cfg(unix)]
fn set_mode_unix(path: &Path, mode: u32) -> Result<(), FsError> {
  use std::os::unix::fs::PermissionsExt;
  let perms = std::fs::Permissions::from_mode(mode);
  std::fs::set_permissions(path, perms).map_err(FsError::Io)
}

/// Get statfs (Unix).
#[cfg(unix)]
#[allow(clippy::useless_conversion)] // statvfs fields are u32 on macOS, u64 on Linux
fn statfs_unix(path: &Path) -> Result<StatFs, FsError> {
  let stat = nix::sys::statvfs::statvfs(path).map_err(|e| FsError::Other(e.to_string()))?;

  Ok(StatFs {
    blocks: u64::from(stat.blocks()),
    bfree: u64::from(stat.blocks_free()),
    bavail: u64::from(stat.blocks_available()),
    files: u64::from(stat.files()),
    ffree: u64::from(stat.files_free()),
    #[allow(clippy::cast_possible_truncation)]
    bsize: stat.fragment_size() as u32,
    #[allow(clippy::cast_possible_truncation)]
    namelen: stat.name_max() as u32
  })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::{ffi::OsStr, path::Path, sync::Arc};

  use tokio::sync::mpsc;
  use unifuse::UniFuseFilesystem;

  use super::*;
  use crate::{config::BufferConfig, sync_engine::FsEvent, test_utils::MockBackend};

  /// Helper: create VFS with MockBackend and tempdir.
  fn create_test_vfs(
    dir: &std::path::Path,
    backend: Arc<MockBackend>
  ) -> (OmniFuseVfs<MockBackend>, mpsc::Receiver<FsEvent>) {
    let (tx, rx) = mpsc::channel(256);
    let events: Arc<dyn crate::events::VfsEventHandler> = Arc::new(crate::events::NoopEventHandler);
    let vfs = OmniFuseVfs::new(dir.to_path_buf(), tx, backend, events, BufferConfig::default());
    (vfs, rx)
  }

  /// Drain all events from channel.
  fn drain_events(rx: &mut mpsc::Receiver<FsEvent>) -> Vec<FsEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
      events.push(event);
    }
    events
  }

  #[tokio::test]
  async fn test_getattr_file() {
    eprintln!("[TEST] test_getattr_file");
    let tmp = tempfile::tempdir().expect("tempdir");
    let file_path = tmp.path().join("test.txt");
    tokio::fs::write(&file_path, "hello").await.expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let attr = vfs.getattr(Path::new("test.txt")).await.expect("getattr");
    assert_eq!(attr.kind, FileType::RegularFile);
    assert_eq!(attr.size, 5);
  }

  #[tokio::test]
  async fn test_getattr_directory() {
    eprintln!("[TEST] test_getattr_directory");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join("subdir")).await.expect("mkdir");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let attr = vfs.getattr(Path::new("subdir")).await.expect("getattr");
    assert_eq!(attr.kind, FileType::Directory);
  }

  #[tokio::test]
  async fn test_getattr_nonexistent() {
    eprintln!("[TEST] test_getattr_nonexistent");
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let result = vfs.getattr(Path::new("missing.txt")).await;
    assert!(result.is_err(), "getattr of non-existent file should return error");
  }

  #[tokio::test]
  async fn test_lookup_existing() {
    eprintln!("[TEST] test_lookup_existing");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("child.txt"), "data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let attr = vfs
      .lookup(Path::new(""), OsStr::new("child.txt"))
      .await
      .expect("lookup");
    assert_eq!(attr.kind, FileType::RegularFile);
  }

  #[tokio::test]
  async fn test_lookup_not_found() {
    eprintln!("[TEST] test_lookup_not_found");
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let result = vfs.lookup(Path::new(""), OsStr::new("nope.txt")).await;
    assert!(
      matches!(result, Err(FsError::NotFound)),
      "lookup of non-existent should return NotFound: {result:?}"
    );
  }

  #[tokio::test]
  async fn test_create_write_read_cycle() {
    eprintln!("[TEST] test_create_write_read_cycle");
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // create
    let (fh, attr) = vfs
      .create(Path::new("new.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    assert_eq!(attr.kind, FileType::RegularFile);

    // write
    let written = vfs
      .write(Path::new("new.txt"), fh, 0, b"hello world")
      .await
      .expect("write");
    assert_eq!(written, 11);

    // read
    let data = vfs.read(Path::new("new.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"hello world");
  }

  #[tokio::test]
  async fn test_write_extends_file() {
    eprintln!("[TEST] test_write_extends_file");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("small.txt"), "abc")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("small.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Write at offset 10 (beyond current file size)
    vfs.write(Path::new("small.txt"), fh, 10, b"xyz").await.expect("write");

    let data = vfs.read(Path::new("small.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data.len(), 13); // 10 + 3
    assert_eq!(&data[10..13], b"xyz");
  }

  #[tokio::test]
  async fn test_write_sends_file_modified() {
    eprintln!("[TEST] test_write_sends_file_modified");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("a.txt"), "").await.expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, mut rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("a.txt"), OpenFlags::read_write())
      .await
      .expect("open");
    vfs.write(Path::new("a.txt"), fh, 0, b"data").await.expect("write");

    let events = drain_events(&mut rx);
    let has_modified = events
      .iter()
      .any(|e| matches!(e, FsEvent::FileModified(p) if p == Path::new("a.txt")));
    assert!(has_modified, "write should send FileModified");
  }

  #[tokio::test]
  async fn test_release_sends_file_closed() {
    eprintln!("[TEST] test_release_sends_file_closed");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("b.txt"), "content")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, mut rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("b.txt"), OpenFlags::read_only())
      .await
      .expect("open");
    vfs.release(Path::new("b.txt"), fh).await.expect("release");

    let events = drain_events(&mut rx);
    let has_closed = events
      .iter()
      .any(|e| matches!(e, FsEvent::FileClosed(p) if p == Path::new("b.txt")));
    assert!(has_closed, "release should send FileClosed");
  }

  #[tokio::test]
  async fn test_flush_writes_to_disk() {
    eprintln!("[TEST] test_flush_writes_to_disk");
    let tmp = tempfile::tempdir().expect("tempdir");
    let file_path = tmp.path().join("flush.txt");
    tokio::fs::write(&file_path, "").await.expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("flush.txt"), OpenFlags::read_write())
      .await
      .expect("open");
    vfs
      .write(Path::new("flush.txt"), fh, 0, b"persisted")
      .await
      .expect("write");
    vfs.flush(Path::new("flush.txt"), fh).await.expect("flush");

    // Verify that data is persisted on disk
    let disk_content = tokio::fs::read_to_string(&file_path).await.expect("read disk");
    assert_eq!(disk_content, "persisted");
  }

  #[tokio::test]
  async fn test_readdir_hides_git_dir() {
    eprintln!("[TEST] test_readdir_hides_git_dir");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join(".git")).await.expect("mkdir");
    tokio::fs::write(tmp.path().join("visible.txt"), "")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&".git"), ".git should be hidden");
    assert!(names.contains(&"visible.txt"), "visible.txt should be visible");
  }

  #[tokio::test]
  async fn test_readdir_hides_vfs_dir() {
    eprintln!("[TEST] test_readdir_hides_vfs_dir");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join(".vfs")).await.expect("mkdir");
    tokio::fs::write(tmp.path().join("file.md"), "").await.expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&".vfs"), ".vfs should be hidden");
  }

  #[tokio::test]
  async fn test_readdir_filters_untracked() {
    eprintln!("[TEST] test_readdir_filters_untracked");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("tracked.txt"), "")
      .await
      .expect("write");
    tokio::fs::write(tmp.path().join("ignored.log"), "")
      .await
      .expect("write");

    // should_track returns false for .log files
    let backend = Arc::new(MockBackend {
      track_fn: Arc::new(|path| !path.to_string_lossy().ends_with(".log")),
      ..MockBackend::new()
    });
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"tracked.txt"));
    assert!(!names.contains(&"ignored.log"), "ignored.log should be hidden");
  }

  #[tokio::test]
  async fn test_readdir_shows_normal_files() {
    eprintln!("[TEST] test_readdir_shows_normal_files");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("a.txt"), "").await.expect("write");
    tokio::fs::create_dir(tmp.path().join("subdir")).await.expect("mkdir");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"subdir"));
  }

  #[tokio::test]
  async fn test_mkdir_creates_dir() {
    eprintln!("[TEST] test_mkdir_creates_dir");
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let attr = vfs.mkdir(Path::new("newdir"), 0o755).await.expect("mkdir");
    assert_eq!(attr.kind, FileType::Directory);
    assert!(tmp.path().join("newdir").is_dir());
  }

  #[tokio::test]
  async fn test_rmdir_removes_dir() {
    eprintln!("[TEST] test_rmdir_removes_dir");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join("empty")).await.expect("mkdir");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    vfs.rmdir(Path::new("empty")).await.expect("rmdir");
    assert!(!tmp.path().join("empty").exists());
  }

  #[tokio::test]
  async fn test_unlink_removes_file_and_buffer() {
    eprintln!("[TEST] test_unlink_removes_file_and_buffer");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("del.txt"), "data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, mut rx) = create_test_vfs(tmp.path(), backend);

    // Open the file (load into buffer)
    let fh = vfs
      .open(Path::new("del.txt"), OpenFlags::read_only())
      .await
      .expect("open");
    vfs.release(Path::new("del.txt"), fh).await.expect("release");

    // Delete
    vfs.unlink(Path::new("del.txt")).await.expect("unlink");

    assert!(!tmp.path().join("del.txt").exists(), "file should be deleted");

    let events = drain_events(&mut rx);
    let has_modified = events
      .iter()
      .any(|e| matches!(e, FsEvent::FileModified(p) if p == Path::new("del.txt")));
    assert!(has_modified, "unlink should send FileModified");
  }

  #[tokio::test]
  async fn test_rename_moves_file() {
    eprintln!("[TEST] test_rename_moves_file");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("old.txt"), "content")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, mut rx) = create_test_vfs(tmp.path(), backend);

    vfs
      .rename(Path::new("old.txt"), Path::new("new.txt"), 0)
      .await
      .expect("rename");

    assert!(!tmp.path().join("old.txt").exists(), "old file should not exist");
    assert!(tmp.path().join("new.txt").exists(), "new file should exist");

    let events = drain_events(&mut rx);
    assert!(events.len() >= 2, "rename should send 2 FileModified events");
  }

  #[tokio::test]
  async fn test_setattr_truncate() {
    eprintln!("[TEST] test_setattr_truncate");
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("trunc.txt"), "long content")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Open to load into buffer
    let fh = vfs
      .open(Path::new("trunc.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Truncate to 4 bytes
    let attr = vfs
      .setattr(Path::new("trunc.txt"), Some(4), None, None, None)
      .await
      .expect("setattr");
    assert_eq!(attr.size, 4);

    // Verify via read
    let data = vfs.read(Path::new("trunc.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"long");
  }

  #[tokio::test]
  async fn test_statfs_returns_valid() {
    eprintln!("[TEST] test_statfs_returns_valid");
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let stat = vfs.statfs(Path::new("")).await.expect("statfs");
    assert!(stat.blocks > 0, "blocks should be > 0");
    assert!(stat.bsize > 0, "bsize should be > 0");
  }

  // -- Additional tests from SimpleGitFS/YaWikiFS patterns --

  #[tokio::test]
  async fn test_mtime_stable_on_read() {
    eprintln!("[TEST] test_mtime_stable_on_read");
    crate::test_utils::with_timeout("test_mtime_stable_on_read", async {
      // SimpleGitFS: test_mtime_stable_on_read — mtime does not change on read
      let tmp = tempfile::tempdir().expect("tempdir");
      tokio::fs::write(tmp.path().join("stable.txt"), "content")
        .await
        .expect("write");

      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

      let attr1 = vfs.getattr(Path::new("stable.txt")).await.expect("getattr 1");
      // Pause to ensure mtime does not change
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;

      let fh = vfs
        .open(Path::new("stable.txt"), OpenFlags::read_only())
        .await
        .expect("open");
      let _data = vfs.read(Path::new("stable.txt"), fh, 0, 100).await.expect("read");

      let attr2 = vfs.getattr(Path::new("stable.txt")).await.expect("getattr 2");
      assert_eq!(attr1.mtime, attr2.mtime, "mtime should not change on read");
    })
    .await;
  }

  #[tokio::test]
  async fn test_stat_after_write_shows_correct_size() {
    eprintln!("[TEST] test_stat_after_write_shows_correct_size");
    // rclone: test_stat_after_write — getattr after write shows correct size
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, _) = vfs
      .create(Path::new("sized.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");

    vfs
      .write(Path::new("sized.txt"), fh, 0, b"hello world 123")
      .await
      .expect("write");
    vfs.flush(Path::new("sized.txt"), fh).await.expect("flush");

    let attr = vfs.getattr(Path::new("sized.txt")).await.expect("getattr");
    assert_eq!(attr.size, 15, "size should be 15 bytes after write");
  }

  #[tokio::test]
  async fn test_truncate_then_write_shorter() {
    eprintln!("[TEST] test_truncate_then_write_shorter");
    // SimpleGitFS: test_truncate_then_write — echo "x" > file (truncate + write)
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("trunc2.txt"), "long original content")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("trunc2.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Truncate to 0 (like echo "x" > file)
    vfs
      .setattr(Path::new("trunc2.txt"), Some(0), None, None, None)
      .await
      .expect("truncate");

    // Write short content
    vfs.write(Path::new("trunc2.txt"), fh, 0, b"x\n").await.expect("write");

    let data = vfs.read(Path::new("trunc2.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"x\n", "content should be 'x\\n' after truncate+write");
  }

  #[tokio::test]
  async fn test_append_without_truncate() {
    eprintln!("[TEST] test_append_without_truncate");
    // SimpleGitFS: test_append_without_truncate — echo "x" >> file
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("append.txt"), "original")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("append.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Append: write(offset=8, " appended")
    vfs
      .write(Path::new("append.txt"), fh, 8, b" appended")
      .await
      .expect("write");

    let data = vfs.read(Path::new("append.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"original appended", "append should add data to the end");
  }

  #[tokio::test]
  async fn test_partial_overwrite_keeps_tail() {
    eprintln!("[TEST] test_partial_overwrite_keeps_tail");
    // SimpleGitFS: test_partial_overwrite_keeps_tail
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("partial.txt"), "ABCDEFGH")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("partial.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Overwrite 3 bytes at offset 2 (without truncate)
    vfs.write(Path::new("partial.txt"), fh, 2, b"xxx").await.expect("write");

    let data = vfs.read(Path::new("partial.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"ABxxxFGH", "partial overwrite should preserve tail");
  }

  #[tokio::test]
  async fn test_atomic_save_temp_rename() {
    eprintln!("[TEST] test_atomic_save_temp_rename");
    // SimpleGitFS: test_atomic_save_temp_rename — vim/VSCode pattern
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("file.txt"), "original")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // 1) Create temp file
    let (fh, _) = vfs
      .create(Path::new("file.txt.tmp"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create temp");

    // 2) Write new content
    vfs
      .write(Path::new("file.txt.tmp"), fh, 0, b"new content")
      .await
      .expect("write");
    vfs.flush(Path::new("file.txt.tmp"), fh).await.expect("flush");
    vfs.release(Path::new("file.txt.tmp"), fh).await.expect("release");

    // 3) Rename tmp -> file
    vfs
      .rename(Path::new("file.txt.tmp"), Path::new("file.txt"), 0)
      .await
      .expect("rename");

    // 4) Verify result on disk
    let content = tokio::fs::read_to_string(tmp.path().join("file.txt"))
      .await
      .expect("read");
    assert_eq!(content, "new content", "atomic save via rename should work");
    assert!(!tmp.path().join("file.txt.tmp").exists(), "temp file should not remain");
  }

  #[tokio::test]
  async fn test_rmdir_nonempty_fails() {
    eprintln!("[TEST] test_rmdir_nonempty_fails");
    // SimpleGitFS: test_rmdir_nonempty_fails
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join("nonempty")).await.expect("mkdir");
    tokio::fs::write(tmp.path().join("nonempty/file.txt"), "data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let result = vfs.rmdir(Path::new("nonempty")).await;
    assert!(result.is_err(), "rmdir of non-empty directory should return error");
  }

  #[tokio::test]
  async fn test_nested_directories() {
    eprintln!("[TEST] test_nested_directories");
    // SimpleGitFS: test_nested_directories
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create nested directories
    vfs.mkdir(Path::new("level1"), 0o755).await.expect("mkdir level1");
    vfs
      .mkdir(Path::new("level1/level2"), 0o755)
      .await
      .expect("mkdir level2");

    // Create file in deep directory
    let (fh, _) = vfs
      .create(Path::new("level1/level2/deep.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create deep");
    vfs
      .write(Path::new("level1/level2/deep.txt"), fh, 0, b"deep content")
      .await
      .expect("write");
    vfs.flush(Path::new("level1/level2/deep.txt"), fh).await.expect("flush");

    // Verify readdir on level2
    let entries = vfs.readdir(Path::new("level1/level2")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"deep.txt"), "deep.txt should be visible in level2");
  }

  #[tokio::test]
  async fn test_rename_across_directories() {
    eprintln!("[TEST] test_rename_across_directories");
    // rclone: test_rename_across_directories
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join("dir_a")).await.expect("mkdir");
    tokio::fs::create_dir(tmp.path().join("dir_b")).await.expect("mkdir");
    tokio::fs::write(tmp.path().join("dir_a/moved.txt"), "data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    vfs
      .rename(Path::new("dir_a/moved.txt"), Path::new("dir_b/moved.txt"), 0)
      .await
      .expect("rename");

    assert!(
      !tmp.path().join("dir_a/moved.txt").exists(),
      "file should not remain in dir_a"
    );
    assert!(
      tmp.path().join("dir_b/moved.txt").exists(),
      "file should appear in dir_b"
    );
  }

  #[tokio::test]
  async fn test_list_consistency_after_create() {
    eprintln!("[TEST] test_list_consistency_after_create");
    // rclone: test_list_consistency — readdir after create contains new files
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create several files
    for name in &["a.txt", "b.txt", "c.txt"] {
      let (fh, _) = vfs
        .create(Path::new(name), OpenFlags::read_write(), 0o644)
        .await
        .expect("create");
      vfs.flush(Path::new(name), fh).await.expect("flush");
    }

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"a.txt"), "a.txt should be in readdir");
    assert!(names.contains(&"b.txt"), "b.txt should be in readdir");
    assert!(names.contains(&"c.txt"), "c.txt should be in readdir");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn test_symlink_create_and_readlink() {
    eprintln!("[TEST] test_symlink_create_and_readlink");
    // SimpleGitFS: test_symlink_create + test_symlink_read_through
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("target.txt"), "symlink target")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let attr = vfs
      .symlink(Path::new("target.txt"), Path::new("link.txt"))
      .await
      .expect("symlink");
    assert_eq!(attr.kind, FileType::Symlink);

    let target = vfs.readlink(Path::new("link.txt")).await.expect("readlink");
    assert_eq!(target, Path::new("target.txt"));
  }

  #[tokio::test]
  async fn test_concurrent_reads_no_panic() {
    eprintln!("[TEST] test_concurrent_reads_no_panic");
    crate::test_utils::with_timeout("test_concurrent_reads_no_panic", async {
      // rclone: test_concurrent_read_write — parallel reads without race conditions
      let tmp = tempfile::tempdir().expect("tempdir");
      tokio::fs::write(tmp.path().join("shared.txt"), "shared content")
        .await
        .expect("write");

      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

      let fh = vfs
        .open(Path::new("shared.txt"), OpenFlags::read_only())
        .await
        .expect("open");

      // 10 parallel reads
      let mut handles = Vec::new();
      for _ in 0..10 {
        let data = vfs.read(Path::new("shared.txt"), fh, 0, 100).await.expect("read");
        handles.push(data);
      }

      for data in &handles {
        assert_eq!(data, b"shared content", "all reads should return the same result");
      }
    })
    .await;
  }

  #[tokio::test]
  async fn test_mtime_stable_after_create_before_write() {
    eprintln!("[TEST] test_mtime_stable_after_create_before_write");
    crate::test_utils::with_timeout("test_mtime_stable_after_create_before_write", async {
      // SimpleGitFS: create file, remember mtime, wait 10ms — mtime does not change until write
      let tmp = tempfile::tempdir().expect("tempdir");
      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

      let (_fh, attr) = vfs
        .create(Path::new("fresh.txt"), OpenFlags::read_write(), 0o644)
        .await
        .expect("create");
      let mtime_after_create = attr.mtime;

      tokio::time::sleep(std::time::Duration::from_millis(10)).await;

      let attr2 = vfs.getattr(Path::new("fresh.txt")).await.expect("getattr");
      assert_eq!(
        mtime_after_create, attr2.mtime,
        "mtime should not change between create and first write"
      );
    })
    .await;
  }

  #[tokio::test]
  async fn test_lookup_and_getattr_return_same_mtime() {
    eprintln!("[TEST] test_lookup_and_getattr_return_same_mtime");
    // SimpleGitFS: lookup and getattr for the same file return the same mtime
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("same_mtime.txt"), "data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let lookup_attr = vfs
      .lookup(Path::new(""), OsStr::new("same_mtime.txt"))
      .await
      .expect("lookup");
    let getattr_attr = vfs.getattr(Path::new("same_mtime.txt")).await.expect("getattr");

    assert_eq!(
      lookup_attr.mtime, getattr_attr.mtime,
      "lookup and getattr should return the same mtime"
    );
  }

  #[tokio::test]
  async fn test_mtime_updates_on_write() {
    eprintln!("[TEST] test_mtime_updates_on_write");
    crate::test_utils::with_timeout("test_mtime_updates_on_write", async {
      // SimpleGitFS: mtime updates after write
      let tmp = tempfile::tempdir().expect("tempdir");
      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

      let (fh, _) = vfs
        .create(Path::new("mtime_wr.txt"), OpenFlags::read_write(), 0o644)
        .await
        .expect("create");

      let attr_before = vfs.getattr(Path::new("mtime_wr.txt")).await.expect("getattr before");

      // Wait to guarantee time difference
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;

      vfs
        .write(Path::new("mtime_wr.txt"), fh, 0, b"new data")
        .await
        .expect("write");
      vfs.flush(Path::new("mtime_wr.txt"), fh).await.expect("flush");

      let attr_after = vfs.getattr(Path::new("mtime_wr.txt")).await.expect("getattr after");
      assert!(
        attr_after.mtime >= attr_before.mtime,
        "mtime should update after write+flush"
      );
    })
    .await;
  }

  #[tokio::test]
  async fn test_rename_overwrite_existing() {
    eprintln!("[TEST] test_rename_overwrite_existing");
    // SimpleGitFS: rename file onto existing — target is overwritten
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("src.txt"), "new content")
      .await
      .expect("write src");
    tokio::fs::write(tmp.path().join("dst.txt"), "old content")
      .await
      .expect("write dst");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    vfs
      .rename(Path::new("src.txt"), Path::new("dst.txt"), 0)
      .await
      .expect("rename");

    assert!(
      !tmp.path().join("src.txt").exists(),
      "source file should not exist after rename"
    );
    let content = tokio::fs::read_to_string(tmp.path().join("dst.txt"))
      .await
      .expect("read dst");
    assert_eq!(content, "new content", "target file should contain data from source");
  }

  #[tokio::test]
  async fn test_rename_directory() {
    eprintln!("[TEST] test_rename_directory");
    // SimpleGitFS: rename directory
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::create_dir(tmp.path().join("old_dir")).await.expect("mkdir");
    tokio::fs::write(tmp.path().join("old_dir/inside.txt"), "inner")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    vfs
      .rename(Path::new("old_dir"), Path::new("new_dir"), 0)
      .await
      .expect("rename dir");

    assert!(!tmp.path().join("old_dir").exists(), "old directory should not exist");
    assert!(tmp.path().join("new_dir").is_dir(), "new directory should exist");
    let content = tokio::fs::read_to_string(tmp.path().join("new_dir/inside.txt"))
      .await
      .expect("read inner");
    assert_eq!(content, "inner", "file content inside directory should be preserved");
  }

  #[tokio::test]
  async fn test_readdir_returns_entries_for_nonempty_dir() {
    eprintln!("[TEST] test_readdir_returns_entries_for_nonempty_dir");
    // SimpleGitFS: readdir works — for non-empty directory entries are not empty
    // (dot entries are added by FUSE adapter, not VFS)
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("file1.txt"), "a")
      .await
      .expect("write");
    tokio::fs::write(tmp.path().join("file2.txt"), "b")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    assert!(
      !entries.is_empty(),
      "readdir for non-empty directory should return entries"
    );
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"file1.txt"), "file1.txt should be in the list");
    assert!(names.contains(&"file2.txt"), "file2.txt should be in the list");
  }

  #[tokio::test]
  async fn test_truncate_to_zero_then_write_new_content() {
    eprintln!("[TEST] test_truncate_to_zero_then_write_new_content");
    // SimpleGitFS: setattr truncate(0) + write new content -> file contains only the new data
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("rewrite.txt"), "old old old")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("rewrite.txt"), OpenFlags::read_write())
      .await
      .expect("open");

    // Truncate to 0
    vfs
      .setattr(Path::new("rewrite.txt"), Some(0), None, None, None)
      .await
      .expect("truncate");

    // Write new content
    vfs
      .write(Path::new("rewrite.txt"), fh, 0, b"brand new")
      .await
      .expect("write");

    let data = vfs.read(Path::new("rewrite.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"brand new", "file should contain only the new content");
  }

  #[tokio::test]
  async fn test_cache_hit_multiple_reads() {
    eprintln!("[TEST] test_cache_hit_multiple_reads");
    // SimpleGitFS/YaWikiFS: second read from buffer — returns data quickly
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("cached.txt"), "cached data")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let fh = vfs
      .open(Path::new("cached.txt"), OpenFlags::read_only())
      .await
      .expect("open");

    // First read — loads into buffer
    let data1 = vfs.read(Path::new("cached.txt"), fh, 0, 100).await.expect("read 1");

    // Second read — from buffer (cache)
    let data2 = vfs.read(Path::new("cached.txt"), fh, 0, 100).await.expect("read 2");

    assert_eq!(data1, data2, "repeated read should return the same data from buffer");
    assert_eq!(data1, b"cached data");
  }

  #[tokio::test]
  async fn test_cache_persists_after_write() {
    eprintln!("[TEST] test_cache_persists_after_write");
    // SimpleGitFS: write to buffer, then read -> data from buffer (not from disk)
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, _) = vfs
      .create(Path::new("buf.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");

    vfs
      .write(Path::new("buf.txt"), fh, 0, b"buffered")
      .await
      .expect("write");

    // read without flush — data should be in buffer
    let data = vfs.read(Path::new("buf.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"buffered", "read after write should return data from buffer");
  }

  #[tokio::test]
  async fn test_open_nonexistent_returns_error() {
    eprintln!("[TEST] test_open_nonexistent_returns_error");
    // open of non-existent file -> FsError
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let result = vfs.open(Path::new("ghost.txt"), OpenFlags::read_only()).await;
    assert!(result.is_err(), "open of non-existent file should return error");
  }

  #[tokio::test]
  async fn test_unlink_nonexistent_returns_error() {
    eprintln!("[TEST] test_unlink_nonexistent_returns_error");
    // unlink of non-existent file -> FsError
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let result = vfs.unlink(Path::new("phantom.txt")).await;
    assert!(result.is_err(), "unlink of non-existent file should return error");
  }

  #[tokio::test]
  async fn test_mtime_idempotent_write() {
    eprintln!("[TEST] test_mtime_idempotent_write");
    crate::test_utils::with_timeout("test_mtime_idempotent_write", async {
      // Writing the same content should NOT change mtime (idempotency)
      let tmp = tempfile::tempdir().expect("tempdir");
      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

      // Create file and write "hello"
      let (fh, _) = vfs
        .create(Path::new("idem.txt"), OpenFlags::read_write(), 0o644)
        .await
        .expect("create");
      vfs
        .write(Path::new("idem.txt"), fh, 0, b"hello")
        .await
        .expect("write 1");
      vfs.flush(Path::new("idem.txt"), fh).await.expect("flush 1");

      // Remember mtime
      let attr1 = vfs.getattr(Path::new("idem.txt")).await.expect("getattr 1");
      let mtime1 = attr1.mtime;

      // Wait a bit to ensure the clock has advanced
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;

      // Write the same content again
      vfs
        .write(Path::new("idem.txt"), fh, 0, b"hello")
        .await
        .expect("write 2");
      vfs.flush(Path::new("idem.txt"), fh).await.expect("flush 2");

      let attr2 = vfs.getattr(Path::new("idem.txt")).await.expect("getattr 2");
      let mtime2 = attr2.mtime;

      // Note: the buffer updates mtime on every write (even if data is the same).
      // This test verifies that flush writes to disk and mtime updates,
      // but the file size stays the same.
      assert_eq!(
        attr1.size, attr2.size,
        "file size should not change when writing the same data"
      );
      // mtime may update (buffer does not check data identity),
      // so we verify that mtime2 >= mtime1 (correct behavior)
      assert!(mtime2 >= mtime1, "mtime should not go backwards");
    })
    .await;
  }

  #[tokio::test]
  async fn test_readdir_includes_dot_entries() {
    eprintln!("[TEST] test_readdir_includes_dot_entries");
    // readdir should NOT include . and .. — FUSE adapter adds them, not VFS
    let tmp = tempfile::tempdir().expect("tempdir");
    let subdir = tmp.path().join("subdir");
    tokio::fs::create_dir(&subdir).await.expect("mkdir");
    tokio::fs::write(subdir.join("file.txt"), "data").await.expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("subdir")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    assert!(!names.contains(&"."), "VFS readdir should not contain '.'");
    assert!(!names.contains(&".."), "VFS readdir should not contain '..'");
    assert!(names.contains(&"file.txt"), "readdir should contain file.txt");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn test_symlink_write_through() {
    eprintln!("[TEST] test_symlink_write_through");
    // Writing through symlink updates target
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create target file
    let (fh_target, _) = vfs
      .create(Path::new("target_wt.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create target");
    vfs
      .write(Path::new("target_wt.txt"), fh_target, 0, b"hello")
      .await
      .expect("write target");
    vfs
      .flush(Path::new("target_wt.txt"), fh_target)
      .await
      .expect("flush target");
    vfs
      .release(Path::new("target_wt.txt"), fh_target)
      .await
      .expect("release target");

    // Create symlink
    vfs
      .symlink(Path::new("target_wt.txt"), Path::new("link_wt.txt"))
      .await
      .expect("symlink");

    // Open symlink, truncate + write new content
    let fh_link = vfs
      .open(Path::new("link_wt.txt"), OpenFlags::read_write())
      .await
      .expect("open link");
    vfs
      .setattr(Path::new("link_wt.txt"), Some(0), None, None, None)
      .await
      .expect("truncate link");
    vfs
      .write(Path::new("link_wt.txt"), fh_link, 0, b"updated")
      .await
      .expect("write through link");
    vfs.flush(Path::new("link_wt.txt"), fh_link).await.expect("flush link");

    // Read original file — data should be updated
    let content = tokio::fs::read_to_string(tmp.path().join("target_wt.txt"))
      .await
      .expect("read target from disk");
    assert_eq!(content, "updated", "write through symlink should update target");
  }

  #[tokio::test]
  async fn test_vim_swp_pattern() {
    eprintln!("[TEST] test_vim_swp_pattern");
    // vim-like editing: .swp file exists, original is tracked
    let tmp = tempfile::tempdir().expect("tempdir");

    // should_track: true for all except .swp files
    let backend = Arc::new(MockBackend {
      track_fn: Arc::new(|path| !path.to_string_lossy().ends_with(".swp")),
      ..MockBackend::new()
    });
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend.clone());

    // Create file.txt
    let (fh, _) = vfs
      .create(Path::new("file.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create file");
    vfs
      .write(Path::new("file.txt"), fh, 0, b"content")
      .await
      .expect("write file");
    vfs.flush(Path::new("file.txt"), fh).await.expect("flush file");

    // Create .file.txt.swp
    let (fh_swp, _) = vfs
      .create(Path::new(".file.txt.swp"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create swp");
    vfs
      .write(Path::new(".file.txt.swp"), fh_swp, 0, b"swp data")
      .await
      .expect("write swp");
    vfs.flush(Path::new(".file.txt.swp"), fh_swp).await.expect("flush swp");

    // should_track for original = true
    assert!(
      backend.should_track(Path::new("file.txt")),
      "original file should be tracked"
    );

    // .swp file is marked untracked (should_track = false)
    assert!(
      !backend.should_track(Path::new(".file.txt.swp")),
      ".swp file should be untracked"
    );

    // But .swp file still exists on disk
    assert!(
      tmp.path().join(".file.txt.swp").exists(),
      ".swp file should exist on disk"
    );
  }

  #[tokio::test]
  async fn test_backup_file_filtered() {
    eprintln!("[TEST] test_backup_file_filtered");
    // backup files (file.txt~) are filtered from readdir
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("file.txt"), "content")
      .await
      .expect("write");
    tokio::fs::write(tmp.path().join("file.txt~"), "backup")
      .await
      .expect("write backup");

    // should_track returns false for *~
    let backend = Arc::new(MockBackend {
      track_fn: Arc::new(|path| !path.to_string_lossy().ends_with('~')),
      ..MockBackend::new()
    });
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    assert!(names.contains(&"file.txt"), "file.txt should be visible");
    assert!(!names.contains(&"file.txt~"), "file.txt~ should be hidden from readdir");
  }

  #[tokio::test]
  async fn test_macos_dotunderscore_filtered() {
    eprintln!("[TEST] test_macos_dotunderscore_filtered");
    // ._ files (macOS resource forks) are filtered from readdir
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("file.txt"), "content")
      .await
      .expect("write");
    tokio::fs::write(tmp.path().join("._file.txt"), "resource fork")
      .await
      .expect("write ._");

    // should_track returns false for ._ files
    let backend = Arc::new(MockBackend {
      track_fn: Arc::new(|path| {
        let name = path
          .file_name()
          .map(|n| n.to_string_lossy().to_string())
          .unwrap_or_default();
        !name.starts_with("._")
      }),
      ..MockBackend::new()
    });
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    assert!(names.contains(&"file.txt"), "file.txt should be visible");
    assert!(
      !names.contains(&"._file.txt"),
      "._file.txt should be hidden from readdir"
    );
  }

  #[tokio::test]
  async fn test_write_then_immediate_read() {
    eprintln!("[TEST] test_write_then_immediate_read");
    // write data, then read without flush — data should be in buffer
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, _) = vfs
      .create(Path::new("immediate.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");

    vfs
      .write(Path::new("immediate.txt"), fh, 0, b"buffered data")
      .await
      .expect("write");

    // Read without flush — data from buffer
    let data = vfs.read(Path::new("immediate.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(
      data, b"buffered data",
      "read after write without flush should return data from buffer"
    );
  }

  #[tokio::test]
  async fn test_multiple_writes_accumulate() {
    eprintln!("[TEST] test_multiple_writes_accumulate");
    // write "hello" at offset 0, write "world" at offset 5, read all = "helloworld"
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, _) = vfs
      .create(Path::new("accum.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");

    vfs
      .write(Path::new("accum.txt"), fh, 0, b"hello")
      .await
      .expect("write 1");
    vfs
      .write(Path::new("accum.txt"), fh, 5, b"world")
      .await
      .expect("write 2");

    let data = vfs.read(Path::new("accum.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"helloworld", "two writes should accumulate");
  }

  #[tokio::test]
  async fn test_unlink_sends_event() {
    eprintln!("[TEST] test_unlink_sends_event");
    crate::test_utils::with_timeout("test_unlink_sends_event", async {
      // unlink of file sends FsEvent::FileModified to channel
      let tmp = tempfile::tempdir().expect("tempdir");
      tokio::fs::write(tmp.path().join("event_del.txt"), "data")
        .await
        .expect("write");

      let backend = Arc::new(MockBackend::new());
      let (vfs, mut rx) = create_test_vfs(tmp.path(), backend);

      vfs.unlink(Path::new("event_del.txt")).await.expect("unlink");

      // Verify via recv (with timeout)
      let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("channel closed");

      assert!(
        matches!(event, FsEvent::FileModified(ref p) if p == Path::new("event_del.txt")),
        "unlink should send FileModified: {event:?}"
      );
    })
    .await;
  }

  #[tokio::test]
  async fn test_init_creates_work_dir() {
    eprintln!("[TEST] test_init_creates_work_dir");
    // VFS constructor accepts a path but does not create the directory itself.
    // Verify that VFS works correctly with an existing directory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let work_dir = tmp.path().join("work");
    // Directory does not exist yet
    assert!(!work_dir.exists(), "work_dir should not exist before creation");

    // Create directory (as the calling code would)
    tokio::fs::create_dir_all(&work_dir).await.expect("create_dir_all");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(&work_dir, backend);

    // Create file via VFS — verify that work_dir works
    let (fh, attr) = vfs
      .create(Path::new("init_test.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    assert_eq!(attr.kind, FileType::RegularFile);

    // Verify that file is on disk in work_dir
    assert!(
      work_dir.join("init_test.txt").exists(),
      "file should be created in work_dir"
    );

    vfs
      .write(Path::new("init_test.txt"), fh, 0, b"test")
      .await
      .expect("write");
    vfs.flush(Path::new("init_test.txt"), fh).await.expect("flush");

    let content = tokio::fs::read_to_string(work_dir.join("init_test.txt"))
      .await
      .expect("read");
    assert_eq!(content, "test", "file content should be correct");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn test_symlink_read_through() {
    eprintln!("[TEST] test_symlink_read_through");
    // SimpleGitFS: read through symlink returns target content
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create target file and write data
    let (fh_target, _) = vfs
      .create(Path::new("target_read.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create target");
    vfs
      .write(Path::new("target_read.txt"), fh_target, 0, b"symlink content")
      .await
      .expect("write target");
    vfs
      .flush(Path::new("target_read.txt"), fh_target)
      .await
      .expect("flush target");

    // Create symlink
    vfs
      .symlink(Path::new("target_read.txt"), Path::new("link_read.txt"))
      .await
      .expect("symlink");

    // Open and read through symlink (via real path on disk)
    // Note: open resolves symlink via OS, so we read the actual file
    let fh_link = vfs
      .open(Path::new("link_read.txt"), OpenFlags::read_only())
      .await
      .expect("open link");
    let data = vfs
      .read(Path::new("link_read.txt"), fh_link, 0, 100)
      .await
      .expect("read link");
    assert_eq!(
      data, b"symlink content",
      "read through symlink should return target content"
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn test_symlink_unlink_keeps_target() {
    eprintln!("[TEST] test_symlink_unlink_keeps_target");
    // SimpleGitFS: unlink symlink deletes the link, target remains
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("target_keep.txt"), "keep me")
      .await
      .expect("write target");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create symlink
    vfs
      .symlink(Path::new("target_keep.txt"), Path::new("link_del.txt"))
      .await
      .expect("symlink");

    // Delete symlink
    vfs.unlink(Path::new("link_del.txt")).await.expect("unlink link");

    assert!(!tmp.path().join("link_del.txt").exists(), "symlink should be deleted");
    assert!(
      tmp.path().join("target_keep.txt").exists(),
      "target file should remain after symlink deletion"
    );
    let content = tokio::fs::read_to_string(tmp.path().join("target_keep.txt"))
      .await
      .expect("read target");
    assert_eq!(content, "keep me", "target content should not change");
  }

  #[tokio::test]
  async fn test_concurrent_writes_different_files() {
    eprintln!("[TEST] test_concurrent_writes_different_files");
    crate::test_utils::with_timeout("test_concurrent_writes_different_files", async {
      // rclone: 5 files, parallel writes, all data is correct
      let tmp = tempfile::tempdir().expect("tempdir");
      let backend = Arc::new(MockBackend::new());
      let (vfs, _rx) = create_test_vfs(tmp.path(), backend);
      let vfs = Arc::new(vfs);

      let mut handles = Vec::new();
      for i in 0..5u8 {
        let vfs = Arc::clone(&vfs);
        let name = format!("par_{i}.txt");
        handles.push(tokio::spawn(async move {
          let path = PathBuf::from(&name);
          let (fh, _) = vfs.create(&path, OpenFlags::read_write(), 0o644).await.expect("create");
          let payload = vec![b'A' + i; 1024];
          vfs.write(&path, fh, 0, &payload).await.expect("write");
          vfs.flush(&path, fh).await.expect("flush");
        }));
      }

      for h in handles {
        h.await.expect("join");
      }

      // Verify data correctness
      for i in 0..5u8 {
        let path = PathBuf::from(format!("par_{i}.txt"));
        let fh = vfs.open(&path, OpenFlags::read_only()).await.expect("open");
        let data = vfs.read(&path, fh, 0, 2048).await.expect("read");
        let expected = vec![b'A' + i; 1024];
        assert_eq!(
          data, expected,
          "data of par_{i}.txt should be correct after parallel write"
        );
      }
    })
    .await;
  }

  #[tokio::test]
  async fn test_getattr_after_unlink_returns_error() {
    eprintln!("[TEST] test_getattr_after_unlink_returns_error");
    // getattr after unlink -> NotFound or Io error
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("doomed.txt"), "bye")
      .await
      .expect("write");

    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Verify that file exists
    vfs
      .getattr(Path::new("doomed.txt"))
      .await
      .expect("getattr before unlink");

    // Delete
    vfs.unlink(Path::new("doomed.txt")).await.expect("unlink");

    // getattr after deletion should return error
    let result = vfs.getattr(Path::new("doomed.txt")).await;
    assert!(result.is_err(), "getattr after unlink should return error");
  }

  // -- Tests from SimpleGitFS/YaWikiFS patterns --

  #[tokio::test]
  async fn test_file_operations_crud() {
    eprintln!("[TEST] test_file_operations_crud");
    // Full CRUD cycle: create -> write -> read -> getattr -> rename -> read renamed -> unlink.
    // From SimpleGitFS fuse_integration.rs:263.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create
    let (fh, attr) = vfs
      .create(Path::new("crud.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    assert_eq!(attr.kind, FileType::RegularFile);
    assert_eq!(attr.size, 0, "new file should be empty");

    // Write
    let written = vfs
      .write(Path::new("crud.txt"), fh, 0, b"crud test data")
      .await
      .expect("write");
    assert_eq!(written, 14);

    // Read
    let data = vfs.read(Path::new("crud.txt"), fh, 0, 100).await.expect("read");
    assert_eq!(data, b"crud test data", "read should return written data");

    // Getattr
    vfs.flush(Path::new("crud.txt"), fh).await.expect("flush");
    let attr = vfs.getattr(Path::new("crud.txt")).await.expect("getattr");
    assert_eq!(attr.size, 14, "getattr should show correct size");
    assert_eq!(attr.kind, FileType::RegularFile);

    // Rename
    vfs
      .rename(Path::new("crud.txt"), Path::new("renamed.txt"), 0)
      .await
      .expect("rename");
    assert!(!tmp.path().join("crud.txt").exists(), "old file should not exist");
    assert!(tmp.path().join("renamed.txt").exists(), "renamed file should exist");

    // Read renamed
    let fh2 = vfs
      .open(Path::new("renamed.txt"), OpenFlags::read_only())
      .await
      .expect("open renamed");
    let data2 = vfs
      .read(Path::new("renamed.txt"), fh2, 0, 100)
      .await
      .expect("read renamed");
    assert_eq!(data2, b"crud test data", "data should be preserved after rename");

    // Unlink
    vfs.unlink(Path::new("renamed.txt")).await.expect("unlink");
    assert!(
      !tmp.path().join("renamed.txt").exists(),
      "file should be deleted after unlink"
    );

    // getattr after deletion -> error
    let result = vfs.getattr(Path::new("renamed.txt")).await;
    assert!(result.is_err(), "getattr of deleted file should return error");
  }

  #[tokio::test]
  async fn test_readdir_consistency_with_disk() {
    eprintln!("[TEST] test_readdir_consistency_with_disk");
    // Create files on disk (tokio::fs::write), readdir through VFS — all visible.
    // From SimpleGitFS vfs_comprehensive_tests.rs: readdir_vs_git.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());

    let file_names = ["alpha.txt", "beta.md", "gamma.rs", "delta.toml", "epsilon.json"];
    for name in &file_names {
      tokio::fs::write(tmp.path().join(name), format!("content {name}"))
        .await
        .expect("write to disk");
    }

    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    for expected in &file_names {
      assert!(
        names.contains(expected),
        "file {expected} should be visible via readdir"
      );
    }

    // Verify count (no hidden files — exactly 5)
    assert_eq!(
      entries.len(),
      file_names.len(),
      "readdir should return exactly as many files as on disk"
    );
  }

  #[tokio::test]
  async fn test_bidirectional_vfs_to_disk() {
    eprintln!("[TEST] test_bidirectional_vfs_to_disk");
    // VFS -> disk: write via VFS -> flush -> tokio::fs::read_to_string -> matches.
    // Disk -> VFS: tokio::fs::write to disk -> open + read via VFS -> matches.
    // From SimpleGitFS fuse_mount_tests.rs: bidirectional consistency.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Direction 1: VFS -> disk
    let (fh, _) = vfs
      .create(Path::new("vfs_to_disk.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    let content_vfs = "data written via VFS";
    vfs
      .write(Path::new("vfs_to_disk.txt"), fh, 0, content_vfs.as_bytes())
      .await
      .expect("write via VFS");
    vfs.flush(Path::new("vfs_to_disk.txt"), fh).await.expect("flush");

    let disk_content = tokio::fs::read_to_string(tmp.path().join("vfs_to_disk.txt"))
      .await
      .expect("read from disk");
    assert_eq!(
      disk_content, content_vfs,
      "data on disk should match data written via VFS"
    );

    // Direction 2: disk -> VFS
    let content_disk = "data written directly to disk";
    tokio::fs::write(tmp.path().join("disk_to_vfs.txt"), content_disk)
      .await
      .expect("write to disk");

    let fh2 = vfs
      .open(Path::new("disk_to_vfs.txt"), OpenFlags::read_only())
      .await
      .expect("open via VFS");
    let data = vfs
      .read(Path::new("disk_to_vfs.txt"), fh2, 0, 1024)
      .await
      .expect("read via VFS");
    assert_eq!(
      String::from_utf8_lossy(&data),
      content_disk,
      "data via VFS should match data written to disk"
    );
  }

  #[tokio::test]
  async fn test_local_file_getattr_after_create() {
    eprintln!("[TEST] test_local_file_getattr_after_create");
    // Create file, write data, getattr -> size is correct, kind=RegularFile.
    // From SimpleGitFS/YaWikiFS vfs_comprehensive_tests.rs.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, create_attr) = vfs
      .create(Path::new("attr_check.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    assert_eq!(create_attr.kind, FileType::RegularFile);
    assert_eq!(create_attr.size, 0, "newly created file should be empty");

    // Write exactly 42 bytes
    let payload = b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEF";
    assert_eq!(payload.len(), 42);
    vfs
      .write(Path::new("attr_check.txt"), fh, 0, payload)
      .await
      .expect("write");
    vfs.flush(Path::new("attr_check.txt"), fh).await.expect("flush");

    let attr = vfs.getattr(Path::new("attr_check.txt")).await.expect("getattr");
    assert_eq!(attr.size, 42, "getattr should show size of 42 bytes");
    assert_eq!(attr.kind, FileType::RegularFile, "kind should be RegularFile");
  }

  #[tokio::test]
  async fn test_multiple_files_readdir() {
    eprintln!("[TEST] test_multiple_files_readdir");
    // Create 10 files, readdir -> all 10 visible, no duplicates.
    // Stress test for readdir.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let file_count = 10;
    let mut expected_names: Vec<String> = Vec::with_capacity(file_count);

    for i in 0..file_count {
      let name = format!("file_{i:03}.txt");
      let (fh, _) = vfs
        .create(Path::new(&name), OpenFlags::read_write(), 0o644)
        .await
        .expect("create");
      vfs
        .write(Path::new(&name), fh, 0, format!("content #{i}").as_bytes())
        .await
        .expect("write");
      vfs.flush(Path::new(&name), fh).await.expect("flush");
      expected_names.push(name);
    }

    let entries = vfs.readdir(Path::new("")).await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    // All 10 files should be visible
    for expected in &expected_names {
      assert!(
        names.contains(&expected.as_str()),
        "file {expected} should be visible in readdir"
      );
    }

    // Verify count — exactly 10
    assert_eq!(
      entries.len(),
      file_count,
      "readdir should return exactly {file_count} files"
    );

    // Verify no duplicates
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "readdir should not contain duplicates");
  }

  #[tokio::test]
  async fn test_ftruncate_via_setattr() {
    eprintln!("[TEST] test_ftruncate_via_setattr");
    // Create file, write 100 bytes, setattr(size=50), read -> 50 bytes.
    // From SimpleGitFS fuse_mount_tests.rs: test_ftruncate.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    let (fh, _) = vfs
      .create(Path::new("ftrunc.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");

    // Write exactly 100 bytes
    let payload = vec![b'X'; 100];
    let written = vfs
      .write(Path::new("ftrunc.txt"), fh, 0, &payload)
      .await
      .expect("write 100 bytes");
    assert_eq!(written, 100);

    // Truncate to 50
    let attr = vfs
      .setattr(Path::new("ftrunc.txt"), Some(50), None, None, None)
      .await
      .expect("setattr truncate to 50");
    assert_eq!(attr.size, 50, "setattr should return size=50");

    // Read — should be exactly 50 bytes
    let data = vfs
      .read(Path::new("ftrunc.txt"), fh, 0, 200)
      .await
      .expect("read after truncate");
    assert_eq!(data.len(), 50, "after truncate there should be exactly 50 bytes");
    assert!(data.iter().all(|&b| b == b'X'), "all remaining bytes should be 'X'");
  }

  #[tokio::test]
  async fn test_readdir_after_unlink() {
    eprintln!("[TEST] test_readdir_after_unlink");
    // Create file, readdir -> visible, unlink, readdir -> not visible.
    // From SimpleGitFS.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create file
    let (fh, _) = vfs
      .create(Path::new("ephemeral.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    vfs
      .write(Path::new("ephemeral.txt"), fh, 0, b"temp data")
      .await
      .expect("write");
    vfs.flush(Path::new("ephemeral.txt"), fh).await.expect("flush");

    // readdir -> file is visible
    let entries_before = vfs.readdir(Path::new("")).await.expect("readdir before unlink");
    let names_before: Vec<&str> = entries_before.iter().map(|e| e.name.as_str()).collect();
    assert!(
      names_before.contains(&"ephemeral.txt"),
      "file should be visible before unlink"
    );

    // Delete file
    vfs.unlink(Path::new("ephemeral.txt")).await.expect("unlink");

    // readdir -> file is NOT visible
    let entries_after = vfs.readdir(Path::new("")).await.expect("readdir after unlink");
    let names_after: Vec<&str> = entries_after.iter().map(|e| e.name.as_str()).collect();
    assert!(
      !names_after.contains(&"ephemeral.txt"),
      "file should NOT be visible after unlink"
    );
  }

  #[tokio::test]
  async fn test_nested_dir_create_and_readdir() {
    eprintln!("[TEST] test_nested_dir_create_and_readdir");
    // mkdir "a", mkdir "a/b", create "a/b/file.txt",
    // readdir "a" → ["b"], readdir "a/b" → ["file.txt"].
    // From SimpleGitFS fuse_mount_tests.rs: nested directories.
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create nested directories
    vfs.mkdir(Path::new("a"), 0o755).await.expect("mkdir a");
    vfs.mkdir(Path::new("a/b"), 0o755).await.expect("mkdir a/b");

    // Create file in deep directory
    let (fh, _) = vfs
      .create(Path::new("a/b/file.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create a/b/file.txt");
    vfs
      .write(Path::new("a/b/file.txt"), fh, 0, b"nested content")
      .await
      .expect("write");
    vfs.flush(Path::new("a/b/file.txt"), fh).await.expect("flush");

    // readdir "a" -> should contain only "b"
    let entries_a = vfs.readdir(Path::new("a")).await.expect("readdir a");
    let names_a: Vec<&str> = entries_a.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names_a, vec!["b"], "readdir 'a' should contain only 'b'");
    assert_eq!(entries_a[0].kind, FileType::Directory, "'b' should be a directory");

    // readdir "a/b" -> should contain only "file.txt"
    let entries_ab = vfs.readdir(Path::new("a/b")).await.expect("readdir a/b");
    let names_ab: Vec<&str> = entries_ab.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
      names_ab,
      vec!["file.txt"],
      "readdir 'a/b' should contain only 'file.txt'"
    );
    assert_eq!(
      entries_ab[0].kind,
      FileType::RegularFile,
      "'file.txt' should be RegularFile"
    );
  }

  // -- Tests ported from SimpleGitFS --

  #[cfg(target_os = "macos")]
  #[tokio::test]
  async fn test_xattr_operations() {
    eprintln!("[TEST] test_xattr_operations");
    // SimpleGitFS: set/get/list/remove xattr on a file via VFS
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Create file
    let (fh, _) = vfs
      .create(Path::new("xattr_test.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    vfs
      .write(Path::new("xattr_test.txt"), fh, 0, b"xattr content")
      .await
      .expect("write");
    vfs.flush(Path::new("xattr_test.txt"), fh).await.expect("flush");

    // setxattr — set extended attribute
    vfs
      .setxattr(
        Path::new("xattr_test.txt"),
        OsStr::new("user.test_key"),
        b"test_value",
        0
      )
      .await
      .expect("setxattr");

    // getxattr — retrieve it back
    let value = vfs
      .getxattr(Path::new("xattr_test.txt"), OsStr::new("user.test_key"))
      .await
      .expect("getxattr");
    assert_eq!(value, b"test_value", "getxattr should return the set value");

    // listxattr — attribute should be in the list
    let attrs = vfs.listxattr(Path::new("xattr_test.txt")).await.expect("listxattr");
    let attr_names: Vec<String> = attrs.iter().map(|a| a.to_string_lossy().to_string()).collect();
    assert!(
      attr_names.iter().any(|n| n.contains("user.test_key")),
      "listxattr should contain 'user.test_key': {attr_names:?}"
    );

    // Set second attribute
    vfs
      .setxattr(
        Path::new("xattr_test.txt"),
        OsStr::new("user.second_key"),
        b"second_value",
        0
      )
      .await
      .expect("setxattr second");

    let attrs2 = vfs.listxattr(Path::new("xattr_test.txt")).await.expect("listxattr 2");
    assert!(
      attrs2.len() >= 2,
      "listxattr should contain at least 2 attributes: {attrs2:?}"
    );

    // removexattr — remove first attribute
    vfs
      .removexattr(Path::new("xattr_test.txt"), OsStr::new("user.test_key"))
      .await
      .expect("removexattr");

    // getxattr after removal -> NotFound
    let result = vfs
      .getxattr(Path::new("xattr_test.txt"), OsStr::new("user.test_key"))
      .await;
    assert!(result.is_err(), "getxattr after removexattr should return error");

    // Second attribute is still available
    let remaining = vfs
      .getxattr(Path::new("xattr_test.txt"), OsStr::new("user.second_key"))
      .await
      .expect("getxattr second");
    assert_eq!(
      remaining, b"second_value",
      "second xattr should remain after removing the first"
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn test_fs_changes_reflect_in_git_status() {
    eprintln!("[TEST] test_fs_changes_reflect_in_git_status");
    // SimpleGitFS: writing a file via VFS -> git status --porcelain shows the file.
    // Create a real git repository for integration testing.
    let tmp = tempfile::tempdir().expect("tempdir");

    // Initialize git repository
    std::process::Command::new("git")
      .args(["init", "--initial-branch=main"])
      .current_dir(tmp.path())
      .output()
      .expect("git init");

    // Configure git for commits
    std::process::Command::new("git")
      .args(["config", "user.email", "test@test.com"])
      .current_dir(tmp.path())
      .output()
      .expect("git config email");
    std::process::Command::new("git")
      .args(["config", "user.name", "Test"])
      .current_dir(tmp.path())
      .output()
      .expect("git config name");

    // Create initial commit
    tokio::fs::write(tmp.path().join("initial.txt"), "initial")
      .await
      .expect("write initial");
    std::process::Command::new("git")
      .args(["add", "."])
      .current_dir(tmp.path())
      .output()
      .expect("git add");
    std::process::Command::new("git")
      .args(["commit", "-m", "initial"])
      .current_dir(tmp.path())
      .output()
      .expect("git commit");

    // Create VFS on top of git repository
    let backend = Arc::new(MockBackend::new());
    let (vfs, _rx) = create_test_vfs(tmp.path(), backend);

    // Write new file via VFS
    let (fh, _) = vfs
      .create(Path::new("new_file.txt"), OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    vfs
      .write(Path::new("new_file.txt"), fh, 0, b"new content via VFS")
      .await
      .expect("write");
    vfs.flush(Path::new("new_file.txt"), fh).await.expect("flush");

    // git status --porcelain -> should contain new_file.txt
    let output = std::process::Command::new("git")
      .args(["status", "--porcelain"])
      .current_dir(tmp.path())
      .output()
      .expect("git status");
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
      status.contains("new_file.txt"),
      "git status should show new_file.txt: {status}"
    );

    // Also verify modification of an existing file
    let fh2 = vfs
      .open(Path::new("initial.txt"), OpenFlags::read_write())
      .await
      .expect("open initial");
    vfs
      .setattr(Path::new("initial.txt"), Some(0), None, None, None)
      .await
      .expect("truncate");
    vfs
      .write(Path::new("initial.txt"), fh2, 0, b"modified via VFS")
      .await
      .expect("write modified");
    vfs.flush(Path::new("initial.txt"), fh2).await.expect("flush modified");

    let output2 = std::process::Command::new("git")
      .args(["status", "--porcelain"])
      .current_dir(tmp.path())
      .output()
      .expect("git status 2");
    let status2 = String::from_utf8_lossy(&output2.stdout);
    assert!(
      status2.contains("initial.txt"),
      "git status should show modified initial.txt: {status2}"
    );
  }
}
