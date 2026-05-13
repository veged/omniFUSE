//! Honest user-scenario integration tests for OmniFuse.
//!
//! These tests exercise the full VFS + SyncEngine + MockBackend pipeline
//! from a user's perspective: create/edit/delete/rename files through VFS
//! and verify the backend receives correct sync events.
//!
//! In CI/CD with `OMNIFUSE_FUSE_TESTS=0`, these tests are skipped.
//!
//! Run: `cargo test -p omnifuse-core --test real_fuse_tests`

// Scenario tests keep historical helper names and tuple fixtures.
#![allow(
  clippy::doc_markdown,
  clippy::expect_used,
  clippy::type_complexity,
  dead_code,
  unused_variables
)]

use std::{
  path::{Path, PathBuf},
  sync::Arc,
  time::Duration
};

use omnifuse_core::{
  Level, Sink,
  backend::SyncResult,
  config::{BufferConfig, SyncConfig},
  sync_engine::SyncEngine,
  test_utils::{MockBackend, TEST_TIMEOUT, TestEventHandler, with_timeout},
  vfs::OmniFuseVfs
};
use unifuse::{FileType, OpenFlags, UniFuseFilesystem};

fn fuse_tests_disabled() -> bool {
  std::env::var("OMNIFUSE_FUSE_TESTS").as_deref() == Ok("0")
}

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

async fn safe_shutdown(engine: &SyncEngine) {
  tokio::time::timeout(TEST_TIMEOUT, engine.shutdown())
    .await
    .expect("shutdown timed out")
    .expect("shutdown failed");
}

async fn safe_join(handle: tokio::task::JoinHandle<()>) {
  tokio::time::timeout(TEST_TIMEOUT, handle)
    .await
    .expect("join timed out")
    .expect("join failed");
}

async fn wait_processing() {
  tokio::time::sleep(Duration::from_millis(200)).await;
}

// ============================================================================
// HONEST USER SCENARIOS
// ============================================================================

#[tokio::test]
async fn test_honest_editor_open_read_write_save() {
  with_timeout("test_honest_editor_open_read_write_save", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    let path = Path::new("document.md");
    let (fh, attr) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    assert_eq!(attr.kind, FileType::RegularFile);
    vfs
      .write(path, fh, 0, b"# My Document\n\nContent here.")
      .await
      .expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(backend.sync_call_count() > 0, "sync should be called after save");
    assert!(events.push_count() > 0, "push should be triggered");
  })
  .await;
}

#[tokio::test]
async fn test_honest_open_read_no_modify_close() {
  with_timeout("test_honest_open_read_no_modify_close", async {
    let (vfs, engine, handle, _backend, _events, tmp) = create_pipeline(0);
    let file_path = tmp.path().join("readme.md");
    std::fs::write(&file_path, "# README").expect("write");
    let path = Path::new("readme.md");
    let fh = vfs.open(path, OpenFlags::read_only()).await.expect("open");
    let data = vfs.read(path, fh, 0, 100).await.expect("read");
    assert_eq!(data, b"# README");
    vfs.release(path, fh).await.expect("release");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
  })
  .await;
}

#[tokio::test]
async fn test_honest_multi_file_edit_session() {
  with_timeout("test_honest_multi_file_edit_session", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    for i in 0..5 {
      let path_str = format!("file_{i}.md");
      let path = Path::new(&path_str);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("# File {i}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      vfs.release(path, fh).await.expect("release");
    }
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(backend.sync_call_count() >= 1, "sync should be called");
  })
  .await;
}

#[tokio::test]
async fn test_honest_rename_file() {
  with_timeout("test_honest_rename_file", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    let old_path = Path::new("old_name.md");
    let (fh, _) = vfs
      .create(old_path, OpenFlags::read_write(), 0o644)
      .await
      .expect("create");
    vfs.write(old_path, fh, 0, b"# Content").await.expect("write");
    vfs.flush(old_path, fh).await.expect("flush");
    vfs.release(old_path, fh).await.expect("release");
    let new_path = Path::new("new_name.md");
    vfs.rename(old_path, new_path, 0).await.expect("rename");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(
      events
        .renamed_calls
        .lock()
        .expect("lock")
        .iter()
        .any(|(old, new)| old == old_path && new == new_path),
      "rename event should be recorded"
    );
    assert!(backend.sync_call_count() > 0, "sync should be called after rename");
  })
  .await;
}

#[tokio::test]
async fn test_honest_delete_file() {
  with_timeout("test_honest_delete_file", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    let path = Path::new("to_delete.md");
    std::fs::write(tmp.path().join("to_delete.md"), "# Delete me").expect("write");
    let fh = vfs.open(path, OpenFlags::read_only()).await.expect("open");
    vfs.release(path, fh).await.expect("release");
    vfs.unlink(path).await.expect("unlink");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(
      !tmp.path().join("to_delete.md").exists(),
      "file should be deleted from disk"
    );
    assert!(
      events.deleted_calls.lock().expect("lock").contains(&path.to_path_buf()),
      "delete event should be recorded"
    );
  })
  .await;
}

