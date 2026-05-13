//! E2E Git user scenarios: full pipeline tests simulating real user workflows.
//!
//! These tests exercise the complete VFS → SyncEngine → Backend pipeline
//! from a user's perspective: create/edit/delete/rename files through VFS
//! and verify the backend receives correct sync events.
//!
//! Run: `cargo test -p omnifuse-core --test e2e_git_user_scenarios`

// User-scenario tests intentionally keep tuple fixtures and lock assertions direct.
#![allow(
  clippy::cast_possible_truncation,
  clippy::doc_markdown,
  clippy::expect_used,
  clippy::significant_drop_tightening,
  clippy::type_complexity
)]

use std::{path::Path, sync::Arc, time::Duration};

use omnifuse_core::{
  Level, Sink,
  backend::SyncResult,
  config::{BufferConfig, SyncConfig},
  sync_engine::SyncEngine,
  test_utils::{MockBackend, TEST_TIMEOUT, TestEventHandler, with_timeout},
  vfs::OmniFuseVfs
};
use unifuse::{OpenFlags, UniFuseFilesystem};

// ─── Helpers (copied from e2e_pipeline_tests.rs for self-containment) ───

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
  let events_dyn: Arc<dyn Sink> = events.clone();
  let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

  let vfs = OmniFuseVfs::new(
    tmp.path().to_path_buf(),
    engine.sender(),
    backend.clone(),
    events.clone() as Arc<dyn Sink>,
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

// ─── Tests ───

/// User creates a new file, writes content, closes it → backend.sync() called with the file.
#[tokio::test]
async fn test_user_creates_new_file_and_syncs() {
  eprintln!("[TEST] test_user_creates_new_file_and_syncs");
  with_timeout("test_user_creates_new_file_and_syncs", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("new_file.txt");
    let content = b"brand new file content";

    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, content).await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after VFS release"
    );

    let calls = backend.sync_calls.lock().expect("lock");
    let all_files: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      all_files.iter().any(|p| p.ends_with("new_file.txt")),
      "new_file.txt should be in synced files: {all_files:?}"
    );
  })
  .await;
}

/// User edits existing file → backend.sync() called with modified file.
#[tokio::test]
async fn test_user_edits_existing_file_and_syncs() {
  eprintln!("[TEST] test_user_edits_existing_file_and_syncs");
  with_timeout("test_user_edits_existing_file_and_syncs", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("existing.txt");

    // First create the file
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs
      .write(path, fh, 0, b"original content")
      .await
      .expect("write initial");
    vfs.flush(path, fh).await.expect("flush initial");
    vfs.release(path, fh).await.expect("release initial");

    wait_processing().await;

    // Now edit it
    let fh = vfs.open(path, OpenFlags::read_write()).await.expect("open");
    vfs.write(path, fh, 0, b"edited content").await.expect("write edit");
    vfs.flush(path, fh).await.expect("flush edit");
    vfs.release(path, fh).await.expect("release edit");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after editing"
    );

    let calls = backend.sync_calls.lock().expect("lock");
    let all_files: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      all_files.iter().any(|p| p.ends_with("existing.txt")),
      "existing.txt should be in synced files: {all_files:?}"
    );
  })
  .await;
}

/// User deletes a file → backend.sync() receives the deletion event, file removed from disk.
#[tokio::test]
async fn test_user_deletes_file_and_syncs() {
  eprintln!("[TEST] test_user_deletes_file_and_syncs");
  with_timeout("test_user_deletes_file_and_syncs", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("to_delete.txt");

    // Create the file first
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"will be deleted").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    // Delete the file
    vfs.unlink(path).await.expect("unlink");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    assert!(backend.sync_call_count() > 0, "sync should have been called");

    // File should be deleted from disk
    let full_path = tmp.path().join("to_delete.txt");
    assert!(!full_path.exists(), "to_delete.txt should be deleted from disk");

    // Event handler should have recorded the deletion
    let deleted = events.deleted_calls.lock().expect("lock");
    assert!(
      deleted.iter().any(|p| p.ends_with("to_delete.txt")),
      "on_file_deleted should have been called for to_delete.txt"
    );
  })
  .await;
}

/// User renames a file → backend.sync() receives both old and new paths.
#[tokio::test]
async fn test_user_renames_file_and_syncs() {
  eprintln!("[TEST] test_user_renames_file_and_syncs");
  with_timeout("test_user_renames_file_and_syncs", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let old_path = Path::new("old_name.txt");
    let new_path = Path::new("new_name.txt");

    // Create the file
    let (fh, _attr) = vfs
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

    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after rename"
    );

    // New file should exist, old should not
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

    // The new path should appear in sync batches
    let calls = backend.sync_calls.lock().expect("lock");
    let all_files: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    assert!(
      all_files.iter().any(|p| p.ends_with("new_name.txt")),
      "new_name.txt should be in synced files: {all_files:?}"
    );
  })
  .await;
}

