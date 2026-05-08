//! Cross-backend E2E scenarios: tests that apply to any backend implementation.
//!
//! These tests verify behavior that is common across Git, Wiki, and any future backend:
//! offline mode, error recovery, multi-VFS sharing one SyncEngine, graceful shutdown.
//!
//! Run: `cargo test -p omnifuse-core --test e2e_cross_backend_scenarios`

#![allow(clippy::expect_used)]

use std::{path::Path, sync::Arc, time::Duration};

use omnifuse_core::{
  backend::{RemoteRefreshResult, SyncResult},
  config::{BufferConfig, SyncConfig},
  events::{LogLevel, VfsEventHandler},
  sync_engine::{FsEvent, SyncEngine},
  test_utils::{MockBackend, TEST_TIMEOUT, TestEventHandler, with_timeout},
  vfs::OmniFuseVfs
};
use unifuse::{OpenFlags, UniFuseFilesystem};

// ============================================================================
// Helpers
// ============================================================================

/// Create the full pipeline: VFS + SyncEngine + MockBackend.
///
/// Returns (vfs, engine, handle, backend, events, tmpdir).
fn create_pipeline(
  debounce_secs: u64
) -> (
  Arc<OmniFuseVfs<MockBackend>>,
  SyncEngine,
  tokio::task::JoinHandle<()>,
  Arc<MockBackend>,
  Arc<TestEventHandler>,
  tempfile::TempDir
) {
  let tmp = tempfile::tempdir().expect("tmpdir");
  let backend = Arc::new(MockBackend::new());
  let events = Arc::new(TestEventHandler::new());

  let config = SyncConfig {
    debounce_timeout_secs: debounce_secs,
    ..SyncConfig::default()
  };
  let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
  let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

  let vfs = OmniFuseVfs::new(
    tmp.path().to_path_buf(),
    engine.sender(),
    backend.clone(),
    events.clone() as Arc<dyn VfsEventHandler>,
    BufferConfig::default()
  );

  (Arc::new(vfs), engine, handle, backend, events, tmp)
}

/// Shutdown engine gracefully with a timeout guard to prevent deadlock hangs.
async fn safe_shutdown(engine: &SyncEngine) {
  eprintln!("  [shutdown] engine...");
  tokio::time::timeout(TEST_TIMEOUT, engine.shutdown())
    .await
    .expect("shutdown timed out — possible deadlock")
    .expect("shutdown failed");
  eprintln!("  [shutdown] complete");
}

/// Join a background task handle with a timeout guard.
async fn safe_join(handle: tokio::task::JoinHandle<()>) {
  tokio::time::timeout(TEST_TIMEOUT, handle)
    .await
    .expect("join timed out — possible deadlock")
    .expect("join failed");
}

/// Wait for event processing in the SyncEngine (short sleep).
async fn wait_processing() {
  eprintln!("  [wait] processing events...");
  tokio::time::sleep(Duration::from_millis(200)).await;
}

// ============================================================================
// Tests
// ============================================================================

/// 1. Offline mode: write files → go online → sync succeeds.
///
/// User works without network, files queue up, then connection restored →
/// all synced. Verifies: Offline result → no push, Warn logged →
/// switch to Success → push succeeds.
#[tokio::test]
async fn test_offline_then_recovery_syncs_all() {
  eprintln!("[TEST] test_offline_then_recovery_syncs_all");
  with_timeout("test_offline_then_recovery_syncs_all", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);

    // Phase 1: Go offline — sync returns Offline
    backend.set_sync_result(SyncResult::Offline);

    // Write 3 files through VFS
    for name in ["file1.txt", "file2.txt", "file3.txt"] {
      let path = Path::new(name);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("content of {name}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      vfs.release(path, fh).await.expect("release");
    }

    wait_processing().await;

    // Offline: no push should have happened, but Warn should be logged
    assert_eq!(events.push_count(), 0, "no push should occur while offline");
    assert!(
      events.log_count(LogLevel::Warn) >= 1,
      "offline status should log a warning"
    );

    // Phase 2: Go back online — sync succeeds
    backend.set_sync_result(SyncResult::Success { synced_files: 4 });

    // Write a 4th file to trigger a new sync cycle
    let path = Path::new("file4.txt");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"recovered content").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Now push should have happened
    assert!(
      events.push_count() > 0,
      "push should occur after recovering from offline"
    );

    // All 4 files should have been part of sync calls
    let calls = backend.sync_calls.lock().expect("lock");
    let total_synced: usize = calls.iter().map(|batch| batch.len()).sum();
    assert!(
      total_synced >= 4,
      "at least 4 files should have been synced across all calls, got {total_synced}"
    );
  })
  .await;
}

