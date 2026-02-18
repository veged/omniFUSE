//! End-to-end pipeline tests: VFS -> SyncEngine -> Backend.
//!
//! These tests exercise the full pipeline as a complete system:
//! user writes a file through VFS -> VFS sends FsEvent to SyncEngine ->
//! SyncEngine batches and calls Backend.sync() -> events are emitted
//! to VfsEventHandler.
//!
//! Run: `cargo test -p omnifuse-core --test e2e_pipeline_tests`

#![allow(clippy::expect_used)]

use std::{path::Path, sync::Arc, time::Duration};

use omnifuse_core::{
  backend::{RemoteChange, SyncResult},
  config::{BufferConfig, SyncConfig},
  events::{LogLevel, VfsEventHandler},
  sync_engine::SyncEngine,
  test_utils::{MockBackend, TEST_TIMEOUT, TestEventHandler, with_timeout},
  vfs::OmniFuseVfs
};
use unifuse::{OpenFlags, UniFuseFilesystem};

// ============================================================================
// Helpers
// ============================================================================

/// Create the full pipeline: VFS + SyncEngine + MockBackend.
///
/// The VFS is wired to the SyncEngine via the engine's sender channel,
/// so file operations on the VFS automatically produce FsEvents that
/// the SyncEngine processes.
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

  // Wire VFS to SyncEngine via the engine's sender
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

/// E2E: Create + write + flush + release through VFS triggers sync in backend.
///
/// VFS.release() sends FsEvent::FileClosed which triggers immediate sync
/// in SyncEngine. Verify that MockBackend.sync() was called and the
/// correct file path was included.
#[tokio::test]
async fn test_e2e_write_triggers_sync() {
  eprintln!("[TEST] test_e2e_write_triggers_sync");
  with_timeout("test_e2e_write_triggers_sync", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Full VFS lifecycle: create -> write -> flush -> release
    let path = Path::new("hello.txt");
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"hello world").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    // Wait for SyncEngine to process the FileClosed event
    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Backend should have been called at least once
    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after VFS release"
    );

    // Verify the file path appeared in one of the sync batches
    let calls = backend.sync_calls.lock().expect("lock");
    let all_files: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      all_files.iter().any(|p| p.ends_with("hello.txt")),
      "hello.txt should be in synced files: {all_files:?}"
    );
  })
  .await;
}

/// E2E: Full create-write-close cycle verifies file on disk, sync called,
/// and event handler received on_push.
#[tokio::test]
async fn test_e2e_create_write_close_syncs() {
  eprintln!("[TEST] test_e2e_create_write_close_syncs");
  with_timeout("test_e2e_create_write_close_syncs", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("document.md");
    let content = b"# Hello\n\nThis is a test document.\n";

    // Create -> write -> flush -> release
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, content).await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // 1. File should exist on disk
    let full_path = tmp.path().join("document.md");
    assert!(full_path.exists(), "document.md should exist on disk");

    // 2. File content should match what was written
    let disk_content = std::fs::read(&full_path).expect("read file");
    assert_eq!(disk_content, content, "file content should match");

    // 3. Sync was called
    assert!(backend.sync_call_count() > 0, "sync should have been called");

    // 4. Event handler got on_push
    assert!(
      events.push_count() > 0,
      "on_push should have been called after successful sync"
    );

    // 5. Event handler got on_file_created
    let created = events.created_calls.lock().expect("lock");
    assert!(
      created.iter().any(|p| p.ends_with("document.md")),
      "on_file_created should have been called for document.md"
    );
  })
  .await;
}

