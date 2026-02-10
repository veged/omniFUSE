//! `OmniFuseVfs` — реализация `UniFuseFilesystem`.
//!
//! Делегирует:
//! - Файловые операции → local directory (`tokio::fs`) + `FileBufferManager`
//! - Sync уведомления → `SyncEngine` (через канал `FsEvent`)
//! - Фильтрацию → `Backend::should_track()`

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
  backend::Backend,
  buffer::FileBufferManager,
  config::BufferConfig,
  events::VfsEventHandler,
  sync_engine::FsEvent
};

/// Ядро файловой системы `OmniFuse`.
///
/// Реализует async `UniFuseFilesystem`, делегируя:
/// - Файловые операции → local directory (`tokio::fs`) + `FileBufferManager`
/// - Sync уведомления → `SyncEngine` (через канал)
/// - Фильтрацию → `Backend::should_track()`
pub struct OmniFuseVfs<B: Backend> {
  /// Локальная директория (рабочая копия).
  local_dir: PathBuf,
  /// Менеджер буферов файлов.
  buffer_manager: Arc<FileBufferManager>,
  /// Канал для отправки событий в `SyncEngine`.
  sync_tx: mpsc::Sender<FsEvent>,
  /// Backend для фильтрации.
  backend: Arc<B>,
  /// Обработчик событий (UI/логи).
  events: Arc<dyn VfsEventHandler>,
  /// Счётчик file handle.
  next_fh: AtomicU64
}

impl<B: Backend> OmniFuseVfs<B> {
  /// Создать новый VFS.
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

  /// Полный путь на диске для FUSE-пути.
  fn full_path(&self, path: &Path) -> PathBuf {
    self.local_dir.join(path)
  }

  /// Проверить, скрыт ли файл (`.git`, `.vfs`, или по правилам backend'а).
  fn is_hidden(&self, path: &Path, name: &OsStr) -> bool {
    let s = name.to_string_lossy();
    if s == ".git" || s == ".vfs" {
      return true;
    }
    // Проверка backend'а (например, .gitignore)
    !self.backend.should_track(&path.join(name))
  }

  /// Выделить новый file handle.
  fn alloc_fh(&self) -> FileHandle {
    FileHandle(self.next_fh.fetch_add(1, Ordering::Relaxed))
  }

  /// Отправить событие в `SyncEngine` (без блокировки).
  fn send_event(&self, event: FsEvent) {
    let _ = self.sync_tx.try_send(event);
  }
}