/// 2. Backend returns Conflict → event handler receives warning.
///
/// SyncResult::Conflict → on_log(Warn) called with conflict details.
#[tokio::test]
async fn test_conflict_result_logs_warning() {
  eprintln!("[TEST] test_conflict_result_logs_warning");
  with_timeout("test_conflict_result_logs_warning", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);

    backend.set_sync_result(SyncResult::Conflict {
      synced_files: 0,
      conflict_files: vec![std::path::PathBuf::from("conflict.txt")]
    });

    let path = Path::new("conflict.txt");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"this will conflict").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Warning should have been logged
    assert!(
      events.log_count(LogLevel::Warn) >= 1,
      "conflict result should log a warning"
    );

    // Verify the log message mentions "conflict"
    let logs = events.log_calls.lock().expect("lock");
    let has_conflict = logs
      .iter()
      .any(|(level, msg)| *level == LogLevel::Warn && msg.to_lowercase().contains("conflict"));
    assert!(has_conflict, "warning log should mention 'conflict', got: {logs:?}");
  })
  .await;
}

/// 3. Multiple VFS instances → single SyncEngine.
///
/// 5 VFS instances sharing one SyncEngine sender → all events processed
/// correctly. Each VFS creates 1 file, release triggers FileClosed.
#[tokio::test]
async fn test_multiple_vfs_single_sync_engine() {
  eprintln!("[TEST] test_multiple_vfs_single_sync_engine");
  with_timeout("test_multiple_vfs_single_sync_engine", async {
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let events = Arc::new(TestEventHandler::new());
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let config = SyncConfig {
      debounce_timeout_secs: 0,
      ..SyncConfig::default()
    };
    let (engine, engine_handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);

    let num_vfs = 5u32;
    let mut vfs_handles = Vec::new();

    // Create 5 VFS instances, each with its own tempdir,
    // all sharing the same SyncEngine sender
    for v in 0..num_vfs {
      let tmp = tempfile::tempdir().expect("tempdir");
      let sender = engine.sender();
      let vfs_events: Arc<dyn VfsEventHandler> = Arc::new(omnifuse_core::events::NoopEventHandler);
      let vfs = OmniFuseVfs::new(
        tmp.path().to_path_buf(),
        sender,
        Arc::clone(&backend),
        vfs_events,
        BufferConfig::default()
      );
      let vfs = Arc::new(vfs);

      let h = tokio::spawn(async move {
        let _tmp = tmp; // keep alive
        let name = format!("vfs_{v}.txt");
        let path = Path::new(&name);
        let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
        vfs
          .write(path, fh, 0, format!("data from vfs {v}").as_bytes())
          .await
          .expect("write");
        vfs.flush(path, fh).await.expect("flush");
        vfs.release(path, fh).await.expect("release");
      });
      vfs_handles.push(h);
    }

    for h in vfs_handles {
      h.await.expect("join vfs task");
    }

    // Wait for all events to be processed
    tokio::time::sleep(Duration::from_millis(200)).await;

    safe_shutdown(&engine).await;
    safe_join(engine_handle).await;

    // SyncEngine should have received events from all 5 VFS instances
    let sync_count = backend.sync_call_count();
    assert!(
      sync_count >= 1,
      "sync should have been called at least once, got {sync_count}"
    );

    // Total synced files across all calls should be >= 5
    let calls = backend.sync_calls.lock().expect("lock");
    let total_synced: usize = calls.iter().map(|batch| batch.len()).sum();
    assert!(
      total_synced >= 5,
      "at least 5 files should have been synced across all calls, got {total_synced}"
    );

    eprintln!("  [OK] {num_vfs} VFS instances, {sync_count} syncs, {total_synced} total files synced");
  })
  .await;
}

/// 4. Shutdown → final sync → no data loss.
///
/// Write files with long debounce, don't wait for debounce timer →
/// immediately shutdown → final sync includes all dirty files.
#[tokio::test]
async fn test_shutdown_final_sync_no_data_loss() {
  eprintln!("[TEST] test_shutdown_final_sync_no_data_loss");
  with_timeout("test_shutdown_final_sync_no_data_loss", async {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 5 });
    let events = Arc::new(TestEventHandler::new());

    // Long debounce: sync only fires on shutdown (final sync)
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    let vfs = OmniFuseVfs::new(
      tmp.path().to_path_buf(),
      engine.sender(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>,
      BufferConfig::default()
    );

    // Write 5 files through VFS
    let filenames = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
    for name in &filenames {
      let path = Path::new(name);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("content of {name}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      // Do NOT call release — accumulate in dirty set
    }

    // Allow events to be processed by the worker
    tokio::time::sleep(Duration::from_millis(50)).await;

    // No sync should have triggered yet (debounce=3600s, no FileClosed)
    assert_eq!(
      backend.sync_call_count(),
      0,
      "sync should not trigger during debounce window"
    );

    // Shutdown triggers final sync with all accumulated dirty files
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Exactly one sync call (final on shutdown)
    assert_eq!(
      backend.sync_call_count(),
      1,
      "shutdown should trigger exactly one final sync"
    );

    // All 5 files should be in that single batch
    let calls = backend.sync_calls.lock().expect("lock");
    for name in &filenames {
      assert!(
        calls[0].iter().any(|p| p.ends_with(name)),
        "{name} should be in the final sync batch, got: {:?}",
        calls[0]
      );
    }

    eprintln!("  [OK] all 5 files included in shutdown final sync");
  })
  .await;
}

