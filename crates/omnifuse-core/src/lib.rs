//! omnifuse-core — ядро VFS для `OmniFuse`.
//!
//! Содержит:
//! - `Backend` trait — единый интерфейс для синхронизируемых хранилищ
//! - `SyncEngine` — оркестрация синхронизации (debounce, poll, retry)
//! - `OmniFuseVfs` — реализация `UniFuseFilesystem` (файловые операции через local dir + буферы)
//! - `FileBufferManager` — кэширование файлов в памяти с LRU-вытеснением
//! - `VfsEventHandler` — trait для событий (UI, логи)
//!
//! # Архитектура
//!
//! ```text
//! ┌─────────────┐     ┌──────────────┐     ┌──────────┐
//! │ FUSE/WinFsp │ ──► │ OmniFuseVfs  │ ──► │ Backend  │
//! │ (unifuse)   │     │ (файлы+буфер)│     │ (git/wiki│
//! └─────────────┘     └──────┬───────┘     └──────────┘
//!                            │
//!                     ┌──────▼───────┐
//!                     │  SyncEngine  │
//!                     │(debounce+poll)│
//!                     └──────────────┘
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod backend;
#[allow(
  clippy::cast_possible_truncation,
  clippy::significant_drop_tightening
)]
pub mod buffer;
pub mod config;
pub mod events;
pub mod sync_engine;
pub mod vfs;

pub use backend::{Backend, InitResult, RemoteChange, SyncResult};
pub use buffer::{FileBuffer, FileBufferManager};
pub use config::{BufferConfig, FuseMountOptions, LoggingConfig, MountConfig, SyncConfig};
pub use events::{LogLevel, NoopEventHandler, VfsEventHandler};
pub use sync_engine::{FsEvent, SyncEngine};
pub use vfs::OmniFuseVfs;

use std::{path::Path, sync::Arc};

use tracing::info;

/// Главная точка входа: смонтировать `OmniFuse`.
///
/// 1. Инициализирует backend (clone/fetch)
/// 2. Запускает `SyncEngine` (dirty tracking + periodic poll)
/// 3. Создаёт `OmniFuseVfs` (implements async `UniFuseFilesystem`)
/// 4. Монтирует через `UniFuseHost`
/// 5. При завершении: flush + final sync
///
/// # Errors
///
/// Возвращает ошибку при невозможности инициализации или монтирования.
pub async fn run_mount<B: Backend>(
  config: MountConfig,
  backend: B,
  events: impl VfsEventHandler
) -> anyhow::Result<()> {
  let events: Arc<dyn VfsEventHandler> = Arc::new(events);
  let backend = Arc::new(backend);

  // Проверить наличие FUSE
  if !unifuse::UniFuseHost::<OmniFuseVfs<B>>::is_available() {
    anyhow::bail!("FUSE/WinFsp не установлен");
  }

  // Инициализировать backend
  let init_result = backend.init(&config.local_dir).await?;
  info!(?init_result, "backend инициализирован");
  events.on_sync(&format!("{init_result:?}"));

  // Запустить SyncEngine
  let (sync_engine, sync_handle) = SyncEngine::start(
    config.sync.clone(),
    Arc::clone(&backend),
    Arc::clone(&events)
  );

  // Создать VFS
  let vfs = OmniFuseVfs::new(
    config.local_dir.clone(),
    sync_engine.sender(),
    Arc::clone(&backend),
    Arc::clone(&events),
    config.buffer.clone()
  );

  // Монтировать
  let host = unifuse::UniFuseHost::new(vfs);
  let mount_options = unifuse::MountOptions {
    fs_name: config.mount_options.fs_name.clone(),
    allow_other: config.mount_options.allow_other,
    read_only: config.mount_options.read_only
  };

  events.on_mounted(backend.name(), &config.mount_point);
  info!(
    mount_point = %config.mount_point.display(),
    backend = backend.name(),
    "монтирование"
  );

  host.mount(&config.mount_point, &mount_options).await?;

  // Завершение
  events.on_unmounted();
  sync_engine.shutdown().await?;
  sync_handle.await?;

  Ok(())
}

/// Проверить доступность платформы FUSE/`WinFsp`.
#[must_use]
pub fn is_fuse_available() -> bool {
  // Используем фиктивный тип для проверки — нам нужен только статический метод
  unifuse::UniFuseHost::<DummyFs>::is_available()
}

/// Фиктивная ФС для проверки доступности FUSE.
struct DummyFs;

impl unifuse::UniFuseFilesystem for DummyFs {
  async fn getattr(&self, _path: &Path) -> Result<unifuse::FileAttr, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn lookup(
    &self,
    _parent: &Path,
    _name: &std::ffi::OsStr
  ) -> Result<unifuse::FileAttr, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn open(
    &self,
    _path: &Path,
    _flags: unifuse::OpenFlags
  ) -> Result<unifuse::FileHandle, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn read(
    &self,
    _path: &Path,
    _fh: unifuse::FileHandle,
    _offset: u64,
    _size: u32
  ) -> Result<Vec<u8>, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn release(
    &self,
    _path: &Path,
    _fh: unifuse::FileHandle
  ) -> Result<(), unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn readdir(
    &self,
    _path: &Path
  ) -> Result<Vec<unifuse::DirEntry>, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn statfs(&self, _path: &Path) -> Result<unifuse::StatFs, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }
}
