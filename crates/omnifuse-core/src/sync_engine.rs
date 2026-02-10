//! Synchronization engine.
//!
//! Orchestrates timing: collects dirty files via debounce,
//! triggers sync on close, handles periodic remote polling.
//!
//! Does NOT handle merge — that is `Backend`'s responsibility.

use std::{
  collections::HashSet,
  path::PathBuf,
  sync::Arc
};

use tokio::{
  sync::mpsc,
  time::{Instant, sleep, sleep_until}
};
use tracing::{debug, error, warn};

use crate::{
  backend::{Backend, SyncResult},
  config::SyncConfig,
  events::{LogLevel, VfsEventHandler}
};

/// Event from the FUSE layer.
#[derive(Debug, Clone)]
pub enum FsEvent {
  /// File modified (write to buffer).
  FileModified(PathBuf),
  /// File closed (after flush).
  FileClosed(PathBuf),
  /// Force sync.
  Flush,
  /// Shutdown.
  Shutdown
}

/// Synchronization engine.
///
/// Orchestrates timing: collects dirty files, batches via debounce,
/// triggers sync on close, handles periodic polling.
///
/// Does NOT handle merge — that is `Backend`'s responsibility.
pub struct SyncEngine {
  event_tx: mpsc::Sender<FsEvent>
}

impl SyncEngine {
  /// Create and start `SyncEngine`.
  ///
  /// Starts two background workers:
  /// 1. `dirty_worker` — collects `FsEvent`, maintains `DirtySet`, triggers sync
  /// 2. `poll_worker` — periodically calls `backend.poll_remote()`
  pub fn start<B: Backend>(
    config: SyncConfig,
    backend: Arc<B>,
    events: Arc<dyn VfsEventHandler>
  ) -> (Self, tokio::task::JoinHandle<()>) {
    let (event_tx, event_rx) = mpsc::channel(256);

    let handle = tokio::spawn(Self::dirty_worker(
      config,
      backend.clone(),
      events.clone(),
      event_rx
    ));

    // Poll worker — separate task
    let poll_backend = backend;
    let poll_events = events;
    tokio::spawn(async move {
      Self::poll_worker(poll_backend, poll_events).await;
    });

    (Self { event_tx }, handle)
  }

  /// Get a `Sender` for sending events.
  #[must_use]
  pub fn sender(&self) -> mpsc::Sender<FsEvent> {
    self.event_tx.clone()
  }

  /// Shut down the `SyncEngine`.
  ///
  /// # Errors
  ///
  /// Returns an error if the channel is already closed.
  pub async fn shutdown(&self) -> anyhow::Result<()> {
    self
      .event_tx
      .send(FsEvent::Shutdown)
      .await
      .map_err(|e| anyhow::anyhow!("error sending Shutdown: {e}"))
  }

  /// Worker for processing dirty files.
  ///
  /// Logic:
  /// - `FileModified` -> add to dirty set, set debounce deadline
  /// - `FileClosed` -> add to dirty set, immediate sync (don't wait for debounce)
  /// - `Flush` -> immediate sync
  /// - `Shutdown` -> final sync, exit
  /// - debounce timeout -> sync if dirty set is not empty
  async fn dirty_worker<B: Backend>(
    config: SyncConfig,
    backend: Arc<B>,
    events: Arc<dyn VfsEventHandler>,
    mut event_rx: mpsc::Receiver<FsEvent>
  ) {
    let mut dirty_set: HashSet<PathBuf> = HashSet::new();
    let mut trigger_sync = false;
    let debounce_duration = config.debounce_timeout();

    // Debounce deadline — far in the future (inactive)
    let far_future = Instant::now() + std::time::Duration::from_secs(365 * 24 * 3600);
    let mut debounce_deadline = far_future;

    loop {
      tokio::select! {
        event = event_rx.recv() => {
          match event {
            Some(FsEvent::FileModified(path)) => {
              dirty_set.insert(path);
              debounce_deadline = Instant::now() + debounce_duration;
            }
            Some(FsEvent::FileClosed(path)) => {
              dirty_set.insert(path);
              trigger_sync = true;
            }
            Some(FsEvent::Flush) => {
              trigger_sync = true;
            }
            Some(FsEvent::Shutdown) | None => {
              // Final sync
              if !dirty_set.is_empty() {
                Self::do_sync(&backend, &events, &mut dirty_set).await;
              }
              break;
            }
          }
        }
        () = sleep_until(debounce_deadline), if !dirty_set.is_empty() => {
          trigger_sync = true;
          debounce_deadline = far_future;
        }
      }

      if trigger_sync && !dirty_set.is_empty() {
        Self::do_sync(&backend, &events, &mut dirty_set).await;
        trigger_sync = false;
      }
    }

    debug!("dirty_worker finished");
  }

  /// Synchronize dirty files.
  async fn do_sync<B: Backend>(
    backend: &Arc<B>,
    events: &Arc<dyn VfsEventHandler>,
    dirty_set: &mut HashSet<PathBuf>
  ) {
    let files: Vec<PathBuf> = dirty_set.drain().filter(|f| backend.should_track(f)).collect();

    if files.is_empty() {
      return;
    }

    debug!(count = files.len(), "syncing dirty files");

    match backend.sync(&files).await {
      Ok(SyncResult::Success { synced_files }) => {
        events.on_push(synced_files);
        debug!(synced_files, "sync successful");
      }
      Ok(SyncResult::Conflict {
        synced_files,
        conflict_files
      }) => {
        events.on_push(synced_files);
        warn!(
          synced_files,
          conflicts = conflict_files.len(),
          "sync with conflicts"
        );
        events.on_log(
          LogLevel::Warn,
          &format!("conflicts: {conflict_files:?}")
        );
      }
      Ok(SyncResult::Offline) => {
        warn!("remote unavailable, files will be synced later");
        events.on_log(LogLevel::Warn, "remote unavailable");
      }
      Err(e) => {
        error!(error = %e, "sync error");
        events.on_log(LogLevel::Error, &format!("sync error: {e}"));
      }
    }
  }

  /// Worker for periodic remote polling.
  async fn poll_worker<B: Backend>(backend: Arc<B>, events: Arc<dyn VfsEventHandler>) {
    let interval = backend.poll_interval();

    loop {
      sleep(interval).await;

      match backend.poll_remote().await {
        Ok(changes) if !changes.is_empty() => {
          let count = changes.len();
          debug!(count, "received changes from remote");

          if let Err(e) = backend.apply_remote(changes).await {
            warn!(error = %e, "error applying remote changes");
            events.on_log(
              LogLevel::Warn,
              &format!("error applying remote changes: {e}")
            );
          } else {
            events.on_sync("updated");
          }
        }
        Ok(_) => {
          // No changes
        }
        Err(e) => {
          warn!(error = %e, "remote poll error");
          events.on_log(LogLevel::Warn, &format!("poll failed: {e}"));
        }
      }
    }
  }
}
