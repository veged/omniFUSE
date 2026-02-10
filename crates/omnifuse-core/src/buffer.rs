//! Кэширование файлов в памяти с LRU-вытеснением.
//!
//! Портировано из `SimpleGitFS` `core/src/vfs/buffer.rs`.
//! Все операции async-safe через `tokio::sync::RwLock` и атомарные типы.

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

/// Кэшированный буфер одного файла.
#[derive(Debug)]
pub struct FileBuffer {
  /// Путь к файлу.
  pub path: PathBuf,
  /// Содержимое файла.
  content: RwLock<Vec<u8>>,
  /// Буфер был изменён (ожидает flush на диск).
  dirty: AtomicBool,
  /// Время последней модификации.
  mtime: RwLock<SystemTime>,
  /// Кэшированный размер файла.
  size: AtomicU64
}

impl FileBuffer {
  /// Создать новый буфер для файла.
  #[must_use]
  pub fn new(path: PathBuf, content: Vec<u8>) -> Self {
    Self::with_mtime(path, content, SystemTime::now())
  }

  /// Создать новый буфер с указанным временем модификации.
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

  /// Прочитать часть содержимого буфера.
  pub async fn read(&self, offset: u64, size: u32) -> Vec<u8> {
    let content = self.content.read().await;
    let start = offset as usize;
    let end = std::cmp::min(start + size as usize, content.len());

    if start >= content.len() {
      return Vec::new();
    }

    content[start..end].to_vec()
  }

  /// Записать данные в буфер.
  ///
  /// Расширяет буфер при необходимости. Помечает буфер как грязный.
  pub async fn write(&self, offset: u64, data: &[u8]) -> usize {
    let mut content = self.content.write().await;
    let offset = offset as usize;

    // Расширить при необходимости
    if offset + data.len() > content.len() {
      content.resize(offset + data.len(), 0);
    }

    content[offset..offset + data.len()].copy_from_slice(data);

    // Обновить метаданные
    self.dirty.store(true, Ordering::SeqCst);
    self.size.store(content.len() as u64, Ordering::SeqCst);

    // Обновить mtime
    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();

    data.len()
  }

  /// Получить полное содержимое буфера.
  pub async fn content(&self) -> Vec<u8> {
    self.content.read().await.clone()
  }

  /// Установить полное содержимое буфера.
  pub async fn set_content(&self, data: Vec<u8>) {
    let mut content = self.content.write().await;
    self.size.store(data.len() as u64, Ordering::SeqCst);
    *content = data;
    self.dirty.store(true, Ordering::SeqCst);

    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();
  }

  /// Обрезать буфер до указанного размера.
  pub async fn truncate(&self, new_size: u64) {
    let mut content = self.content.write().await;
    content.truncate(new_size as usize);
    self.size.store(new_size, Ordering::SeqCst);
    self.dirty.store(true, Ordering::SeqCst);

    let mut mtime = self.mtime.write().await;
    *mtime = SystemTime::now();
  }

  /// Изменён ли буфер (ожидает flush).
  #[must_use]
  pub fn is_dirty(&self) -> bool {
    self.dirty.load(Ordering::SeqCst)
  }

  /// Пометить буфер как чистый (после flush на диск).
  pub fn mark_clean(&self) {
    self.dirty.store(false, Ordering::SeqCst);
  }

  /// Размер файла в байтах.
  #[must_use]
  pub fn size(&self) -> u64 {
    self.size.load(Ordering::SeqCst)
  }

  /// Время последней модификации.
  pub async fn mtime(&self) -> SystemTime {
    *self.mtime.read().await
  }
}

/// Менеджер файловых буферов с LRU-вытеснением.
///
/// Потокобезопасный кэш содержимого файлов в памяти.
/// Вытеснение старых буферов при превышении лимита памяти.
/// Грязные буферы (с ожидающей записью) не вытесняются.
#[derive(Debug)]
pub struct FileBufferManager {
  /// Кэшированные буферы по пути.
  buffers: DashMap<PathBuf, Arc<FileBuffer>>,
  /// Очередь LRU для вытеснения.
  lru_order: RwLock<VecDeque<PathBuf>>,
  /// Текущее использование памяти в байтах.
  memory_usage: AtomicUsize,
  /// Конфигурация.
  config: BufferConfig
}

impl FileBufferManager {
  /// Создать новый менеджер буферов.
  #[must_use]
  pub fn new(config: BufferConfig) -> Self {
    Self {
      buffers: DashMap::new(),
      lru_order: RwLock::new(VecDeque::new()),
      memory_usage: AtomicUsize::new(0),
      config
    }
  }

  /// Получить буфер из кэша (если есть).
  #[must_use]
  pub fn get(&self, path: &Path) -> Option<Arc<FileBuffer>> {
    self.buffers.get(path).map(|r| Arc::clone(r.value()))
  }

  /// Добавить файл в кэш.
  pub async fn cache(&self, path: &Path, content: Vec<u8>) -> Arc<FileBuffer> {
    self.cache_with_mtime(path, content, SystemTime::now()).await
  }

