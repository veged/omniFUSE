//! Edge-case E2E tests: offline mode, error resilience, race conditions.
//!
//! Run: `cargo test -p omnifuse-core --test e2e_edge_cases`

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

/// Shutdown engine gracefully with a timeout guard.
async fn safe_shutdown(engine: &SyncEngine) {
  tokio::time::timeout(TEST_TIMEOUT, engine.shutdown())
    .await
    .expect("shutdown timed out")
    .expect("shutdown failed");
}

/// Join a background task handle with a timeout guard.
async fn safe_join(handle: tokio::task::JoinHandle<()>) {
  tokio::time::timeout(TEST_TIMEOUT, handle)
    .await
    .expect("join timed out")
    .expect("join failed");
}

/// Wait for event processing in the SyncEngine.
async fn wait_processing() {
  tokio::time::sleep(Duration::from_millis(200)).await;
}

// ============================================================================
// Tests
// ============================================================================

/// 1. Backend set to Offline → sync returns SyncResult::Offline → files remain dirty → Warn logged.
#[tokio::test]
async fn test_offline_mode_sync_returns_offline() {
  eprintln!("[TEST] test_offline_mode_sync_returns_offline");
  with_timeout("test_offline_mode_sync_returns_offline", async {
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    backend.set_sync_result(SyncResult::Offline);

    let (engine, _handle) = SyncEngine::start(
      SyncConfig::default(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>
    );
    let tx = engine.sender();

    tx.send(FsEvent::FileClosed(Path::new("offline.txt").to_path_buf()))
      .await
      .expect("send");
    wait_processing().await;

    // Warn should be logged for Offline
    assert!(events.log_count(LogLevel::Warn) >= 1, "Offline should log Warn");

    // on_push should NOT be called (Offline does not call on_push)
    assert_eq!(events.push_count(), 0, "Offline should not call on_push");

    safe_shutdown(&engine).await;
  })
  .await;
}

/// 2. Backend refresh_error set → poll_worker logs Warn → doesn't crash.
#[tokio::test]
async fn test_offline_mode_poll_fails_gracefully() {
  eprintln!("[TEST] test_offline_mode_poll_fails_gracefully");
  with_timeout("test_offline_mode_poll_fails_gracefully", async {
    let backend = Arc::new(MockBackend {
      poll_interval_dur: Duration::from_millis(50),
      ..MockBackend::new()
    });
    backend.set_refresh_error("network unreachable");
    let events = Arc::new(TestEventHandler::new());

    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), backend.clone(), events_dyn);

    // Wait for several poll cycles
    tokio::time::sleep(Duration::from_millis(200)).await;

    // refresh_remote should have been called
    assert!(
      backend.refresh_call_count() >= 2,
      "refresh_remote should be called despite errors"
    );

    // Warn should be logged for poll errors
    assert!(events.log_count(LogLevel::Warn) >= 1, "poll error should log Warn");

    // No panic — worker continues
  })
  .await;
}

/// 3. Offline → write files → switch to Success → flush → files synced, push_count > 0.
#[tokio::test]
async fn test_recovery_after_offline() {
  eprintln!("[TEST] test_recovery_after_offline");
  with_timeout("test_recovery_after_offline", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);

    // Start offline
    backend.set_sync_result(SyncResult::Offline);

    let path = Path::new("recovery.txt");
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"written while offline").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    // Should have logged Warn for Offline
    assert!(events.log_count(LogLevel::Warn) >= 1, "Offline should log Warn");
    assert_eq!(events.push_count(), 0, "no push while offline");

    // Switch to online
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Flush to trigger retry
    let tx = engine.sender();
    tx.send(FsEvent::Flush).await.expect("send flush");
    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // After recovery, push should have been called
    assert!(events.push_count() > 0, "on_push should be called after recovery");
  })
  .await;
}

