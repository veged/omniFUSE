//! Synchronization engine.
//!
//! Orchestrates timing: collects dirty files via debounce,
//! triggers sync on close, handles periodic remote polling.
//!
//! Does NOT handle merge — that is `Backend`'s responsibility.

use std::{
  collections::HashSet,
  path::PathBuf,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering}
  },
  time::Duration
};

use tokio::{
  sync::mpsc,
  time::{Instant, sleep, sleep_until}
};
use tracing::{debug, error, info, warn};

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

/// Iteration counters for runtime monitoring.
#[derive(Debug, Default)]
pub struct WorkerMetrics {
  /// Number of `poll_worker` iterations.
  pub poll_iterations: AtomicU64,
  /// Number of sync iterations (`dirty_worker`).
  pub sync_iterations: AtomicU64,
  /// Number of slow poll iterations (above threshold).
  pub slow_poll_count: AtomicU64,
  /// Number of slow sync iterations (above threshold).
  pub slow_sync_count: AtomicU64
}

/// Threshold for slow iteration warning.
const SLOW_ITERATION_THRESHOLD: Duration = Duration::from_secs(5);

/// Safe `Duration` to milliseconds (u64) conversion.
fn millis(d: Duration) -> u64 {
  d.as_secs()
    .saturating_mul(1000)
    .saturating_add(u64::from(d.subsec_millis()))
}

/// Heartbeat logging interval (every N poll iterations).
const POLL_HEARTBEAT_INTERVAL: u64 = 100;

/// Synchronization engine.
///
/// Orchestrates timing: collects dirty files, batches via debounce,
/// triggers sync on close, handles periodic polling.
///
/// Does NOT handle merge — that is `Backend`'s responsibility.
pub struct SyncEngine {
  event_tx: mpsc::Sender<FsEvent>,
  /// Handle for stopping `poll_worker`.
  poll_handle: tokio::task::JoinHandle<()>,
  /// Runtime worker metrics.
  metrics: Arc<WorkerMetrics>
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
    let metrics = Arc::new(WorkerMetrics::default());

    let dirty_metrics = Arc::clone(&metrics);
    let handle = tokio::spawn(Self::dirty_worker(
      config,
      backend.clone(),
      events.clone(),
      event_rx,
      dirty_metrics
    ));

    // Poll worker — separate task, JoinHandle stored for shutdown
    let poll_backend = backend;
    let poll_events = events;
    let poll_metrics = Arc::clone(&metrics);
    let poll_handle = tokio::spawn(async move {
      Self::poll_worker(poll_backend, poll_events, poll_metrics).await;
    });