/// E2E: Write to 3 files within the debounce window; verify all 3
/// appear in a single sync() call (batched by SyncEngine).
///
/// Uses a long debounce (3600s) so only the final shutdown sync fires,
/// collecting everything into one batch.
#[tokio::test]
async fn test_e2e_multiple_files_batched() {
  eprintln!("[TEST] test_e2e_multiple_files_batched");
  with_timeout("test_e2e_multiple_files_batched", async {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let events = Arc::new(TestEventHandler::new());

    // Long debounce: sync only fires on shutdown (final sync)
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    let vfs = Arc::new(OmniFuseVfs::new(
      tmp.path().to_path_buf(),
      engine.sender(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>,
      BufferConfig::default()
    ));

    // Write to 3 files (VFS.write sends FileModified, not FileClosed)
    let filenames = ["alpha.txt", "beta.txt", "gamma.txt"];
    for name in &filenames {
      let path = Path::new(name);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("content of {name}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      // Do NOT call release — that would send FileClosed which triggers
      // immediate sync. We want to accumulate in dirty set.
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

    // All 3 files should be in that single batch
    let calls = backend.sync_calls.lock().expect("lock");
    for name in &filenames {
      assert!(
        calls[0].iter().any(|p| p.ends_with(name)),
        "{name} should be in the batched sync call, got: {:?}",
        calls[0]
      );
    }
  })
  .await;
}

/// E2E: Create file -> write -> flush -> unlink through VFS.
///
/// Verify: the initial write generates FsEvent::FileModified, unlink
/// generates another FileModified, and the file is removed from disk.
#[tokio::test]
async fn test_e2e_unlink_triggers_sync() {
  eprintln!("[TEST] test_e2e_unlink_triggers_sync");
  with_timeout("test_e2e_unlink_triggers_sync", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("ephemeral.txt");

    // Create -> write -> flush -> release (triggers initial sync)
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"temporary content").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    // Unlink the file
    vfs.unlink(path).await.expect("unlink");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have been called (at least for the initial write+close)
    assert!(backend.sync_call_count() > 0, "sync should have been called");

    // File should be deleted from disk
    let full_path = tmp.path().join("ephemeral.txt");
    assert!(!full_path.exists(), "ephemeral.txt should be deleted from disk");

    // Event handler should have recorded the deletion
    let deleted = events.deleted_calls.lock().expect("lock");
    assert!(
      deleted.iter().any(|p| p.ends_with("ephemeral.txt")),
      "on_file_deleted should have been called for ephemeral.txt"
    );
  })
  .await;
}

/// E2E: Create file -> write -> rename through VFS.
///
/// Verify: rename sends FileModified for both old and new paths,
/// SyncEngine processes them, and the file exists at the new location.
#[tokio::test]
async fn test_e2e_rename_triggers_sync() {
  eprintln!("[TEST] test_e2e_rename_triggers_sync");
  with_timeout("test_e2e_rename_triggers_sync", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let old_path = Path::new("old_name.txt");
    let new_path = Path::new("new_name.txt");

    // Create -> write -> flush -> release
    let (fh, _) = vfs
      .create(old_path, OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    vfs.write(old_path, fh, 0, b"rename me").await.expect("write");
    vfs.flush(old_path, fh).await.expect("flush");
    vfs.release(old_path, fh).await.expect("release");

    wait_processing().await;

    // Rename the file
    vfs.rename(old_path, new_path, 0).await.expect("rename");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have been called
    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after rename"
    );

    // New file should exist on disk, old should not
    assert!(
      tmp.path().join("new_name.txt").exists(),
      "new_name.txt should exist after rename"
    );
    assert!(
      !tmp.path().join("old_name.txt").exists(),
      "old_name.txt should not exist after rename"
    );

    // Event handler should have recorded the rename
    let renamed = events.renamed_calls.lock().expect("lock");
    assert!(
      renamed
        .iter()
        .any(|(old, new)| { old.ends_with("old_name.txt") && new.ends_with("new_name.txt") }),
      "on_file_renamed should have been called with old -> new paths"
    );

    // The new path should appear in one of the sync batches
    let calls = backend.sync_calls.lock().expect("lock");
    let all_files: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      all_files.iter().any(|p| p.ends_with("new_name.txt")),
      "new_name.txt should be in synced files: {all_files:?}"
    );
  })
  .await;
}