#[tokio::test]
async fn test_honest_large_file_write() {
  with_timeout("test_honest_large_file_write", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    let path = Path::new("large.bin");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    let chunk = vec![0xABu8; 64 * 1024];
    let mut offset = 0u64;
    for _ in 0..160 {
      vfs.write(path, fh, offset, &chunk).await.expect("write chunk");
      offset += chunk.len() as u64;
    }
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(backend.sync_call_count() > 0, "sync should be called for large file");
  })
  .await;
}

#[tokio::test]
async fn test_honest_concurrent_different_files() {
  with_timeout("test_honest_concurrent_different_files", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    let mut handles = Vec::new();
    for i in 0..5 {
      let vfs = vfs.clone();
      handles.push(tokio::spawn(async move {
        let path_str = format!("user_{i}_doc.md");
        let path = Path::new(&path_str);
        let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
        vfs
          .write(path, fh, 0, format!("# Document by user {i}").as_bytes())
          .await
          .expect("write");
        vfs.flush(path, fh).await.expect("flush");
        vfs.release(path, fh).await.expect("release");
      }));
    }
    for h in handles {
      h.await.expect("user thread panicked");
    }
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(backend.sync_call_count() >= 1, "sync should be called");
  })
  .await;
}

#[tokio::test]
async fn test_honest_conflict_detection_and_preservation() {
  with_timeout("test_honest_conflict_detection_and_preservation", async {
    let (vfs, engine, handle, backend, events, tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Conflict {
      synced_files: 0,
      conflict_files: vec![PathBuf::from("conflict.md")]
    });
    let path = Path::new("conflict.md");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"# My local changes").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(
      tmp.path().join("conflict.md").exists(),
      "local file should be preserved after conflict"
    );
    assert!(events.log_count(Level::Warn) > 0, "conflict should log a warning");
  })
  .await;
}

#[tokio::test]
async fn test_honest_offline_work_then_recovery() {
  with_timeout("test_honest_offline_work_then_recovery", async {
    let (vfs, engine, handle, backend, events, _tmp) = create_pipeline(0);
    backend.set_sync_result(SyncResult::Offline);
    for i in 0..3 {
      let path_str = format!("offline_{i}.md");
      let path = Path::new(&path_str);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("# Written offline {i}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      vfs.release(path, fh).await.expect("release");
    }
    wait_processing().await;
    backend.set_sync_result(SyncResult::Success { synced_files: 3 });
    let path = Path::new("recovery.md");
    let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
    vfs.write(path, fh, 0, b"# Back online").await.expect("write");
    vfs.flush(path, fh).await.expect("flush");
    vfs.release(path, fh).await.expect("release");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(events.push_count() > 0, "push should happen after recovery");
  })
  .await;
}

#[tokio::test]
async fn test_honest_nested_directory_workflow() {
  with_timeout("test_honest_nested_directory_workflow", async {
    let (vfs, engine, handle, backend, _events, _tmp) = create_pipeline(0);
    vfs.mkdir(Path::new("src"), 0o755).await.expect("mkdir src");
    vfs
      .mkdir(Path::new("src/components"), 0o755)
      .await
      .expect("mkdir components");
    vfs
      .mkdir(Path::new("src/components/ui"), 0o755)
      .await
      .expect("mkdir ui");
    let paths = ["src/main.md", "src/components/index.md", "src/components/ui/button.md"];
    for path_str in &paths {
      let path = Path::new(path_str);
      let (fh, _) = vfs.create(path, OpenFlags::read_write(), 0o644).await.expect("create");
      vfs
        .write(path, fh, 0, format!("# {path_str}").as_bytes())
        .await
        .expect("write");
      vfs.flush(path, fh).await.expect("flush");
      vfs.release(path, fh).await.expect("release");
    }
    let entries = vfs.readdir(Path::new("src")).await.expect("readdir src");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"main.md"), "main.md should be in src/");
    assert!(names.contains(&"components"), "components/ should be in src/");
    wait_processing().await;
    safe_shutdown(&engine).await;
    safe_join(handle).await;
    assert!(backend.sync_call_count() > 0, "sync should be called");
  })
  .await;
}

#[tokio::test]
async fn test_honest_statfs_returns_valid() {
  with_timeout("test_honest_statfs_returns_valid", async {
    let (vfs, engine, handle, _backend, _events, _tmp) = create_pipeline(0);
    let stat = vfs.statfs(Path::new("")).await.expect("statfs");
    assert!(stat.blocks > 0, "blocks should be > 0");
    assert!(stat.bsize > 0, "bsize should be > 0");
    assert!(stat.bfree > 0, "bfree should be > 0");
    safe_shutdown(&engine).await;
    safe_join(handle).await;
  })
  .await;
}
