//! omnifuse-core — VFS core for `OmniFuse`.
//!
//! Contains:
//! - `Backend` trait — unified interface for synchronized storage backends
//! - `SyncEngine` — sync orchestration (debounce, poll, retry)
//! - `OmniFuseVfs` — `UniFuseFilesystem` implementation (file operations via local dir + buffers)
//! - `FileBufferManager` — in-memory file caching with LRU eviction
//! - `VfsEventHandler` — trait for events (UI, logs)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐     ┌──────────────┐     ┌──────────┐
//! │ FUSE/WinFsp │ ──► │ OmniFuseVfs  │ ──► │ Backend  │
//! │ (unifuse)   │     │ (files+buffer)│     │ (git/wiki│
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

/// Main entry point: mount `OmniFuse`.
///
/// 1. Initializes backend (clone/fetch)
/// 2. Starts `SyncEngine` (dirty tracking + periodic poll)
/// 3. Creates `OmniFuseVfs` (implements async `UniFuseFilesystem`)
/// 4. Mounts via `UniFuseHost`
/// 5. On shutdown: flush + final sync
///
/// # Errors
///
/// Returns an error if initialization or mounting fails.
pub async fn run_mount<B: Backend>(
  config: MountConfig,
  backend: B,
  events: impl VfsEventHandler
) -> anyhow::Result<()> {
  let events: Arc<dyn VfsEventHandler> = Arc::new(events);
  let backend = Arc::new(backend);

  // Check FUSE availability
  if !unifuse::UniFuseHost::<OmniFuseVfs<B>>::is_available() {
    anyhow::bail!("FUSE/WinFsp is not installed");
  }

  // Initialize backend
  let init_result = backend.init(&config.local_dir).await?;
  info!(?init_result, "backend initialized");
  events.on_sync(&format!("{init_result:?}"));

  // Start SyncEngine
  let (sync_engine, sync_handle) = SyncEngine::start(
    config.sync.clone(),
    Arc::clone(&backend),
    Arc::clone(&events)
  );

  // Create VFS
  let vfs = OmniFuseVfs::new(
    config.local_dir.clone(),
    sync_engine.sender(),
    Arc::clone(&backend),
    Arc::clone(&events),
    config.buffer.clone()
  );

  // Mount
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
    "mounting"
  );

  host.mount(&config.mount_point, &mount_options).await?;

  // Shutdown
  events.on_unmounted();
  sync_engine.shutdown().await?;
  sync_handle.await?;

  Ok(())
}

/// Check FUSE/`WinFsp` platform availability.
#[must_use]
pub fn is_fuse_available() -> bool {
  // Use a dummy type for the check — we only need the static method
  unifuse::UniFuseHost::<DummyFs>::is_available()
}

/// Dummy filesystem for checking FUSE availability.
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