/// 4. Sync error → file stays dirty → remove error → Flush → sync succeeds.
#[tokio::test]
async fn test_sync_error_retries_on_next_event() {
  eprintln!("[TEST] test_sync_error_retries_on_next_event");
  with_timeout("test_sync_error_retries_on_next_event", async {
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    // First: error
    backend.set_sync_error("temporary failure");

    let (engine, _handle) = SyncEngine::start(
      SyncConfig::default(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>
    );
    let tx = engine.sender();

    tx.send(FsEvent::FileClosed(Path::new("retry_file.txt").to_path_buf()))
      .await
      .expect("send");
    wait_processing().await;

    assert_eq!(backend.sync_call_count(), 1, "first sync should be called");
    assert!(events.log_count(LogLevel::Error) >= 1, "sync error should be logged");

    // Remove error
    *backend.sync_error.lock().expect("lock") = None;
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Flush triggers retry
    tx.send(FsEvent::Flush).await.expect("send flush");
    wait_processing().await;

    safe_shutdown(&engine).await;

    assert!(backend.sync_call_count() >= 2, "sync should be retried");
    assert!(
      events.push_count() > 0,
      "on_push should be called after successful retry"
    );
  })
  .await;
}

/// 5. refresh_remote returns Err → event handler receives Warn → worker continues.
#[tokio::test]
async fn test_poll_remote_error_logged_not_crashed() {
  eprintln!("[TEST] test_poll_remote_error_logged_not_crashed");
  with_timeout("test_poll_remote_error_logged_not_crashed", async {
    let backend = Arc::new(MockBackend {
      poll_interval_dur: Duration::from_millis(50),
      ..MockBackend::new()
    });
    backend.set_refresh_error("dns resolution failed");
    let events = Arc::new(TestEventHandler::new());

    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), backend.clone(), events_dyn);

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
      backend.refresh_call_count() >= 2,
      "refresh should continue despite errors"
    );
    assert!(events.log_count(LogLevel::Warn) >= 1, "poll error should log Warn");

    // Verify the error message is in the log
    let logs = events.log_calls.lock().expect("lock");
    let has_poll_error = logs.iter().any(|(_level, msg)| msg.contains("poll failed"));
    assert!(has_poll_error, "log should contain poll error message");
  })
  .await;
}

/// 6. refresh_remote applies changes → poll continues working.
#[tokio::test]
async fn test_apply_remote_does_not_break_polling() {
  eprintln!("[TEST] test_apply_remote_does_not_break_polling");
  with_timeout("test_apply_remote_does_not_break_polling", async {
    let backend = Arc::new(MockBackend {
      poll_interval_dur: Duration::from_millis(50),
      ..MockBackend::new()
    });
    backend.set_refresh_result(RemoteRefreshResult::Applied {
      changed: vec![Path::new("remote_change.txt").to_path_buf()],
      deleted: Vec::new()
    });
    let events = Arc::new(TestEventHandler::new());

    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (_engine, _handle) = SyncEngine::start(SyncConfig::default(), backend.clone(), events_dyn);

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Polling continues (multiple refresh calls)
    assert!(
      backend.refresh_call_count() >= 2,
      "polling should continue after applied refresh"
    );

    // on_sync("updated") should have been called
    let sync_events = events.sync_calls.lock().expect("lock");
    assert!(
      sync_events.contains(&"updated".to_string()),
      "on_sync(\"updated\") should be called"
    );
  })
  .await;
}

/// 7. Go offline → write 10 files → recover → all 10 synced.
#[tokio::test]
async fn test_extended_offline_many_writes() {
  eprintln!("[TEST] test_extended_offline_many_writes");
  with_timeout("test_extended_offline_many_writes", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(3600);

    // Start offline
    backend.set_sync_result(SyncResult::Offline);

    // Write 10 files
    for i in 0..10 {
      let filename = format!("offline_{i}.txt");
      let path = Path::new(&filename);
      let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("content {i}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      vfs.release(path, fh).await.expect("release");
    }

    wait_processing().await;

    // Files should be in dirty set (Offline returns them)
    assert!(backend.sync_call_count() >= 1, "sync should have been called");
    assert_eq!(events.push_count(), 0, "no push while offline");

    // Recover
    backend.set_sync_result(SyncResult::Success { synced_files: 10 });

    // Shutdown triggers final sync
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Verify all 10 files were synced
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();
    for i in 0..10 {
      let filename = format!("offline_{i}.txt");
      assert!(
        all_synced.iter().any(|p| p.ends_with(&filename)),
        "{filename} should be synced after recovery"
      );
    }

    assert!(events.push_count() > 0, "on_push should be called after recovery");
  })
  .await;
}