    (
      Self {
        event_tx,
        poll_handle,
        metrics
      },
      handle
    )
  }

  /// Get a `Sender` for sending events.
  #[must_use]
  pub fn sender(&self) -> mpsc::Sender<FsEvent> {
    self.event_tx.clone()
  }

  /// Runtime worker metrics.
  #[must_use]
  pub const fn metrics(&self) -> &Arc<WorkerMetrics> {
    &self.metrics
  }

  /// Shut down the `SyncEngine`.
  ///
  /// Aborts `poll_worker` and sends `Shutdown` to `dirty_worker`
  /// for a final sync.
  ///
  /// # Errors
  ///
  /// Returns an error if the channel is already closed.
  pub async fn shutdown(&self) -> anyhow::Result<()> {
    // Abort poll_worker first — it doesn't need graceful shutdown
    self.poll_handle.abort();
    info!("poll_worker stopped");

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
    mut event_rx: mpsc::Receiver<FsEvent>,
    metrics: Arc<WorkerMetrics>
  ) {
    info!(debounce_secs = config.debounce_timeout_secs, "dirty_worker started");

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
                info!(
                  dirty_count = dirty_set.len(),
                  "dirty_worker: final sync before shutdown"
                );
                Self::do_sync(&backend, &events, &mut dirty_set, &metrics).await;
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
        Self::do_sync(&backend, &events, &mut dirty_set, &metrics).await;
        trigger_sync = false;
      }
    }

    info!(
      total_syncs = metrics.sync_iterations.load(Ordering::Relaxed),
      slow_syncs = metrics.slow_sync_count.load(Ordering::Relaxed),
      "dirty_worker finished"
    );
  }

  /// Synchronize dirty files.
  async fn do_sync<B: Backend>(
    backend: &Arc<B>,
    events: &Arc<dyn VfsEventHandler>,
    dirty_set: &mut HashSet<PathBuf>,
    metrics: &WorkerMetrics
  ) {
    let files: Vec<PathBuf> = dirty_set.drain().filter(|f| backend.should_track(f)).collect();

    if files.is_empty() {
      return;
    }

    let iteration = metrics.sync_iterations.fetch_add(1, Ordering::Relaxed) + 1;
    let start = Instant::now();

    debug!(count = files.len(), iteration, "syncing dirty files");

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
        warn!(synced_files, conflicts = conflict_files.len(), "sync with conflicts");
        events.on_log(LogLevel::Warn, &format!("conflicts: {conflict_files:?}"));
      }
      Ok(SyncResult::Offline) => {
        // Return files to dirty set — will sync on next attempt
        dirty_set.extend(files);
        warn!("remote unavailable, files will be synced later");
        events.on_log(LogLevel::Warn, "remote unavailable");
      }
      Err(e) => {
        // Return files to dirty set — will retry on next event
        dirty_set.extend(files);
        error!(error = %e, "sync error");
        events.on_log(LogLevel::Error, &format!("sync error: {e}"));
      }
    }

    let elapsed = start.elapsed();
    if elapsed > SLOW_ITERATION_THRESHOLD {
      metrics.slow_sync_count.fetch_add(1, Ordering::Relaxed);
      warn!(iteration, elapsed_ms = millis(elapsed), "slow sync iteration");
    }
  }

  /// Worker for periodic remote polling.
  ///
  /// Terminated via `JoinHandle::abort()` when `shutdown()` is called.
  async fn poll_worker<B: Backend>(backend: Arc<B>, events: Arc<dyn VfsEventHandler>, metrics: Arc<WorkerMetrics>) {
    let interval = backend.poll_interval();
    info!(interval_ms = millis(interval), "poll_worker started");

    loop {
      sleep(interval).await;

      let iteration = metrics.poll_iterations.fetch_add(1, Ordering::Relaxed) + 1;
      let start = Instant::now();

      match backend.poll_remote().await {
        Ok(changes) if !changes.is_empty() => {
          let count = changes.len();
          debug!(count, iteration, "received changes from remote");

          if let Err(e) = backend.apply_remote(changes).await {
            warn!(error = %e, "error applying remote changes");
            events.on_log(LogLevel::Warn, &format!("error applying remote changes: {e}"));
          } else {
            events.on_sync("updated");
          }
        }
        Ok(_) => {
          // No changes
        }
        Err(e) => {
          warn!(error = %e, iteration, "remote poll error");
          events.on_log(LogLevel::Warn, &format!("poll failed: {e}"));
        }
      }

      let elapsed = start.elapsed();
      if elapsed > SLOW_ITERATION_THRESHOLD {
        metrics.slow_poll_count.fetch_add(1, Ordering::Relaxed);
        warn!(iteration, elapsed_ms = millis(elapsed), "slow poll iteration");
      }

      // Heartbeat: log status every N iterations
      if iteration % POLL_HEARTBEAT_INTERVAL == 0 {
        info!(
          iteration,
          slow_polls = metrics.slow_poll_count.load(Ordering::Relaxed),
          "poll_worker heartbeat"
        );
      }
    }
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::{path::PathBuf, sync::Arc, time::Duration};

  use super::*;
  use crate::{
    backend::SyncResult,
    config::SyncConfig,
    events::LogLevel,
    test_utils::{MockBackend, TestEventHandler}
  };

  /// Helper: create SyncEngine with short debounce.
  fn start_engine(
    backend: Arc<MockBackend>,
    events: Arc<TestEventHandler>,
    debounce_ms: u64
  ) -> (SyncEngine, tokio::task::JoinHandle<()>) {
    let config = SyncConfig {
      debounce_timeout_secs: debounce_ms / 1000,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = events;
    SyncEngine::start(config, backend, events_dyn)
  }

  /// Helper: wait for event processing.
  async fn wait_processing() {
    eprintln!("  [wait] processing events...");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }

  /// Helper: shutdown engine with timeout (prevents deadlock hangs).
  async fn safe_shutdown(engine: &SyncEngine) {
    eprintln!("  [shutdown] engine...");
    tokio::time::timeout(crate::test_utils::TEST_TIMEOUT, engine.shutdown())
      .await
      .expect("shutdown timed out — possible deadlock")
      .expect("shutdown failed");
    eprintln!("  [shutdown] complete");
  }

  /// Helper: join handle with timeout (prevents deadlock hangs).
  async fn safe_join(handle: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(crate::test_utils::TEST_TIMEOUT, handle)
      .await
      .expect("join timed out — possible deadlock")
      .expect("join failed");
  }

  #[tokio::test]
  async fn test_file_modified_batched_by_debounce() {
    eprintln!("[TEST] test_file_modified_batched_by_debounce");
    // debounce = 0 seconds -> sync immediately after processing
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 2 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    // Send multiple FileModified events
    let tx = engine.sender();
    tx.send(FsEvent::FileModified(PathBuf::from("a.txt")))
      .await
      .expect("send");
    tx.send(FsEvent::FileModified(PathBuf::from("b.txt")))
      .await
      .expect("send");

    wait_processing().await;

    safe_shutdown(&engine).await;

    // There should be at least one sync call
    assert!(backend.sync_call_count() >= 1, "sync should have been called");
  }

  #[tokio::test]
  async fn test_file_closed_triggers_immediate_sync() {
    eprintln!("[TEST] test_file_closed_triggers_immediate_sync");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileClosed(PathBuf::from("file.txt")))
      .await
      .expect("send");

    wait_processing().await;

    safe_shutdown(&engine).await;

    // FileClosed triggers sync immediately
    assert!(backend.sync_call_count() >= 1, "FileClosed should trigger sync");
  }

  #[tokio::test]
  async fn test_flush_event_triggers_sync() {
    eprintln!("[TEST] test_flush_event_triggers_sync");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    // First add a file to dirty set, then Flush
    tx.send(FsEvent::FileModified(PathBuf::from("x.txt")))
      .await
      .expect("send");
    tx.send(FsEvent::Flush).await.expect("send");

    wait_processing().await;

    safe_shutdown(&engine).await;

    assert!(backend.sync_call_count() >= 1, "Flush should trigger sync");
  }

  #[tokio::test]
  async fn test_shutdown_triggers_final_sync() {
    eprintln!("[TEST] test_shutdown_triggers_final_sync");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 3600s — very long, sync won't trigger by timer
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
    let (engine, handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let tx = engine.sender();
    tx.send(FsEvent::FileModified(PathBuf::from("pending.txt")))
      .await
      .expect("send");
    // Allow time for event processing (no sync due to debounce)
    tokio::time::sleep(Duration::from_millis(50)).await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Shutdown should trigger final sync
    assert!(backend.sync_call_count() >= 1, "Shutdown should trigger final sync");
  }

  #[tokio::test]
  async fn test_shutdown_on_empty_dirty_set() {
    eprintln!("[TEST] test_shutdown_on_empty_dirty_set");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    let (engine, handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // No dirty files — sync should not be called
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not be called without dirty files"
    );
  }

  #[tokio::test]
  async fn test_multiple_file_modified_deduplicated() {
    eprintln!("[TEST] test_multiple_file_modified_deduplicated");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    // Same path 3 times
    for _ in 0..3 {
      tx.send(FsEvent::FileModified(PathBuf::from("same.txt")))
        .await
        .expect("send");
    }
    tx.send(FsEvent::FileClosed(PathBuf::from("trigger.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    // Verify "same.txt" is not duplicated in sync calls
    let calls = backend.sync_calls.lock().expect("lock");
    for call in calls.iter() {
      let same_count = call.iter().filter(|p| p == &&PathBuf::from("same.txt")).count();
      assert!(same_count <= 1, "same.txt should not be duplicated: {same_count}");
    }
  }

  #[tokio::test]
  async fn test_sync_success_calls_on_push() {
    eprintln!("[TEST] test_sync_success_calls_on_push");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileClosed(PathBuf::from("a.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(events.push_count() >= 1, "on_push should have been called");
    let pushes = events.push_calls.lock().expect("lock");
    assert!(pushes.contains(&3), "on_push(3) expected");
  }

  #[tokio::test]
  async fn test_sync_conflict_logs_warning() {
    eprintln!("[TEST] test_sync_conflict_logs_warning");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Conflict {
      synced_files: 1,
      conflict_files: vec![PathBuf::from("conflict.txt")]
    });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileClosed(PathBuf::from("a.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(events.log_count(LogLevel::Warn) >= 1, "Conflict should log Warn");
  }

  #[tokio::test]
  async fn test_sync_offline_logs_warning() {
    eprintln!("[TEST] test_sync_offline_logs_warning");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Offline);
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileClosed(PathBuf::from("a.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(events.log_count(LogLevel::Warn) >= 1, "Offline should log Warn");
  }

  #[tokio::test]
  async fn test_sync_error_logs_error() {
    eprintln!("[TEST] test_sync_error_logs_error");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_error("connection lost");
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileClosed(PathBuf::from("a.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(events.log_count(LogLevel::Error) >= 1, "Backend error should log Error");
  }

  #[tokio::test]
  async fn test_untracked_files_filtered() {
    eprintln!("[TEST] test_untracked_files_filtered");
    let backend = Arc::new(MockBackend {
      track_fn: Arc::new(|path| !path.to_string_lossy().contains(".git")),
      ..MockBackend::new()
    });
    backend.set_sync_result(SyncResult::Success { synced_files: 0 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    // One tracked, one not
    tx.send(FsEvent::FileClosed(PathBuf::from(".git/config")))
      .await
      .expect("send");
    tx.send(FsEvent::FileClosed(PathBuf::from("readme.md")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    // Verify: .git/config should not be included in sync
    let calls = backend.sync_calls.lock().expect("lock");
    for call in calls.iter() {
      assert!(
        !call.contains(&PathBuf::from(".git/config")),
        ".git/config should not be included in sync"
      );
    }
  }

  #[tokio::test]
  async fn test_mixed_events_ordering() {
    eprintln!("[TEST] test_mixed_events_ordering");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 2 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    tx.send(FsEvent::FileModified(PathBuf::from("a.txt")))
      .await
      .expect("send");
    tx.send(FsEvent::FileClosed(PathBuf::from("b.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(backend.sync_call_count() >= 1, "mixed events should trigger sync");
  }

  #[tokio::test]
  async fn test_sender_clone_works() {
    eprintln!("[TEST] test_sender_clone_works");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    // Clone sender and send from the clone
    let tx_clone = engine.sender();
    tx_clone
      .send(FsEvent::FileClosed(PathBuf::from("via_clone.txt")))
      .await
      .expect("send");

    wait_processing().await;
    safe_shutdown(&engine).await;

    assert!(backend.sync_call_count() >= 1, "sender clone should work");
  }

  #[tokio::test]
  async fn test_poll_worker_calls_poll_remote() {
    eprintln!("[TEST] test_poll_worker_calls_poll_remote");
    crate::test_utils::with_timeout("test_poll_worker_calls_poll_remote", async {
      let backend = Arc::new(MockBackend {
        poll_interval_dur: Duration::from_millis(50),
        ..MockBackend::new()
      });
      let events = Arc::new(TestEventHandler::new());

      let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
      let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), Arc::clone(&backend), events_dyn);

      // Wait for 3-4 poll intervals
      tokio::time::sleep(Duration::from_millis(200)).await;

      assert!(
        backend.poll_call_count() >= 2,
        "poll_remote should be called periodically"
      );
    })
    .await;
  }

  #[tokio::test]
  async fn test_poll_worker_applies_changes() {
    eprintln!("[TEST] test_poll_worker_applies_changes");
    crate::test_utils::with_timeout("test_poll_worker_applies_changes", async {
      let backend = Arc::new(MockBackend {
        poll_interval_dur: Duration::from_millis(50),
        ..MockBackend::new()
      });
      backend.set_poll_result(vec![crate::backend::RemoteChange::Modified {
        path: PathBuf::from("remote.txt"),
        content: b"hello".to_vec()
      }]);
      let events = Arc::new(TestEventHandler::new());

      let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
      let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), Arc::clone(&backend), events_dyn);

      tokio::time::sleep(Duration::from_millis(200)).await;

      let apply_calls = backend.apply_calls.lock().expect("lock");
      assert!(!apply_calls.is_empty(), "apply_remote should have been called");

      let sync_events = events.sync_calls.lock().expect("lock");
      assert!(
        sync_events.contains(&"updated".to_string()),
        "on_sync(\"updated\") expected"
      );
    })
    .await;
  }

  #[tokio::test]
  async fn test_poll_worker_logs_error() {
    eprintln!("[TEST] test_poll_worker_logs_error");
    crate::test_utils::with_timeout("test_poll_worker_logs_error", async {
      let backend = Arc::new(MockBackend {
        poll_interval_dur: Duration::from_millis(50),
        ..MockBackend::new()
      });
      backend.set_poll_error("network down");
      let events = Arc::new(TestEventHandler::new());

      let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
      let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), Arc::clone(&backend), events_dyn);

      tokio::time::sleep(Duration::from_millis(200)).await;

      assert!(events.log_count(LogLevel::Warn) >= 1, "poll error should log Warn");
    })
    .await;
  }

  #[tokio::test]
  async fn test_concurrent_events_no_panic() {
    eprintln!("[TEST] test_concurrent_events_no_panic");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    // 10 concurrent senders
    let mut tasks = Vec::new();
    for i in 0..10 {
      let tx = engine.sender();
      tasks.push(tokio::spawn(async move {
        tx.send(FsEvent::FileModified(PathBuf::from(format!("file_{i}.txt"))))
          .await
          .ok();
      }));
    }

    for task in tasks {
      task.await.expect("task");
    }

    wait_processing().await;
    safe_shutdown(&engine).await;

    // Test should simply not panic
  }

  /// SimpleGitFS pattern: rapid writes to one file within debounce window
  /// should be coalesced into a single sync call.
  ///
  /// Use debounce = 1 second (minimum non-zero in SyncConfig).
  /// Send multiple FileModified for one file at 10ms intervals.
  /// Then shutdown — final sync collects everything into one batch.
  /// Verify sync is called exactly ONCE.
  #[tokio::test]
  async fn test_sync_coalesces_rapid_writes() {
    eprintln!("[TEST] test_sync_coalesces_rapid_writes");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 1 second — events will accumulate before timer fires
    let (engine, handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 1000);

    let tx = engine.sender();
    let path = PathBuf::from("rapidly_written.txt");

    // Send 5 FileModified events for one file at 10ms intervals
    for _ in 0..5 {
      tx.send(FsEvent::FileModified(path.clone())).await.expect("send");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Wait 50ms — debounce hasn't expired yet (1 second), sync should not have triggered
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not trigger before debounce expires"
    );

    // Shutdown triggers final sync for all accumulated dirty files
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // All 5 events for one file coalesced into a single sync call
    assert_eq!(
      backend.sync_call_count(),
      1,
      "rapid writes should be coalesced into one sync"
    );

    // File should appear in sync call exactly once (HashSet deduplication)
    let calls = backend.sync_calls.lock().expect("lock");
    assert_eq!(calls[0].len(), 1, "one file — one entry in the batch");
    assert_eq!(calls[0][0], path, "rapidly_written.txt should be in the batch");
  }

  /// SimpleGitFS pattern: two events with a large gap between them
  /// should result in two separate sync calls.
  ///
  /// Use FileClosed for immediate trigger_sync (without waiting for debounce).
  /// Send first FileClosed, wait for processing, then second FileClosed.
  /// Verify sync_call_count() == 2.
  #[tokio::test]
  async fn test_separate_syncs_after_timeout_gap() {
    eprintln!("[TEST] test_separate_syncs_after_timeout_gap");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 0 — so FileModified events are also processed quickly
    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();

    // First batch: FileClosed triggers immediate sync
    tx.send(FsEvent::FileClosed(PathBuf::from("first.txt")))
      .await
      .expect("send");

    // Wait for first batch processing (200ms — guaranteed gap)
    tokio::time::sleep(Duration::from_millis(200)).await;

    let count_after_first = backend.sync_call_count();
    assert!(count_after_first >= 1, "first FileClosed should trigger sync");

    // Second batch: separate FileClosed after pause
    tx.send(FsEvent::FileClosed(PathBuf::from("second.txt")))
      .await
      .expect("send");

    // Wait for second batch processing
    tokio::time::sleep(Duration::from_millis(200)).await;

    safe_shutdown(&engine).await;

    // Two separate events with a gap -> two separate syncs
    assert_eq!(
      backend.sync_call_count(),
      2,
      "two events with a gap should produce two separate syncs"
    );
  }

  /// SimpleGitFS pattern: behavior during offline and subsequent recovery.
  ///
  /// 1. Backend returns Offline — verify warning is logged.
  /// 2. Switch backend to Success — verify push_count > 0.
  #[tokio::test]
  async fn test_sync_offline_then_recovery() {
    eprintln!("[TEST] test_sync_offline_then_recovery");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // Start in offline mode
    backend.set_sync_result(SyncResult::Offline);

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    // Send event in offline mode
    tx.send(FsEvent::FileClosed(PathBuf::from("offline_file.txt")))
      .await
      .expect("send");
    wait_processing().await;

    // Verify: warning logged due to Offline
    assert!(events.log_count(LogLevel::Warn) >= 1, "Offline should log Warn");
    // push should not have occurred (Offline does not call on_push)
    assert_eq!(events.push_count(), 0, "Offline should not call on_push");

    // Restore connection — switch result to Success
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Send new event — backend is now available
    tx.send(FsEvent::FileClosed(PathBuf::from("recovered_file.txt")))
      .await
      .expect("send");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // After recovery push_count should be > 0
    assert!(events.push_count() > 0, "on_push should be called after recovery");
  }

  #[tokio::test]
  async fn test_debounce_resets_on_new_event() {
    eprintln!("[TEST] test_debounce_resets_on_new_event");
    // SimpleGitFS: debounce timer resets on new FileModified
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 1 second
    let (engine, handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 1000);
    let tx = engine.sender();

    // First FileModified
    tx.send(FsEvent::FileModified(PathBuf::from("file.txt")))
      .await
      .expect("send");
    // Wait 500ms (less than debounce)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second FileModified — debounce timer resets
    tx.send(FsEvent::FileModified(PathBuf::from("file.txt")))
      .await
      .expect("send");
    // Wait another 500ms — 1000ms total from start, but only 500ms since last event
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sync should not have triggered: timer was reset
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not trigger — debounce timer was reset"
    );

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Shutdown triggers final sync
    assert_eq!(backend.sync_call_count(), 1, "shutdown should trigger final sync");
  }

  #[tokio::test]
  async fn test_rapid_close_events_deduplicated() {
    eprintln!("[TEST] test_rapid_close_events_deduplicated");
    // SimpleGitFS: multiple FileClosed for the same path are deduplicated
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    // 5 FileClosed for the same file
    for _ in 0..5 {
      tx.send(FsEvent::FileClosed(PathBuf::from("same.txt")))
        .await
        .expect("send");
    }

    wait_processing().await;
    safe_shutdown(&engine).await;

    // Verify: file appears at most once in each sync call
    let calls = backend.sync_calls.lock().expect("lock");
    for call in calls.iter() {
      let count = call.iter().filter(|p| p == &&PathBuf::from("same.txt")).count();
      assert!(count <= 1, "same.txt should not be duplicated in sync batch: {count}");
    }
  }

  #[tokio::test]
  async fn test_flush_with_multiple_dirty_files() {
    eprintln!("[TEST] test_flush_with_multiple_dirty_files");
    // SimpleGitFS: Flush collects all dirty files into one sync
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 3600s — sync won't trigger by timer
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
    let (engine, _handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let tx = engine.sender();

    // Add 3 files to dirty set
    tx.send(FsEvent::FileModified(PathBuf::from("a.txt")))
      .await
      .expect("send");
    tx.send(FsEvent::FileModified(PathBuf::from("b.txt")))
      .await
      .expect("send");
    tx.send(FsEvent::FileModified(PathBuf::from("c.txt")))
      .await
      .expect("send");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Flush triggers sync for all dirty files
    tx.send(FsEvent::Flush).await.expect("send");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // There should be at least one sync with 3 files
    let calls = backend.sync_calls.lock().expect("lock");
    let total_files: usize = calls.iter().map(|c| c.len()).sum();
    assert!(total_files >= 3, "Flush should sync all 3 files: {total_files}");
  }

  /// End-to-end pipeline test: FileModified -> debounce -> sync called with expected file.
  ///
  /// SimpleGitFS core_pipeline_tests pattern: send FileModified,
  /// wait for debounce to expire, verify MockBackend.sync() received the exact file.
  #[tokio::test]
  async fn test_full_pipeline_write_debounce_sync() {
    eprintln!("[TEST] test_full_pipeline_write_debounce_sync");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 0 seconds — sync triggers immediately after event processing
    let (engine, handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    let target = PathBuf::from("pipeline_test.txt");
    tx.send(FsEvent::FileModified(target.clone())).await.expect("send");

    // Wait for debounce processing (0 seconds) + processing margin
    tokio::time::sleep(Duration::from_millis(200)).await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Verify: sync was called at least once
    assert!(backend.sync_call_count() >= 1, "sync should be called after debounce");

    // Verify: target file is present in one of the sync calls
    let calls = backend.sync_calls.lock().expect("lock");
    let found = calls.iter().any(|batch| batch.contains(&target));
    assert!(found, "pipeline_test.txt should be passed to sync()");

    // Verify: on_push called (end-to-end pipeline to event handler)
    assert!(
      events.push_count() >= 1,
      "on_push should be called after successful sync"
    );
  }

  /// Race condition: write during flush.
  ///
  /// SimpleGitFS concurrent_editing_tests pattern: FileModified(A),
  /// wait for sync to start, then FileModified(B). Both files should be synced
  /// (possibly in 2 separate sync calls).
  #[tokio::test]
  async fn test_write_during_flush_race() {
    eprintln!("[TEST] test_write_during_flush_race");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 0 — sync triggers immediately
    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    let file_a = PathBuf::from("race_a.txt");
    let file_b = PathBuf::from("race_b.txt");

    // Send file A followed by FileClosed to force sync
    tx.send(FsEvent::FileClosed(file_a.clone())).await.expect("send");
    // Minimal delay — sync may have already started
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Send file B while (or right after) sync for A
    tx.send(FsEvent::FileClosed(file_b.clone())).await.expect("send");

    // Wait for both events to be processed
    tokio::time::sleep(Duration::from_millis(300)).await;

    safe_shutdown(&engine).await;

    // Both files should be synced (in one or multiple calls)
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<&PathBuf> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(all_synced.contains(&&file_a), "race_a.txt should be synced");
    assert!(all_synced.contains(&&file_b), "race_b.txt should be synced");
  }

  /// Sync error retains files in dirty set for re-synchronization.
  ///
  /// MockBackend.sync() first returns Err, then Success.
  /// Verify: file remains in dirty set and is synced on next event.
  #[tokio::test]
  async fn test_sync_error_retains_dirty() {
    eprintln!("[TEST] test_sync_error_retains_dirty");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // First sync — error
    backend.set_sync_error("temporary failure");

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();
    let target = PathBuf::from("retry_me.txt");

    // Send FileClosed — will trigger sync which will fail with error
    tx.send(FsEvent::FileClosed(target.clone())).await.expect("send");
    wait_processing().await;

    // Verify: sync was called and error was logged
    assert_eq!(backend.sync_call_count(), 1, "first sync should be called");
    assert!(events.log_count(LogLevel::Error) >= 1, "sync error should be logged");

    // Remove error — next sync will succeed
    *backend.sync_error.lock().expect("lock") = None;
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Send Flush — triggers sync for files remaining in dirty set
    tx.send(FsEvent::Flush).await.expect("send");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // Verify: sync called a second time with the same file (retry)
    assert!(
      backend.sync_call_count() >= 2,
      "sync should be retried for files in dirty set"
    );

    // File retry_me.txt should be in the second sync call
    let calls = backend.sync_calls.lock().expect("lock");
    let second_call_has_file = calls.iter().skip(1).any(|batch| batch.contains(&target));
    assert!(second_call_has_file, "retry_me.txt should be re-synced after error");
  }

  /// FileClosed for a file with should_track=false does not trigger sync.
  ///
  /// Backend filters untracked files in do_sync via should_track().
  /// If all files in dirty set are untracked, sync is not called at all.
  #[tokio::test]
  async fn test_close_event_with_untracked_file() {
    eprintln!("[TEST] test_close_event_with_untracked_file");
    let backend = Arc::new(MockBackend {
      // Track only .txt files
      track_fn: Arc::new(|path| path.extension().and_then(|e| e.to_str()) == Some("txt")),
      ..MockBackend::new()
    });
    backend.set_sync_result(SyncResult::Success { synced_files: 0 });
    let events = Arc::new(TestEventHandler::new());

    let (engine, handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);

    let tx = engine.sender();

    // FileClosed for untracked file (.tmp — not .txt)
    tx.send(FsEvent::FileClosed(PathBuf::from("temp.tmp")))
      .await
      .expect("send");
    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // sync may be called, but the file should NOT be in the arguments
    // (do_sync filters via should_track, and if files is empty — return)
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<&PathBuf> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      !all_synced.contains(&&PathBuf::from("temp.tmp")),
      "untracked file temp.tmp should not be included in sync"
    );

    // on_push should not be called (nothing to sync)
    assert_eq!(
      events.push_count(),
      0,
      "on_push should not be called for untracked file"
    );
  }

  /// Debounce collects FileModified for multiple files into one sync batch.
  ///
  /// SimpleGitFS fuse_integration pattern: 5 different files sent
  /// within debounce window -> one sync call with all 5 files.
  /// Use long debounce (3600s) to guarantee sync won't trigger
  /// by timer — only final sync on shutdown.
  #[tokio::test]
  async fn test_debounce_batches_multiple_files() {
    eprintln!("[TEST] test_debounce_batches_multiple_files");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 5 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 3600s — sync won't trigger by timer
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
    let (engine, handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let tx = engine.sender();

    // Send FileModified for 5 different files
    for i in 0..5 {
      tx.send(FsEvent::FileModified(PathBuf::from(format!("batch_{i}.txt"))))
        .await
        .expect("send");
    }

    // Wait for all events to be processed by worker
    tokio::time::sleep(Duration::from_millis(50)).await;

    // sync should not have triggered — debounce is still far away
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not trigger before debounce expires"
    );

    // Shutdown triggers final sync for all accumulated dirty files
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // One sync call (final on shutdown)
    assert_eq!(
      backend.sync_call_count(),
      1,
      "there should be exactly one sync call (final on shutdown)"
    );

    // Verify: all 5 files in one batch
    let calls = backend.sync_calls.lock().expect("lock");
    assert_eq!(calls[0].len(), 5, "sync batch should contain all 5 files");
    for i in 0..5 {
      let expected = PathBuf::from(format!("batch_{i}.txt"));
      assert!(
        calls[0].contains(&expected),
        "file batch_{i}.txt should be in sync batch"
      );
    }
  }

  /// SyncResult::Offline preserves files for re-synchronization.
  ///
  /// SimpleGitFS pattern: when backend is offline, files are returned
  /// to dirty set. On next Flush -> sync is called again
  /// with the same files.
  #[tokio::test]
  async fn test_sync_offline_preserves_dirty() {
    eprintln!("[TEST] test_sync_offline_preserves_dirty");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // Start with Offline
    backend.set_sync_result(SyncResult::Offline);

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    let target = PathBuf::from("offline_retry.txt");

    // FileClosed -> immediate sync -> Offline -> file returns to dirty set
    tx.send(FsEvent::FileClosed(target.clone())).await.expect("send");
    wait_processing().await;

    assert_eq!(backend.sync_call_count(), 1, "first sync should be called");
    assert!(events.log_count(LogLevel::Warn) >= 1, "Offline should log Warn");

    // Switch result to Success — next sync will succeed
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Flush -> triggers sync for files remaining in dirty set after Offline
    tx.send(FsEvent::Flush).await.expect("send");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // sync called again with the same file (retry after Offline)
    assert!(backend.sync_call_count() >= 2, "sync should be retried after Offline");

    // Verify: file is present in second (or subsequent) sync call
    let calls = backend.sync_calls.lock().expect("lock");
    let retry_has_file = calls.iter().skip(1).any(|batch| batch.contains(&target));
    assert!(retry_has_file, "offline_retry.txt should be re-synced after recovery");

    // on_push should be called after successful retry
    assert!(
      events.push_count() > 0,
      "on_push should be called after successful retry"
    );
  }

  /// FileClosed triggers immediate sync, including files from dirty set.
  ///
  /// SimpleGitFS pattern: FileModified(A), FileModified(B), FileClosed(C)
  /// in quick succession. FileClosed(C) sets trigger_sync=true,
  /// and sync collects all 3 files from dirty set (A, B, C).
  #[tokio::test]
  async fn test_event_processing_order() {
    eprintln!("[TEST] test_event_processing_order");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 3600s — FileModified won't trigger sync by timer
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
    let (engine, _handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let tx = engine.sender();

    let file_a = PathBuf::from("order_a.txt");
    let file_b = PathBuf::from("order_b.txt");
    let file_c = PathBuf::from("order_c.txt");

    // Quick succession: FileModified(A), FileModified(B), FileClosed(C)
    tx.send(FsEvent::FileModified(file_a.clone())).await.expect("send A");
    tx.send(FsEvent::FileModified(file_b.clone())).await.expect("send B");
    tx.send(FsEvent::FileClosed(file_c.clone())).await.expect("send C");

    // Wait for processing — FileClosed(C) should trigger immediate sync
    wait_processing().await;

    safe_shutdown(&engine).await;

    // sync should be called at least once (FileClosed triggers immediately)
    assert!(backend.sync_call_count() >= 1, "FileClosed should trigger sync");

    // All 3 files should be synced (in one or multiple calls)
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<&PathBuf> = calls.iter().flat_map(|batch| batch.iter()).collect();

    assert!(
      all_synced.contains(&&file_a),
      "order_a.txt (FileModified) should be in sync"
    );
    assert!(
      all_synced.contains(&&file_b),
      "order_b.txt (FileModified) should be in sync"
    );
    assert!(
      all_synced.contains(&&file_c),
      "order_c.txt (FileClosed) should be in sync"
    );
  }

  // -- Tests ported from SimpleGitFS/YaWikiFS --

  /// SimpleGitFS: send events, backend returns Offline -> files stay
  /// in dirty set. Then switch backend to Success, trigger flush -> sync
  /// succeeds and files are synchronized.
  ///
  /// Difference from test_sync_offline_preserves_dirty: here we verify
  /// that MULTIPLE files are preserved in queue during offline, and all
  /// are synchronized after recovery.
  #[tokio::test]
  async fn test_sync_queue_offline_then_online() {
    eprintln!("[TEST] test_sync_queue_offline_then_online");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // Start in offline mode
    backend.set_sync_result(SyncResult::Offline);

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    // Send 3 files in offline mode
    let files: Vec<PathBuf> = (0..3).map(|i| PathBuf::from(format!("queued_{i}.txt"))).collect();
    for f in &files {
      tx.send(FsEvent::FileClosed(f.clone())).await.expect("send");
    }
    wait_processing().await;

    // All 3 files should have been sent to sync, but returned to dirty set (Offline)
    let call_count_offline = backend.sync_call_count();
    assert!(
      call_count_offline >= 1,
      "sync should be called during offline: {call_count_offline}"
    );
    // push should NOT occur during Offline
    assert_eq!(events.push_count(), 0, "on_push should not be called during Offline");

    // Restore connection
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });

    // Trigger flush to retry files from dirty set
    tx.send(FsEvent::Flush).await.expect("send flush");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // Verify: push was called after recovery
    assert!(events.push_count() > 0, "on_push should be called after recovery");

    // All 3 files should be synchronized (in one of the sync calls after recovery)
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<&PathBuf> = calls.iter().flat_map(|batch| batch.iter()).collect();
    for f in &files {
      assert!(
        all_synced.contains(&f),
        "file {} should be synchronized after recovery",
        f.display()
      );
    }
  }

  /// SimpleGitFS: sync returns error first N times, then success.
  /// Verify that files remain in dirty set on errors and
  /// are synchronized on successful retry.
  #[tokio::test]
  async fn test_sync_retry_with_backoff() {
    eprintln!("[TEST] test_sync_retry_with_backoff");
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // First sync — error
    backend.set_sync_error("transient error");

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    let target = PathBuf::from("retry_backoff.txt");

    // First attempt — error
    tx.send(FsEvent::FileClosed(target.clone())).await.expect("send");
    wait_processing().await;

    assert_eq!(backend.sync_call_count(), 1, "first sync called");
    assert!(events.log_count(LogLevel::Error) >= 1, "error should be logged");

    // Second attempt — still error (file returned to dirty set)
    tx.send(FsEvent::Flush).await.expect("send flush 1");
    wait_processing().await;

    assert_eq!(backend.sync_call_count(), 2, "second sync called (retry)");
    assert!(events.log_count(LogLevel::Error) >= 2, "error should be logged twice");

    // Third attempt — error again
    tx.send(FsEvent::Flush).await.expect("send flush 2");
    wait_processing().await;

    assert_eq!(backend.sync_call_count(), 3, "third sync called (retry)");

    // Now remove error — next sync will succeed
    *backend.sync_error.lock().expect("lock") = None;
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Fourth attempt — success
    tx.send(FsEvent::Flush).await.expect("send flush 3");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // sync called 4 times (3 errors + 1 success)
    assert!(
      backend.sync_call_count() >= 4,
      "sync should be called at least 4 times: {}",
      backend.sync_call_count()
    );

    // on_push called after successful retry
    assert!(
      events.push_count() > 0,
      "on_push should be called after successful sync"
    );

    // File retry_backoff.txt should be in all sync calls
    let calls = backend.sync_calls.lock().expect("lock");
    let in_each_call = calls.iter().all(|batch| batch.contains(&target));
    assert!(
      in_each_call,
      "retry_backoff.txt should be in every sync call (dirty set preserves it)"
    );
  }

  /// YaWikiFS: while sync is running (with delay), send new FileModified events.
  /// New events should NOT be lost — they will be picked up in the next sync cycle.
  #[tokio::test]
  async fn test_write_during_sync_not_lost() {
    eprintln!("[TEST] test_write_during_sync_not_lost");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    // Set sync delay = 200ms
    backend.set_sync_delay(Duration::from_millis(200));
    let events = Arc::new(TestEventHandler::new());

    let (engine, _handle) = start_engine(Arc::clone(&backend), Arc::clone(&events), 0);
    let tx = engine.sender();

    let file_a = PathBuf::from("during_sync_a.txt");
    let file_b = PathBuf::from("during_sync_b.txt");

    // Send file_a and FileClosed -> trigger sync (with 200ms delay)
    tx.send(FsEvent::FileClosed(file_a.clone())).await.expect("send A");

    // Wait 50ms — sync for file_a already started (200ms delay)
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send file_b WHILE sync for file_a is still running
    tx.send(FsEvent::FileModified(file_b.clone())).await.expect("send B");

    // Wait for file_a sync to complete (200ms) + file_b processing
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Remove delay for final sync
    backend.clear_sync_delay();

    // FileClosed for file_b to guarantee triggering sync
    tx.send(FsEvent::FileClosed(file_b.clone()))
      .await
      .expect("send B close");
    wait_processing().await;

    safe_shutdown(&engine).await;

    // Both files should be synced (in one or multiple calls)
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<&PathBuf> = calls.iter().flat_map(|batch| batch.iter()).collect();

    assert!(all_synced.contains(&&file_a), "during_sync_a.txt should be synced");
    assert!(
      all_synced.contains(&&file_b),
      "during_sync_b.txt should NOT be lost — should be synced in next cycle"
    );
  }

  /// Verify: shutdown sends ALL remaining dirty files to sync
  /// before exiting. No file should be lost.
  #[tokio::test]
  async fn test_shutdown_flushes_dirty_before_exit() {
    eprintln!("[TEST] test_shutdown_flushes_dirty_before_exit");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 5 });
    let events = Arc::new(TestEventHandler::new());

    // debounce = 3600s — sync won't trigger by timer
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
    let (engine, handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let tx = engine.sender();

    // Add 5 files to dirty set (without trigger)
    let files: Vec<PathBuf> = (0..5)
      .map(|i| PathBuf::from(format!("shutdown_flush_{i}.txt")))
      .collect();
    for f in &files {
      tx.send(FsEvent::FileModified(f.clone())).await.expect("send");
    }

    // Allow time for event processing (no sync due to debounce)
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not trigger before shutdown (debounce=3600s)"
    );

    // Shutdown -> final sync for all dirty files
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // sync should be called exactly once (final on shutdown)
    assert_eq!(
      backend.sync_call_count(),
      1,
      "shutdown should trigger exactly one final sync"
    );

    // All 5 files should be in one batch
    let calls = backend.sync_calls.lock().expect("lock");
    assert_eq!(calls[0].len(), 5, "final sync should contain all 5 files");
    for f in &files {
      assert!(calls[0].contains(f), "file {} should be in final sync", f.display());
    }

    // on_push should be called
    assert!(events.push_count() > 0, "on_push should be called after final sync");
  }

  /// Regression test: poll_worker with short interval is properly stopped
  /// on shutdown and does not cause 100% CPU.
  ///
  /// Before the fix, poll_worker was an orphaned task with no cancellation
  /// mechanism, causing busy-wait when the tokio runtime shuts down.
  #[tokio::test]
  async fn test_poll_worker_stops_on_shutdown() {
    eprintln!("[TEST] test_poll_worker_stops_on_shutdown");
    crate::test_utils::with_timeout("test_poll_worker_stops_on_shutdown", async {
      let backend = Arc::new(MockBackend {
        poll_interval_dur: Duration::from_millis(10), // aggressive interval
        ..MockBackend::new()
      });
      let events = Arc::new(TestEventHandler::new());

      let events_dyn: Arc<dyn crate::events::VfsEventHandler> = Arc::clone(&events) as _;
      let (engine, handle) = SyncEngine::start(SyncConfig::default(), Arc::clone(&backend), events_dyn);

      // Let poll_worker run and accumulate iterations
      tokio::time::sleep(Duration::from_millis(100)).await;

      let polls_before = backend.poll_call_count();
      assert!(polls_before >= 3, "poll_worker should have polled: {polls_before}");

      // Shutdown should stop poll_worker
      safe_shutdown(&engine).await;
      safe_join(handle).await;

      // Capture counter immediately after shutdown
      let polls_at_shutdown = backend.poll_call_count();

      // Wait — if poll_worker is still running, counter will grow
      tokio::time::sleep(Duration::from_millis(200)).await;

      let polls_after = backend.poll_call_count();
      assert_eq!(
        polls_at_shutdown, polls_after,
        "poll_worker must NOT poll after shutdown: \
         at_shutdown={polls_at_shutdown}, after_wait={polls_after}"
      );

      // Verify metrics
      let metrics = engine.metrics();
      assert!(
        metrics.poll_iterations.load(std::sync::atomic::Ordering::Relaxed) >= 3,
        "metrics should reflect completed iterations"
      );
    })
    .await;
  }

  /// Regression test: multiple SyncEngines with aggressive poll_interval
  /// are created and destroyed — no leaked tasks.
  ///
  /// Before the fix, each start() left an orphaned poll_worker,
  /// accumulating background tasks and CPU load.
  #[tokio::test]
  async fn test_no_leaked_poll_workers_on_repeated_start_shutdown() {
    eprintln!("[TEST] test_no_leaked_poll_workers_on_repeated_start_shutdown");
    crate::test_utils::with_timeout("test_no_leaked_poll_workers_on_repeated_start_shutdown", async {
      let backend = Arc::new(MockBackend {
        poll_interval_dur: Duration::from_millis(5),
        ..MockBackend::new()
      });

      // Create and destroy 10 engines in a row
      for i in 0..10 {
        let events = Arc::new(TestEventHandler::new());
        let events_dyn: Arc<dyn crate::events::VfsEventHandler> = events;
        let (engine, handle) = SyncEngine::start(SyncConfig::default(), Arc::clone(&backend), events_dyn);

        tokio::time::sleep(Duration::from_millis(20)).await;
        safe_shutdown(&engine).await;
        safe_join(handle).await;
        eprintln!("  engine {i} shutdown OK");
      }

      // Wait — if there are leaked tasks, counter will grow
      let polls_before = backend.poll_call_count();
      tokio::time::sleep(Duration::from_millis(100)).await;
      let polls_after = backend.poll_call_count();

      assert_eq!(
        polls_before, polls_after,
        "no background polls should remain after all engines shut down: \
           before={polls_before}, after={polls_after}"
      );
    })
    .await;
  }
}
