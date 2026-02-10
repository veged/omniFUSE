//! unifuse — кроссплатформенная async FUSE-абстракция.
//!
//! Тонкий адаптационный слой поверх существующих Rust-крейтов:
//!
//! - Linux/macOS: **rfuse3** — async, tokio-native, работает напрямую с `/dev/fuse`
//! - Windows: **winfsp-rs** — Native API (не FUSE compatibility layer)
//!
//! # Ключевая идея
//!
//! `UniFuseFilesystem` — async path-based trait, который реализует бизнес-логика (omnifuse-core).
//! Platform adapters (`Rfuse3Adapter`, `WinfspAdapter`) — тонкие обёртки,
//! преобразующие платформо-специфичные вызовы в path-based async вызовы trait'а.
//!
//! # Пример
//!
//! ```ignore
//! use unifuse::{UniFuseFilesystem, UniFuseHost, MountOptions};
//!
//! let fs = MyFilesystem::new();
//! let host = UniFuseHost::new(fs);
//! host.mount("/mnt/myfs", MountOptions::default()).await?;
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod inode;
#[cfg(unix)]
pub mod rfuse3_adapter;
pub mod types;

use std::{
  ffi::OsStr,
  path::{Path, PathBuf},
  sync::Arc,
  time::SystemTime
};

pub use inode::{InodeMap, NodeKind, ROOT_INODE};
pub use types::*;