/// 8. Write 1MB file through VFS → flush → release → sync succeeds → file content verified.
#[tokio::test]
async fn test_large_file_write_1mb() {
  eprintln!("[TEST] test_large_file_write_1mb");
  with_timeout("test_large_file_write_1mb", async {
    let (vfs, engine, handle, backend, _events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Success { synced_files: 1 });

    // Generate 1MB of data
    let size = 1024 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let path = Path::new("large_1mb.bin");
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");

    // Write in chunks
    let chunk_size = 65536; // 64KB chunks
    let mut offset = 0u64;
    while offset < size as u64 {
      let end = (offset as usize + chunk_size).min(size);
      let written = vfs
        .write(path, fh, offset, &content[offset as usize..end])
        .await
        .expect("write");
      offset += written as u64;
    }

    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    wait_processing().await;

    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Verify file on disk
    let full_path = tmp.path().join("large_1mb.bin");
    assert!(full_path.exists(), "large file should exist on disk");

    let disk_content = std::fs::read(&full_path).expect("read large file");
    assert_eq!(disk_content.len(), size, "file size should be 1MB");
    assert_eq!(disk_content, content, "file content should match");

    // Sync should have been called
    assert!(backend.sync_call_count() > 0, "sync should have been called");
  })
  .await;
}

/// 9. Write 1MB → read at offset 500KB, size 1KB → correct data returned.
#[tokio::test]
async fn test_large_file_partial_read() {
  eprintln!("[TEST] test_large_file_partial_read");
  with_timeout("test_large_file_partial_read", async {
    let (vfs, engine, handle, _backend, _events, _tmp) = create_pipeline(0);

    // Generate 1MB of data
    let size = 1024 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let path = Path::new("partial_read.bin");
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");

    // Write all data
    let mut offset = 0u64;
    let chunk_size = 65536;
    while offset < size as u64 {
      let end = (offset as usize + chunk_size).min(size);
      let written = vfs
        .write(path, fh, offset, &content[offset as usize..end])
        .await
        .expect("write");
      offset += written as u64;
    }
    vfs.flush(path, fh).await.expect("flush");

    // Read at offset 500KB, size 1KB
    let read_offset = 500 * 1024;
    let read_size = 1024;
    let read_data = vfs
      .read(path, fh, read_offset as u64, read_size as u32)
      .await
      .expect("read");

    assert_eq!(read_data.len(), read_size, "partial read should return requested size");
    assert_eq!(
      read_data,
      &content[read_offset..read_offset + read_size],
      "partial read data should match"
    );

    vfs.release(path, fh).await.expect("release");

    safe_shutdown(&engine).await;
    safe_join(handle).await;
  })
  .await;
}