/// User edits 3 files within debounce window → single batched sync call.
#[tokio::test]
async fn test_user_edits_multiple_files_batched_sync() {
  eprintln!("[TEST] test_user_edits_multiple_files_batched_sync");
  with_timeout("test_user_edits_multiple_files_batched_sync", async {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let backend = Arc::new(MockBackend::new());
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let events = Arc::new(TestEventHandler::new());

    // Long debounce: sync only fires on shutdown (final sync)
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn Sink> = events.clone();
    let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    let vfs = Arc::new(OmniFuseVfs::new(
      tmp.path().to_path_buf(),
      engine.sender(),
      backend.clone(),
      events.clone() as Arc<dyn Sink>,
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

/// User edits file while remote has changes → conflict detected, event handler warned.
#[tokio::test]
async fn test_user_edit_with_remote_change_conflict() {
  eprintln!("[TEST] test_user_edit_with_remote_change_conflict");
  with_timeout("test_user_edit_with_remote_change_conflict", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Conflict {
      synced_files: 0,
      conflict_files: vec![Path::new("conflict.txt").to_path_buf()]
    });

    let path = Path::new("conflict.txt");

    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"local edit").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    assert!(backend.sync_call_count() > 0, "sync should have been called");

    // Event handler should have received a Warn log about the conflict
    assert!(
      events.log_count(Level::Warn) > 0,
      "on_log(Warn) should have been called for conflict"
    );

    let logs = events.log_calls.lock().expect("lock");
    let has_conflict_warn = logs
      .iter()
      .any(|(level, msg)| *level == Level::Warn && msg.contains("conflict"));
    assert!(has_conflict_warn, "warn log should mention conflict, got: {logs:?}");
  })
  .await;
}

/// User writes to file while sync is in progress → data not lost.
#[tokio::test]
async fn test_user_write_during_sync_no_data_loss() {
  eprintln!("[TEST] test_user_write_during_sync_no_data_loss");
  with_timeout("test_user_write_during_sync_no_data_loss", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });
    // Sync takes 300ms — enough time to send a new event while sync runs
    backend.set_sync_delay(Duration::from_millis(300));

    let path = Path::new("sync_race.txt");

    // Write v1 and release (triggers sync with 300ms delay)
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"first version").await.expect("write v1");
    vfs.flush(path, fh).await.expect("flush v1");
    vfs.release(path, fh).await.expect("release v1");

    // Wait 50ms — sync is in progress (300ms delay)
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Write v2 while sync is running
    let fh = vfs.open(path, OpenFlags::read_write()).await.expect("open v2");
    vfs.write(path, fh, 0, b"second version").await.expect("write v2");
    vfs.flush(path, fh).await.expect("flush v2");
    vfs.release(path, fh).await.expect("release v2");

    // Wait for both syncs to complete
    tokio::time::sleep(Duration::from_millis(600)).await;

    backend.clear_sync_delay();
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Sync should have been called at least twice
    let sync_count = backend.sync_call_count();
    assert!(
      sync_count >= 2,
      "sync should be called at least twice: first batch + new write, got {sync_count}"
    );

    // Verify: the file on disk has the SECOND version
    let fh_check = vfs.open(path, OpenFlags::read_only()).await.expect("open check");
    let data = vfs.read(path, fh_check, 0, 1024).await.expect("read");
    vfs.release(path, fh_check).await.expect("release check");
    let content = String::from_utf8(data).expect("utf8");
    assert_eq!(content, "second version", "new write must not be lost during sync");
  })
  .await;
}

/// User writes a large file (1MB) → buffer handles it, sync succeeds.
#[tokio::test]
async fn test_user_writes_large_file_and_syncs() {
  eprintln!("[TEST] test_user_writes_large_file_and_syncs");
  with_timeout("test_user_writes_large_file_and_syncs", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    let path = Path::new("large_file.bin");
    let large_data = vec![0xABu8; 1_048_576]; // 1MB

    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, &large_data).await.expect("write large");
    vfs.flush(path, fh).await.expect("flush large");
    vfs.release(path, fh).await.expect("release large");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    assert!(
      backend.sync_call_count() > 0,
      "sync should have been called after large file release"
    );

    // Verify file content on disk matches
    let fh_check = vfs.open(path, OpenFlags::read_only()).await.expect("open check");
    let data = vfs
      .read(path, fh_check, 0, large_data.len() as u32)
      .await
      .expect("read large");
    vfs.release(path, fh_check).await.expect("release check");

    assert_eq!(
      data.len(),
      large_data.len(),
      "large file size should match: expected {}, got {}",
      large_data.len(),
      data.len()
    );
    assert_eq!(data, large_data, "large file content should match what was written");
  })
  .await;
}
