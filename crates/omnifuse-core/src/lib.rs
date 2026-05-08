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
#[allow(clippy::cast_possible_truncation, clippy::significant_drop_tightening)]
pub mod buffer;
pub mod config;
/// Shared dirty path index for remote refresh protection.
pub mod dirty_index;
pub mod events;
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
pub use events::{LogLevel, NoopEventHandler, VfsEventHandler};
pub use observability::{
  Disposition, ErrorKind, ErrorSource, EventSeverity, NoopOperationalEventSink, ObservabilitySession, OperationContext,
  OperationKind, OperationOutcome, OperationalEvent, OperationalEventSink, RecordingEventSink, init_logging
};
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
pub async fn run_mount<B: Backend>(
  config: MountConfig,
  backend: B,
  events: impl VfsEventHandler
) -> anyhow::Result<()> {
  init_logging(&config.logging)?;

  let events: Arc<dyn VfsEventHandler> = Arc::new(events);
  let backend = Arc::new(backend);
  let observability = Arc::new(ObservabilitySession::new(
    backend.name(),
    config.mount_point.clone(),
    config.local_dir.clone()
  ));
  let mount_context = observability.start_operation(OperationKind::Mount, 1, None);

  events.on_event(&OperationalEvent::MountStarted {
    context: mount_context.clone(),
    backend: backend.name().to_string(),
    mount_point: config.mount_point.clone(),
    local_dir: config.local_dir.clone()
  });

  let result = async {
    // Check FUSE availability
    if !unifuse::UniFuseHost::<OmniFuseVfs<B>>::is_available() {
      anyhow::bail!("FUSE/WinFsp is not installed");
    }

    let init_context = observability.start_operation(OperationKind::Init, 1, None);
    events.on_event(&OperationalEvent::BackendInitStarted {
      context: init_context.clone(),
      backend: backend.name().to_string()
    });

    // Initialize backend
    let init_result = backend.init(&config.local_dir).await?;
    info!(?init_result, "backend initialized");
    events.on_sync(&format!("{init_result:?}"));
    events.on_event(&OperationalEvent::BackendInitFinished {
      context: init_context.clone(),
      outcome: init_result_outcome(&init_result),
      backend: backend.name().to_string(),
      details: Some(format!("{init_result:?}")),
      elapsed_ms: init_context.elapsed_ms()
    });

    // Start SyncEngine
    let (sync_engine, sync_handle) = SyncEngine::start_with_session(
      config.sync.clone(),
      Arc::clone(&backend),
      Arc::clone(&events),
      Arc::clone(&observability)
    );

    // Create VFS
    let vfs = OmniFuseVfs::new_with_session(
      config.local_dir.clone(),
      sync_engine.sender(),
      Arc::clone(&backend),
      Arc::clone(&events),
      Arc::clone(&observability),
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
  .await;

  match result {
    Ok(()) => {
      events.on_event(&OperationalEvent::MountFinished {
        context: mount_context.clone(),
        outcome: OperationOutcome::Success,
        elapsed_ms: mount_context.elapsed_ms()
      });
      Ok(())
    }
    Err(error) => {
      events.on_error(&error.to_string());
      let elapsed_ms = mount_context.elapsed_ms();
      events.on_event(&OperationalEvent::MountFailed {
        context: mount_context,
        severity: EventSeverity::Error,
        error_kind: backend.classify_error(&error),
        source: ErrorSource::Mount,
        disposition: Disposition::Fatal,
        message: error.to_string(),
        elapsed_ms
      });
      Err(error)
    }
  }
}

const fn init_result_outcome(result: &InitResult) -> OperationOutcome {
  match result {
    InitResult::Fresh | InitResult::UpToDate | InitResult::Updated => OperationOutcome::Success,
    InitResult::Conflicts { .. } => OperationOutcome::Conflict,
    InitResult::Offline => OperationOutcome::Deferred
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