/// Кроссплатформенный async path-based filesystem trait.
///
/// Аналог cgofuse `FileSystemInterface` (42 метода),
/// но с Rust-идиоматичным async API: `Result<T>`, `&Path`, `Vec<u8>`.
///
/// Async-first: rfuse3 вызывает эти методы из tokio runtime.
/// `WinFsp` adapter использует `tokio::runtime::Handle::block_on()`
/// для вызова async методов из синхронного контекста.
///
/// Реализатор trait'а — `omnifuse-core` (`OmniFuseVfs`).
pub trait UniFuseFilesystem: Send + Sync + 'static {
  // --- Lifecycle ---

  /// Инициализация файловой системы. Вызывается перед первой операцией.
  fn init(&mut self) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  /// Очистка при размонтировании.
  fn destroy(&mut self) {}

  // --- Метаданные ---

  /// Получить атрибуты файла или директории.
  fn getattr(&self, path: &Path) -> impl Future<Output = Result<FileAttr, FsError>> + Send;

  /// Установить атрибуты (размер, время, права).
  fn setattr(
    &self,
    _path: &Path,
    _size: Option<u64>,
    _atime: Option<SystemTime>,
    _mtime: Option<SystemTime>,
    _mode: Option<u32>
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Найти файл в директории по имени.
  fn lookup(
    &self,
    parent: &Path,
    name: &OsStr
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send;

  // --- Файловые операции ---

  /// Открыть файл.
  fn open(
    &self,
    path: &Path,
    flags: OpenFlags
  ) -> impl Future<Output = Result<FileHandle, FsError>> + Send;

  /// Создать и открыть файл.
  fn create(
    &self,
    _path: &Path,
    _flags: OpenFlags,
    _mode: u32
  ) -> impl Future<Output = Result<(FileHandle, FileAttr), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Прочитать данные из файла.
  fn read(
    &self,
    path: &Path,
    fh: FileHandle,
    offset: u64,
    size: u32
  ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send;

  /// Записать данные в файл.
  fn write(
    &self,
    _path: &Path,
    _fh: FileHandle,
    _offset: u64,
    _data: &[u8]
  ) -> impl Future<Output = Result<u32, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Сбросить буферы файла.
  fn flush(
    &self,
    _path: &Path,
    _fh: FileHandle
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  /// Закрыть файл.
  fn release(
    &self,
    path: &Path,
    fh: FileHandle
  ) -> impl Future<Output = Result<(), FsError>> + Send;

  /// Синхронизировать файл на диск.
  fn fsync(
    &self,
    _path: &Path,
    _fh: FileHandle,
    _datasync: bool
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  // --- Директории ---

  /// Прочитать содержимое директории.
  fn readdir(&self, path: &Path) -> impl Future<Output = Result<Vec<DirEntry>, FsError>> + Send;

  /// Создать директорию.
  fn mkdir(
    &self,
    _path: &Path,
    _mode: u32
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Удалить директорию.
  fn rmdir(&self, _path: &Path) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Операции над деревом ---

  /// Удалить файл.
  fn unlink(&self, _path: &Path) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Переименовать файл или директорию.
  fn rename(
    &self,
    _from: &Path,
    _to: &Path,
    _flags: u32
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Символические ссылки ---

  /// Создать символическую ссылку.
  fn symlink(
    &self,
    _target: &Path,
    _link: &Path
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Прочитать содержимое символической ссылки.
  fn readlink(&self, _path: &Path) -> impl Future<Output = Result<PathBuf, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Extended attributes ---

  /// Установить расширенный атрибут.
  fn setxattr(
    &self,
    _path: &Path,
    _name: &OsStr,
    _value: &[u8],
    _flags: i32
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Получить расширенный атрибут.
  fn getxattr(
    &self,
    _path: &Path,
    _name: &OsStr
  ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Список расширенных атрибутов.
  fn listxattr(
    &self,
    _path: &Path
  ) -> impl Future<Output = Result<Vec<std::ffi::OsString>, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Удалить расширенный атрибут.
  fn removexattr(
    &self,
    _path: &Path,
    _name: &OsStr
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Информация о файловой системе ---

  /// Получить статистику файловой системы.
  fn statfs(&self, path: &Path) -> impl Future<Output = Result<StatFs, FsError>> + Send;
}

/// Опции монтирования.
#[derive(Debug, Clone)]
pub struct MountOptions {
  /// Имя файловой системы (отображается в mount).
  pub fs_name: String,
  /// Разрешить доступ другим пользователям.
  pub allow_other: bool,
  /// Монтировать только для чтения.
  pub read_only: bool
}

impl Default for MountOptions {
  fn default() -> Self {
    Self {
      fs_name: "omnifuse".to_string(),
      allow_other: false,
      read_only: false
    }
  }
}

/// Хост для монтирования файловой системы.
///
/// Обёртка над platform-specific mount API:
/// - Unix: rfuse3 `Session` + `MountHandle`
/// - Windows: winfsp `FileSystemHost`
pub struct UniFuseHost<F: UniFuseFilesystem> {
  fs: Arc<F>
}

impl<F: UniFuseFilesystem> UniFuseHost<F> {
  /// Создать новый хост для заданной файловой системы.
  pub fn new(fs: F) -> Self {
    Self { fs: Arc::new(fs) }
  }

  /// Смонтировать файловую систему и заблокировать до unmount.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности монтирования.
  #[cfg(unix)]
  pub async fn mount(&self, mountpoint: &Path, options: &MountOptions) -> Result<(), FsError> {
    use rfuse3_adapter::Rfuse3Adapter;

    let adapter = Rfuse3Adapter::new(Arc::clone(&self.fs), mountpoint.to_path_buf());

    let mut mount_options = rfuse3::MountOptions::default();
    mount_options
      .fs_name(&options.fs_name)
      .allow_other(options.allow_other)
      .read_only(options.read_only);

    let mount_handle = rfuse3::raw::Session::new(mount_options)
      .mount_with_unprivileged(adapter, mountpoint)
      .await
      .map_err(|e| FsError::Other(format!("ошибка монтирования: {e}")))?;

    mount_handle
      .await
      .map_err(|e| FsError::Other(format!("ошибка FUSE сессии: {e}")))?;

    Ok(())
  }

  /// Проверить доступность платформы FUSE/WinFsp.
  #[must_use]
  pub fn is_available() -> bool {
    #[cfg(target_os = "linux")]
    {
      Path::new("/dev/fuse").exists()
    }
    #[cfg(target_os = "macos")]
    {
      Path::new("/Library/Filesystems/macfuse.fs").exists()
        || Path::new("/usr/local/lib/libfuse.dylib").exists()
    }
    #[cfg(windows)]
    {
      false // TODO: winfsp проверка
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
      false
    }
  }
}