/// 5. Poll error → graceful handling, worker continues.
///
/// refresh_remote returns Err → Warn logged → next refresh still works.
#[tokio::test]
async fn test_poll_error_graceful_handling() {
  eprintln!("[TEST] test_poll_error_graceful_handling");
  with_timeout("test_poll_error_graceful_handling", async {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let backend = Arc::new(MockBackend {
      // Short poll interval so the poll worker fires quickly
      poll_interval_dur: Duration::from_millis(50),
      ..MockBackend::new()
    });
    backend.set_refresh_error("network down");
    let events = Arc::new(TestEventHandler::new());

    let config = SyncConfig::default();
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (engine, _handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    // Create VFS to keep the pipeline wired
    let _vfs = OmniFuseVfs::new(
      tmp.path().to_path_buf(),
      engine.sender(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>,
      BufferConfig::default()
    );

    // Wait for at least 2-3 poll cycles (50ms each)
    tokio::time::sleep(Duration::from_millis(250)).await;

    // refresh_remote should have been called multiple times
    assert!(
      backend.refresh_call_count() >= 2,
      "refresh_remote should have been called at least twice, got: {}",
      backend.refresh_call_count()
    );

    // Warning should have been logged for the poll error
    assert!(events.log_count(LogLevel::Warn) >= 1, "poll error should log a warning");

    // Verify the log mentions the error
    let logs = events.log_calls.lock().expect("lock");
    let has_network_error = logs
      .iter()
      .any(|(level, msg)| *level == LogLevel::Warn && msg.contains("network down"));
    assert!(
      has_network_error,
      "warning log should mention 'network down', got: {logs:?}"
    );

    // Clear the error by directly accessing the field
    *backend.refresh_error.lock().expect("lock") = None;

    // Set a valid refresh result for future polls
    backend.set_refresh_result(RemoteRefreshResult::Applied {
      changed: vec![std::path::PathBuf::from("remote_updated.txt")],
      deleted: Vec::new()
    });

    // Wait for more poll cycles with the valid result
    tokio::time::sleep(Duration::from_millis(200)).await;

    // User-visible sync signal should be emitted after applied refresh
    let sync_calls = events.sync_calls.lock().expect("lock");
    assert!(
      sync_calls.iter().any(|call| call == "updated"),
      "updated sync signal should be emitted after clearing the error"
    );

    eprintln!(
      "  [OK] refresh error handled gracefully, {} refresh calls, sync signals: {:?}",
      backend.refresh_call_count(),
      sync_calls
    );
  })
  .await;
}

/// 6. Sync error → retry on next event → succeeds.
///
/// sync returns Err → file stays dirty → remove error → Flush → sync succeeds.
#[tokio::test]
async fn test_sync_error_retry_on_next_event() {
  eprintln!("[TEST] test_sync_error_retry_on_next_event");
  with_timeout("test_sync_error_retry_on_next_event", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);

    // Phase 1: Set sync to return error
    backend.set_sync_error("transient failure");

    // Write a file through VFS
    let path = Path::new("retry_file.txt");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"will fail first time").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    // Sync should have been attempted at least once and failed
    assert!(
      backend.sync_call_count() >= 1,
      "sync should have been attempted at least once, got {}",
      backend.sync_call_count()
    );

    // Error should have been logged
    assert!(
      events.log_count(LogLevel::Error) >= 1,
      "sync error should have been logged via on_log(Error)"
    );

    // Verify the error message mentions the backend error
    let logs = events.log_calls.lock().expect("lock");
    let has_transient = logs
      .iter()
      .any(|(level, msg)| *level == LogLevel::Error && msg.contains("transient failure"));
    assert!(
      has_transient,
      "error log should mention 'transient failure', got: {logs:?}"
    );

    // Phase 2: Clear error, set success
    // We need to clear the error. MockBackend doesn't have a clear_sync_error,
    // so we set it to None by directly accessing the field.
    // Actually, we can set a new sync_result and the error takes precedence.
    // Let's check: in MockBackend::sync, it checks sync_error first.
    // We need to clear the error. Let's use the field directly.
    *backend.sync_error.lock().expect("lock") = None;
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Send a Flush event to trigger another sync attempt
    let fh = vfs.open(path, OpenFlags::read_write()).await.expect("open");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");
    // Also send FileClosed via the engine sender to trigger sync
    engine
      .sender()
      .send(FsEvent::FileClosed(path.to_path_buf()))
      .await
      .expect("send FileClosed");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have been called at least twice (initial failure + retry)
    let sync_count = backend.sync_call_count();
    assert!(
      sync_count >= 2,
      "sync should have been called at least twice (error + retry), got {sync_count}"
    );

    // Push should have happened on the successful retry
    assert!(
      events.push_count() > 0,
      "on_push should have been called after successful retry"
    );

    eprintln!(
      "  [OK] sync error recovered after {sync_count} attempts, {} pushes",
      events.push_count()
    );
  })
  .await;
}