/// 10. One task writes 1MB, another reads concurrently → no panic, no corruption.
#[tokio::test]
async fn test_concurrent_large_file_read_write() {
  eprintln!("[TEST] test_concurrent_large_file_read_write");
  with_timeout("test_concurrent_large_file_read_write", async {
    let (vfs, engine, handle, _backend, _events, _tmp) = create_pipeline(0);

    let path = Path::new("concurrent_large.bin");
    let (fh, _attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");

    let size = 1024 * 1024; // 1MB
    let content: Arc<Vec<u8>> = Arc::new((0..size).map(|i| (i % 256) as u8).collect());

    let vfs_write = vfs.clone();
    let vfs_read = vfs.clone();
    let path_write = path.to_path_buf();
    let path_read = path.to_path_buf();
    let content_clone = content.clone();

    // Writer task
    let write_task = tokio::spawn(async move {
      let mut offset = 0u64;
      let chunk_size = 65536;
      while offset < size as u64 {
        let end = (offset as usize + chunk_size).min(size);
        vfs_write
          .write(&path_write, fh, offset, &content_clone[offset as usize..end])
          .await
          .expect("write");
        offset += chunk_size as u64;
      }
    });

    // Reader task (reads while writing)
    let read_task = tokio::spawn(async move {
      // Wait a bit for some data to be written
      tokio::time::sleep(Duration::from_millis(10)).await;
      // Read should not panic even during write
      let _data = vfs_read.read(&path_read, fh, 0, 4096).await;
      // We don't assert content since it's being written concurrently
    });

    write_task.await.expect("write task");
    read_task.await.expect("read task");

    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");

    safe_shutdown(&engine).await;
    safe_join(handle).await;
  })
  .await;
}

/// 11. Send Shutdown, then try to send more events → no panic.
#[tokio::test]
async fn test_channel_closed_graceful() {
  eprintln!("[TEST] test_channel_closed_graceful");
  with_timeout("test_channel_closed_graceful", async {
    let backend = Arc::new(MockBackend::new());
    let events = Arc::new(TestEventHandler::new());

    let (engine, handle) = SyncEngine::start(
      SyncConfig::default(),
      backend.clone(),
      events.clone() as Arc<dyn VfsEventHandler>
    );

    // Send some events first
    let tx = engine.sender();
    tx.send(FsEvent::FileModified(Path::new("before.txt").to_path_buf()))
      .await
      .expect("send before");

    // Shutdown
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Try to send more events after shutdown — should not panic
    // (try_send returns Err, but VFS ignores it; here we use the channel directly)
    let tx2 = engine.sender();
    let result = tx2.try_send(FsEvent::FileModified(Path::new("after.txt").to_path_buf()));
    // After shutdown, the channel receiver is dropped, so try_send returns Err
    assert!(result.is_err(), "sending after shutdown should fail gracefully");

    // No panic — test passes
  })
  .await;
}

/// 12. Write tracked .txt and untracked .tmp files → sync only includes tracked files.
#[tokio::test]
async fn test_debounce_with_mixed_tracked_untracked() {
  eprintln!("[TEST] test_debounce_with_mixed_tracked_untracked");
  with_timeout("test_debounce_with_mixed_tracked_untracked", async {
    let backend = Arc::new(MockBackend {
      // Track only .txt files
      track_fn: Arc::new(|path| path.extension().and_then(|e| e.to_str()) == Some("txt")),
      ..MockBackend::new()
    });
    backend.set_sync_result(SyncResult::Success { synced_files: 2 });
    let events = Arc::new(TestEventHandler::new());

    // Long debounce — sync only on shutdown
    let config = SyncConfig {
      debounce_timeout_secs: 3600,
      ..SyncConfig::default()
    };
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let (engine, handle) = SyncEngine::start(config, backend.clone(), events_dyn);

    let tx = engine.sender();

    // Send tracked files
    tx.send(FsEvent::FileModified(Path::new("tracked_1.txt").to_path_buf()))
      .await
      .expect("send tracked 1");
    tx.send(FsEvent::FileModified(Path::new("tracked_2.txt").to_path_buf()))
      .await
      .expect("send tracked 2");

    // Send untracked files
    tx.send(FsEvent::FileModified(Path::new("untracked_1.tmp").to_path_buf()))
      .await
      .expect("send untracked 1");
    tx.send(FsEvent::FileModified(Path::new("untracked_2.log").to_path_buf()))
      .await
      .expect("send untracked 2");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // No sync yet (debounce=3600s)
    assert_eq!(backend.sync_call_count(), 0, "sync should not trigger before debounce");

    // Shutdown triggers final sync
    safe_shutdown(&engine).await;
    safe_join(handle).await;

    // Verify: only .txt files in sync calls
    let calls = backend.sync_calls.lock().expect("lock");
    let all_synced: Vec<_> = calls.iter().flat_map(|batch| batch.iter()).collect();

    // Tracked files should be present
    assert!(
      all_synced.iter().any(|p| p.ends_with("tracked_1.txt")),
      "tracked_1.txt should be synced"
    );
    assert!(
      all_synced.iter().any(|p| p.ends_with("tracked_2.txt")),
      "tracked_2.txt should be synced"
    );

    // Untracked files should NOT be present
    assert!(
      !all_synced.iter().any(|p| p.ends_with(".tmp")),
      ".tmp files should not be synced"
    );
    assert!(
      !all_synced.iter().any(|p| p.ends_with(".log")),
      ".log files should not be synced"
    );
  })
  .await;
}
