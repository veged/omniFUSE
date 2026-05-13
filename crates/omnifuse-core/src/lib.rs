//! omnifuse-core — VFS core for `OmniFuse`.
//!
//! Contains:
//! - `Backend` trait — unified interface for synchronized storage backends
//! - `SyncEngine` — sync orchestration (debounce, poll, retry)
//! - `OmniFuseVfs` — `UniFuseFilesystem` implementation (file operations via local dir + buffers)
//! - `FileBufferManager` — in-memory file caching with LRU eviction
//! - `event::Sink` — product event sink for GUI, CLI and tests
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
// Unit tests in this crate predate the stricter workspace-wide pedantic profile.
// Keep production linting strict while avoiding broad rewrites of scenario tests.
#![cfg_attr(
  test,
  allow(
    clippy::cmp_owned,
    clippy::doc_markdown,
    clippy::let_unit_value,
    clippy::needless_collect,
    clippy::redundant_closure_for_method_calls,
    clippy::significant_drop_tightening,
    clippy::stable_sort_primitive
  )
)]

pub mod backend;
#[allow(clippy::cast_possible_truncation, clippy::significant_drop_tightening)]
pub mod buffer;
pub mod config;
/// Shared dirty path index for remote refresh protection.
pub mod dirty_index;
pub mod event;
pub mod file_mutations;
pub mod observability;
pub mod sync_engine;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod vfs;

use std::{path::Path, sync::Arc};

pub use backend::{
  Backend, InitResult, RemoteApplyMode, RemoteDeferReason, RemoteRefresh, RemoteRefreshResult, SyncResult
};
pub use buffer::{FileBuffer, FileBufferManager};
pub use config::{BufferConfig, FuseMountOptions, LoggingConfig, MountConfig, SyncConfig};
pub use dirty_index::{DirtyIndex, PathProtection};
pub use event::{
  Action, Code, Error as EventError, Event, Kind, Level, NoopSink, Op, RecordSink, Session, Sink, Source
};
pub use observability::init_logging;
pub use sync_engine::{FsEvent, SyncEngine, WorkerMetrics};
use tracing::info;
pub use vfs::OmniFuseVfs;

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
pub async fn run_mount<B: Backend>(config: MountConfig, backend: B, events: impl Sink) -> anyhow::Result<()> {
  init_logging(&config.logging)?;

  let events: Arc<dyn Sink> = Arc::new(events);
  let backend = Arc::new(backend);
  let session = Arc::new(Session::new(
    backend.name(),
    config.mount_point.clone(),
    config.local_dir.clone()
  ));
  let mount_op = session.op();

  events.emit(
    session
      .op_event(&mount_op, Kind::MountStart, Level::Info, Source::Mount)
      .data(serde_json::json!({
          "backend": backend.name(),
          "mountPoint": config.mount_point.display().to_string(),
          "localDir": config.local_dir.display().to_string()
      }))
  );

  let result = async {
    // Check FUSE availability
    if !unifuse::UniFuseHost::<OmniFuseVfs<B>>::is_available() {
      anyhow::bail!("FUSE/WinFsp is not installed");
    }

    // Initialize backend
    let init_result = backend.init(&config.local_dir).await?;
    info!(?init_result, "backend initialized");

    // Start SyncEngine
    let (sync_engine, sync_handle) = SyncEngine::start_with_session(
      config.sync.clone(),
      Arc::clone(&backend),
      Arc::clone(&events),
      Arc::clone(&session)
    );

    // Create VFS
    let vfs = OmniFuseVfs::new_with_session(
      config.local_dir.clone(),
      sync_engine.sender(),
      Arc::clone(&backend),
      Arc::clone(&events),
      Arc::clone(&session),
      config.buffer.clone()
    );

    // Mount
    let host = unifuse::UniFuseHost::new(vfs);
    let mount_options = unifuse::MountOptions {
      fs_name: config.mount_options.fs_name.clone(),
      allow_other: config.mount_options.allow_other,
      read_only: config.mount_options.read_only
    };

    // Ensure mount point exists and is empty
    std::fs::create_dir_all(&config.mount_point)?;

    events.emit(
      session
        .op_event(&mount_op, Kind::MountReady, Level::Info, Source::Mount)
        .data(serde_json::json!({
            "backend": backend.name(),
            "mountPoint": config.mount_point.display().to_string()
        }))
    );
    info!(
      mount_point = %config.mount_point.display(),
      backend = backend.name(),
      "mounting"
    );

    host.mount(&config.mount_point, &mount_options).await?;

    // Shutdown
    sync_engine.shutdown().await?;
    sync_handle.await?;

    Ok(())
  }
  .await;

  match result {
    Ok(()) => {
      events.emit(
        session
          .op_event(&mount_op, Kind::MountStop, Level::Info, Source::Mount)
          .data(serde_json::json!({
              "elapsedMs": mount_op.elapsed_ms()
          }))
      );
      Ok(())
    }
    Err(error) => {
      events.emit(
        session
          .op_event(&mount_op, Kind::MountFail, Level::Error, Source::Mount)
          .data(serde_json::json!({
              "elapsedMs": mount_op.elapsed_ms()
          }))
          .error(EventError::new(
            error.to_string(),
            backend.classify_error(&error),
            Action::Stop
          ))
      );
      Err(error)
    }
  }
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

  async fn lookup(&self, _parent: &Path, _name: &std::ffi::OsStr) -> Result<unifuse::FileAttr, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn open(&self, _path: &Path, _flags: unifuse::OpenFlags) -> Result<unifuse::FileHandle, unifuse::FsError> {
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

  async fn release(&self, _path: &Path, _fh: unifuse::FileHandle) -> Result<(), unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn readdir(&self, _path: &Path) -> Result<Vec<unifuse::DirEntry>, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }

  async fn statfs(&self, _path: &Path) -> Result<unifuse::StatFs, unifuse::FsError> {
    Err(unifuse::FsError::NotSupported)
  }
}