  /// Добавить файл в кэш с указанным временем модификации.
  pub async fn cache_with_mtime(
    &self,
    path: &Path,
    content: Vec<u8>,
    mtime: SystemTime
  ) -> Arc<FileBuffer> {
    let size = content.len();

    // Вытеснить старые буферы если нужно
    if self.config.lru_eviction_enabled {
      self.maybe_evict(size).await;
    }

    let buffer = Arc::new(FileBuffer::with_mtime(path.to_path_buf(), content, mtime));
    self.buffers.insert(path.to_path_buf(), Arc::clone(&buffer));

    // Обновить LRU
    let mut lru = self.lru_order.write().await;
    lru.push_back(path.to_path_buf());

    // Обновить использование памяти
    self.memory_usage.fetch_add(size, Ordering::SeqCst);

    debug!(path = %path.display(), size, "файл закэширован");

    buffer
  }

  /// Получить буфер или загрузить с диска.
  ///
  /// Если файл закэширован и свеж — возвращает кэш.
  /// Если файл был изменён на диске (и буфер не грязный) — перезагружает.
  /// Иначе — читает с диска и кэширует.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности чтения файла.
  pub async fn get_or_load(&self, path: &Path) -> std::io::Result<Arc<FileBuffer>> {
    // Проверить кэш
    if let Some(buffer) = self.get(path) {
      // Грязный буфер — не перезагружать
      if buffer.is_dirty() {
        trace!(path = %path.display(), "cache hit (dirty)");
        self.touch(path).await;
        return Ok(buffer);
      }

      // Проверить свежесть по mtime
      if let Ok(metadata) = tokio::fs::metadata(path).await
        && let Ok(disk_mtime) = metadata.modified()
      {
        let cached_mtime = buffer.mtime().await;
        if disk_mtime > cached_mtime {
          // Файл изменён извне — перезагрузить
          debug!(path = %path.display(), "кэш устарел, перезагрузка с диска");
          self.remove(path).await;
          let content = tokio::fs::read(path).await?;
          return Ok(self.cache_with_mtime(path, content, disk_mtime).await);
        }
      }

      trace!(path = %path.display(), "cache hit");
      self.touch(path).await;
      return Ok(buffer);
    }

    // Загрузить с диска
    trace!(path = %path.display(), "cache miss, загрузка с диска");
    let metadata = tokio::fs::metadata(path).await?;
    let mtime = metadata.modified().unwrap_or_else(|_| SystemTime::now());
    let content = tokio::fs::read(path).await?;
    Ok(self.cache_with_mtime(path, content, mtime).await)
  }

  /// Обновить позицию в LRU (пометить как недавно использованный).
  async fn touch(&self, path: &Path) {
    let mut lru = self.lru_order.write().await;

    // Удалить из текущей позиции
    if let Some(pos) = lru.iter().position(|p| p == path) {
      lru.remove(pos);
    }

    // Добавить в конец (самый свежий)
    lru.push_back(path.to_path_buf());
  }

  /// Вытеснить старые буферы при превышении лимита памяти.
  async fn maybe_evict(&self, additional_size: usize) {
    let max_memory = self.config.max_memory_bytes();
    let current = self.memory_usage.load(Ordering::SeqCst);

    if current.saturating_add(additional_size) <= max_memory {
      return;
    }

    let mut lru = self.lru_order.write().await;

    while self.memory_usage.load(Ordering::SeqCst).saturating_add(additional_size) > max_memory {
      let Some(oldest) = lru.pop_front() else {
        break;
      };

      // Грязные буферы не вытесняем
      if let Some(buffer) = self.buffers.get(&oldest) {
        if buffer.is_dirty() {
          lru.push_back(oldest);
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

        debug!(path = %oldest.display(), size, "буфер вытеснен");
      }
    }
  }

  /// Удалить буфер из кэша.
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

      debug!(path = %path.display(), "буфер удалён");
    }
  }

  /// Получить все грязные буферы.
  #[must_use]
  pub fn dirty_buffers(&self) -> Vec<Arc<FileBuffer>> {
    self
      .buffers
      .iter()
      .filter(|r| r.value().is_dirty())
      .map(|r| Arc::clone(r.value()))
      .collect()
  }

  /// Записать буфер на диск.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности записи.
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

    debug!(path = %path.display(), size = content.len(), "буфер сброшен на диск");

    Ok(())
  }

  /// Записать все грязные буферы на диск.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности записи.
  pub async fn flush_all(&self) -> std::io::Result<()> {
    let dirty = self.dirty_buffers();

    for buffer in dirty {
      let content = buffer.content().await;
      tokio::fs::write(&buffer.path, &content).await?;
      buffer.mark_clean();

      debug!(path = %buffer.path.display(), size = content.len(), "буфер сброшен");
    }

    Ok(())
  }

  /// Текущее использование памяти в байтах.
  #[must_use]
  pub fn memory_usage(&self) -> usize {
    self.memory_usage.load(Ordering::SeqCst)
  }

  /// Количество закэшированных буферов.
  #[must_use]
  pub fn buffer_count(&self) -> usize {
    self.buffers.len()
  }

  /// Очистить все буферы (грязные сначала сбрасываются на диск).
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности записи.
  pub async fn clear(&self) -> std::io::Result<()> {
    self.flush_all().await?;
    self.buffers.clear();
    self.memory_usage.store(0, Ordering::SeqCst);
    self.lru_order.write().await.clear();
    Ok(())
  }
}

#[cfg(test)]
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
}