/// E2E: Set MockBackend to return error on sync. Write file through VFS,
/// release it. Verify the error was logged via TestEventHandler.
#[tokio::test]
async fn test_e2e_sync_error_logged() {
  eprintln!("[TEST] test_e2e_sync_error_logged");
  with_timeout("test_e2e_sync_error_logged", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    backend.set_sync_error("remote storage unavailable");

    let path = Path::new("will_fail.txt");

    // Create -> write -> flush -> release
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"this sync will fail").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    // Wait for SyncEngine to process and encounter the error
    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have been attempted
    assert!(backend.sync_call_count() > 0, "sync should have been attempted");

    // Error should have been logged via event handler
    assert!(
      events.log_count(LogLevel::Error) > 0,
      "sync error should have been logged via on_log(Error)"
    );

    // Verify the error message mentions the backend error
    let logs = events.log_calls.lock().expect("lock");
    let has_error_msg = logs
      .iter()
      .any(|(level, msg)| *level == LogLevel::Error && msg.contains("remote storage unavailable"));
    assert!(has_error_msg, "error log should contain the backend error message");
  })
  .await;
}

/// E2E: Set sync_result to Success{synced_files: 3}. Write file through
/// VFS, release it. Verify on_push(3) was called on the event handler.
#[tokio::test]
async fn test_e2e_sync_result_events() {
  eprintln!("[TEST] test_e2e_sync_result_events");
  with_timeout("test_e2e_sync_result_events", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });

    let path = Path::new("report.txt");

    // Create -> write -> flush -> release
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"quarterly report data").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // on_push should have been called
    assert!(
      events.push_count() > 0,
      "on_push should have been called after successful sync"
    );

    // Verify the push count matches the SyncResult value
    let pushes = events.push_calls.lock().expect("lock");
    assert!(
      pushes.contains(&3),
      "on_push should have been called with synced_files=3, got: {pushes:?}"
    );
  })
  .await;
}

/// E2E: Set short poll_interval on MockBackend, configure poll_result
/// with a RemoteChange. Verify apply_remote was called and the event
/// handler received on_sync("updated").
#[tokio::test]
async fn test_e2e_poll_remote_applies_changes() {
  eprintln!("[TEST] test_e2e_poll_remote_applies_changes");
  with_timeout("test_e2e_poll_remote_applies_changes", async {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let backend = Arc::new(MockBackend {
      // Short poll interval so the poll worker fires quickly
      poll_interval_dur: Duration::from_millis(50),
      ..MockBackend::new()
    });
    backend.set_poll_result(vec![RemoteChange::Modified {
      path: std::path::PathBuf::from("remote_file.txt"),
      content: b"content from remote".to_vec()
    }]);
    let events = Arc::new(TestEventHandler::new());

    let config = SyncConfig::default();
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (engine, _handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    // Create VFS (even though we don't write through it, the pipeline is wired)
    let _vfs = OmniFuseVfs::new(
      tmp.path().to_path_buf(),
      engine.sender(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>,
      BufferConfig::default()
    );

    // Wait for at least 2-3 poll cycles (50ms each)
    tokio::time::sleep(Duration::from_millis(250)).await;

    // poll_remote should have been called
    assert!(
      backend.poll_call_count() >= 2,
      "poll_remote should have been called at least twice, got: {}",
      backend.poll_call_count()
    );

    // apply_remote should have been called with the changes
    let apply_calls = backend.apply_calls.lock().expect("lock");
    assert!(!apply_calls.is_empty(), "apply_remote should have been called");

    // Event handler should have received on_sync("updated")
    let sync_events = events.sync_calls.lock().expect("lock");
    assert!(
      sync_events.contains(&"updated".to_string()),
      "on_sync(\"updated\") should have been called, got: {sync_events:?}"
    );
  })
  .await;
}

/// E2E: Verify the full VFS -> SyncEngine -> Backend -> EventHandler chain
/// for a read-back cycle: write data, read it back through VFS, and verify
/// consistency while sync runs in the background.
#[tokio::test]
async fn test_e2e_write_read_consistency() {
  eprintln!("[TEST] test_e2e_write_read_consistency");
  with_timeout("test_e2e_write_read_consistency", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("consistent.txt");
    let data = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";

    // Create and write
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, data).await.expect("write");
    vfs.flush(path, fh).await.expect("flush");

    // Read back through VFS before release
    let read_data = vfs.read(path, fh, 0, data.len() as u32).await.expect("read");
    assert_eq!(
      read_data, data,
      "data read back through VFS should match what was written"
    );

    // Release triggers sync
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have happened and file should be consistent
    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after release"
    );
  })
  .await;
}