/// Преобразовать `tokio::fs::Metadata` → `FileAttr`.
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
    let meta = tokio::fs::symlink_metadata(&full_path)
      .await
      .map_err(FsError::Io)?;
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
      // Если есть буфер — обрезать в буфере
      if let Some(buffer) = self.buffer_manager.get(&full_path) {
        buffer.truncate(new_size).await;
      }
      // Обрезать на диске
      let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&full_path)
        .await
        .map_err(FsError::Io)?;
      file.set_len(new_size).await.map_err(FsError::Io)?;
      self.send_event(FsEvent::FileModified(path.to_path_buf()));
    }

    // Установить время
    #[cfg(unix)]
    if atime.is_some() || mtime.is_some() {
      set_times_unix(&full_path, atime, mtime)?;
    }

    // Установить права
    #[cfg(unix)]
    if let Some(mode_val) = mode {
      set_mode_unix(&full_path, mode_val)?;
    }

    let meta = tokio::fs::symlink_metadata(&full_path)
      .await
      .map_err(FsError::Io)?;
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

    // Загрузить файл в буфер
    self
      .buffer_manager
      .get_or_load(&full_path)
      .await
      .map_err(FsError::Io)?;

    Ok(self.alloc_fh())
  }

  async fn create(
    &self,
    path: &Path,
    _flags: OpenFlags,
    mode: u32
  ) -> Result<(FileHandle, FileAttr), FsError> {
    let full_path = self.full_path(path);

    // Создать файл
    tokio::fs::write(&full_path, b"")
      .await
      .map_err(FsError::Io)?;

    #[cfg(unix)]
    set_mode_unix(&full_path, mode)?;

    // Закэшировать пустой буфер
    self.buffer_manager.cache(&full_path, Vec::new()).await;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    self.events.on_file_created(path);

    let meta = tokio::fs::symlink_metadata(&full_path)
      .await
      .map_err(FsError::Io)?;

    Ok((self.alloc_fh(), metadata_to_attr(&meta)))
  }

  async fn read(
    &self,
    path: &Path,
    _fh: FileHandle,
    offset: u64,
    size: u32
  ) -> Result<Vec<u8>, FsError> {
    let full_path = self.full_path(path);

    let buffer = self
      .buffer_manager
      .get_or_load(&full_path)
      .await
      .map_err(FsError::Io)?;

    Ok(buffer.read(offset, size).await)
  }

  async fn write(
    &self,
    path: &Path,
    _fh: FileHandle,
    offset: u64,
    data: &[u8]
  ) -> Result<u32, FsError> {
    let full_path = self.full_path(path);

    let buffer = self
      .buffer_manager
      .get_or_load(&full_path)
      .await
      .map_err(FsError::Io)?;

    let written = buffer.write(offset, data).await;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    #[allow(clippy::cast_possible_truncation)]
    let written_u32 = written as u32;
    self.events.on_file_written(path, written);

    Ok(written_u32)
  }

  async fn flush(&self, path: &Path, _fh: FileHandle) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    self
      .buffer_manager
      .flush(&full_path)
      .await
      .map_err(FsError::Io)
  }

  async fn release(&self, path: &Path, _fh: FileHandle) -> Result<(), FsError> {
    let full_path = self.full_path(path);

    // Flush на диск
    self
      .buffer_manager
      .flush(&full_path)
      .await
      .map_err(FsError::Io)?;

    // Уведомить sync engine
    self.send_event(FsEvent::FileClosed(path.to_path_buf()));

    Ok(())
  }

  async fn fsync(
    &self,
    path: &Path,
    _fh: FileHandle,
    _datasync: bool
  ) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    self
      .buffer_manager
      .flush(&full_path)
      .await
      .map_err(FsError::Io)
  }

  async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
    let full_path = self.full_path(path);
    let mut entries = Vec::new();

    let mut read_dir = tokio::fs::read_dir(&full_path)
      .await
      .map_err(FsError::Io)?;

    while let Some(entry) = read_dir.next_entry().await.map_err(FsError::Io)? {
      // Скрыть .git, .vfs
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

    tokio::fs::create_dir(&full_path)
      .await
      .map_err(FsError::Io)?;

    #[cfg(unix)]
    set_mode_unix(&full_path, mode)?;

    let meta = tokio::fs::symlink_metadata(&full_path)
      .await
      .map_err(FsError::Io)?;
    Ok(metadata_to_attr(&meta))
  }

  async fn rmdir(&self, path: &Path) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    tokio::fs::remove_dir(&full_path)
      .await
      .map_err(FsError::Io)
  }

  async fn unlink(&self, path: &Path) -> Result<(), FsError> {
    let full_path = self.full_path(path);

    // Удалить из буфера
    self.buffer_manager.remove(&full_path).await;

    tokio::fs::remove_file(&full_path)
      .await
      .map_err(FsError::Io)?;

    self.send_event(FsEvent::FileModified(path.to_path_buf()));
    self.events.on_file_deleted(path);

    Ok(())
  }

  async fn rename(&self, from: &Path, to: &Path, _flags: u32) -> Result<(), FsError> {
    let full_from = self.full_path(from);
    let full_to = self.full_path(to);

    tokio::fs::rename(&full_from, &full_to)
      .await
      .map_err(FsError::Io)?;

    // Обновить буфер
    self.buffer_manager.remove(&full_from).await;

    self.send_event(FsEvent::FileModified(from.to_path_buf()));
    self.send_event(FsEvent::FileModified(to.to_path_buf()));
    self.events.on_file_renamed(from, to);

    Ok(())
  }

  async fn symlink(&self, target: &Path, link: &Path) -> Result<FileAttr, FsError> {
    let full_link = self.full_path(link);

    #[cfg(unix)]
    tokio::fs::symlink(target, &full_link)
      .await
      .map_err(FsError::Io)?;

    #[cfg(not(unix))]
    return Err(FsError::NotSupported);

    let meta = tokio::fs::symlink_metadata(&full_link)
      .await
      .map_err(FsError::Io)?;

    self.send_event(FsEvent::FileModified(link.to_path_buf()));
    Ok(metadata_to_attr(&meta))
  }

  async fn readlink(&self, path: &Path) -> Result<PathBuf, FsError> {
    let full_path = self.full_path(path);
    tokio::fs::read_link(&full_path)
      .await
      .map_err(FsError::Io)
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
  async fn setxattr(
    &self,
    path: &Path,
    name: &OsStr,
    value: &[u8],
    _flags: i32
  ) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    xattr::set(&full_path, name, value).map_err(FsError::Io)
  }

  #[cfg(not(unix))]
  async fn setxattr(
    &self,
    _path: &Path,
    _name: &OsStr,
    _value: &[u8],
    _flags: i32
  ) -> Result<(), FsError> {
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
    let attrs: Vec<_> = xattr::list(&full_path)
      .map_err(FsError::Io)?
      .collect();
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

/// Установить время файла (Unix) через `filetime`.
#[cfg(unix)]
fn set_times_unix(
  path: &Path,
  atime: Option<SystemTime>,
  mtime: Option<SystemTime>
) -> Result<(), FsError> {
  use std::fs;

  // Используем std::fs для установки mtime (поддерживается через set_modified)
  // Для полной поддержки atime+mtime используем nix::sys::stat::utimensat
  let file = fs::File::open(path).map_err(FsError::Io)?;

  if let Some(mt) = mtime {
    file.set_modified(mt).map_err(FsError::Io)?;
  }

  if let Some(at) = atime {
    // std не поддерживает set_accessed напрямую, но на практике mtime важнее
    let _ = at;
  }

  Ok(())
}

/// Установить права файла (Unix).
#[cfg(unix)]
fn set_mode_unix(path: &Path, mode: u32) -> Result<(), FsError> {
  use std::os::unix::fs::PermissionsExt;
  let perms = std::fs::Permissions::from_mode(mode);
  std::fs::set_permissions(path, perms).map_err(FsError::Io)
}

/// Получить statfs (Unix).
#[cfg(unix)]
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
