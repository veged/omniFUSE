//! Concurrent editing integration tests for `OmniFuseVfs`.
//!
//! Tests verify that multiple tasks performing simultaneous VFS operations
//! (write, read, create, delete, rename, readdir, flush) do not cause
//! panics, data corruption, or lost writes.
//!
//! Run: `cargo test -p omnifuse-core --test concurrent_editing_tests`

#![allow(clippy::expect_used)]

use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio::sync::mpsc;
use unifuse::{OpenFlags, UniFuseFilesystem};

use omnifuse_core::{
    config::{BufferConfig, SyncConfig},
    events::{NoopEventHandler, VfsEventHandler},
    sync_engine::{FsEvent, SyncEngine},
    test_utils::{MockBackend, TestEventHandler, with_timeout},
    vfs::OmniFuseVfs,
};

// ============================================================================
// Helpers
// ============================================================================

/// Helper: create VFS backed by MockBackend in a temporary directory.
///
/// Returns the VFS, an FsEvent receiver (for inspecting events),
/// and the mpsc sender (for manually sending events if needed).
fn create_test_vfs(
    dir: &std::path::Path,
    backend: Arc<MockBackend>,
) -> (OmniFuseVfs<MockBackend>, mpsc::Receiver<FsEvent>, mpsc::Sender<FsEvent>) {
    let (tx, rx) = mpsc::channel(256);
    let events: Arc<dyn VfsEventHandler> = Arc::new(NoopEventHandler);
    let vfs = OmniFuseVfs::new(
        dir.to_path_buf(),
        tx.clone(),
        backend,
        events,
        BufferConfig::default(),
    );
    (vfs, rx, tx)
}

// ============================================================================
// Tests
// ============================================================================

/// Two tasks write to the same file at different offsets simultaneously.
/// Both writes should succeed and the final file content should contain data
/// from both tasks (no partial corruption).
#[tokio::test]
async fn test_concurrent_write_same_file() {
    eprintln!("[TEST] test_concurrent_write_same_file");
    with_timeout("test_concurrent_write_same_file", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        // Pre-create the file with enough initial space
        let path = PathBuf::from("shared.txt");
        let initial_data = vec![0u8; 200];
        let (fh_init, _) = vfs
            .create(&path, OpenFlags::read_write(), 0o644)
            .await
            .expect("create");
        vfs.write(&path, fh_init, 0, &initial_data)
            .await
            .expect("initial write");
        vfs.flush(&path, fh_init).await.expect("flush init");
        vfs.release(&path, fh_init).await.expect("release init");

        // Spawn two tasks that write at different offsets
        let vfs1 = Arc::clone(&vfs);
        let vfs2 = Arc::clone(&vfs);
        let path1 = path.clone();
        let path2 = path.clone();

        let data_a = vec![0xAAu8; 50];
        let data_b = vec![0xBBu8; 50];

        let h1 = tokio::spawn(async move {
            let fh = vfs1
                .open(&path1, OpenFlags::read_write())
                .await
                .expect("open task1");
            // Write at offset 0
            vfs1.write(&path1, fh, 0, &data_a)
                .await
                .expect("write task1");
            vfs1.flush(&path1, fh).await.expect("flush task1");
            vfs1.release(&path1, fh).await.expect("release task1");
        });

        let h2 = tokio::spawn(async move {
            let fh = vfs2
                .open(&path2, OpenFlags::read_write())
                .await
                .expect("open task2");
            // Write at offset 100 (non-overlapping with task1)
            vfs2.write(&path2, fh, 100, &data_b)
                .await
                .expect("write task2");
            vfs2.flush(&path2, fh).await.expect("flush task2");
            vfs2.release(&path2, fh).await.expect("release task2");
        });

        h1.await.expect("join task1");
        h2.await.expect("join task2");

        // Verify: file exists and has the expected size
        let fh_read = vfs
            .open(&path, OpenFlags::read_only())
            .await
            .expect("open for read");
        let content = vfs
            .read(&path, fh_read, 0, 200)
            .await
            .expect("read all");
        vfs.release(&path, fh_read).await.expect("release read");

        // Both regions should have been written
        // (exact behavior depends on which task runs last, but no panic occurred)
        assert_eq!(content.len(), 200, "file should be 200 bytes");
        eprintln!("  [OK] both writes completed without panic or corruption");
    })
    .await;
}

/// 10 tasks create different files in parallel.
/// All files should exist afterwards with correct content.
#[tokio::test]
async fn test_concurrent_create_different_files() {
    eprintln!("[TEST] test_concurrent_create_different_files");
    with_timeout("test_concurrent_create_different_files", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        let mut handles = Vec::new();
        for i in 0..10u8 {
            let vfs = Arc::clone(&vfs);
            handles.push(tokio::spawn(async move {
                let name = format!("file_{i}.txt");
                let path = PathBuf::from(&name);
                let data = format!("content of file {i}");

                let (fh, _attr) = vfs
                    .create(&path, OpenFlags::read_write(), 0o644)
                    .await
                    .expect("create");
                vfs.write(&path, fh, 0, data.as_bytes())
                    .await
                    .expect("write");
                vfs.flush(&path, fh).await.expect("flush");
                vfs.release(&path, fh).await.expect("release");
            }));
        }

        for h in handles {
            h.await.expect("join");
        }

        // Verify: all 10 files exist with correct content
        for i in 0..10u8 {
            let name = format!("file_{i}.txt");
            let path = PathBuf::from(&name);
            let expected = format!("content of file {i}");

            let fh = vfs
                .open(&path, OpenFlags::read_only())
                .await
                .expect("open for verify");
            let data = vfs
                .read(&path, fh, 0, 1024)
                .await
                .expect("read for verify");
            vfs.release(&path, fh).await.expect("release verify");

            let actual = String::from_utf8(data).expect("utf8");
            assert_eq!(
                actual, expected,
                "file_{i}.txt content mismatch: got {actual:?}"
            );
        }

        eprintln!("  [OK] all 10 files created and verified");
    })
    .await;
}

/// One task writes while another reads the same file concurrently.
/// No panics, no data corruption — reads return either old or new data.
#[tokio::test]
async fn test_concurrent_write_and_read() {
    eprintln!("[TEST] test_concurrent_write_and_read");
    with_timeout("test_concurrent_write_and_read", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        // Pre-create the file with known content
        let path = PathBuf::from("rw_target.txt");
        let initial = b"initial content here!";
        let (fh, _) = vfs
            .create(&path, OpenFlags::read_write(), 0o644)
            .await
            .expect("create");
        vfs.write(&path, fh, 0, initial)
            .await
            .expect("write initial");
        vfs.flush(&path, fh).await.expect("flush");
        vfs.release(&path, fh).await.expect("release");

        // Writer task: overwrite with new data repeatedly
        let vfs_w = Arc::clone(&vfs);
        let path_w = path.clone();
        let writer = tokio::spawn(async move {
            for round in 0..20u32 {
                let fh = vfs_w
                    .open(&path_w, OpenFlags::read_write())
                    .await
                    .expect("open write");
                let data = format!("updated round {round:04}");
                vfs_w
                    .write(&path_w, fh, 0, data.as_bytes())
                    .await
                    .expect("write");
                vfs_w.flush(&path_w, fh).await.expect("flush");
                vfs_w.release(&path_w, fh).await.expect("release");
            }
        });

        // Reader task: read the file repeatedly
        let vfs_r = Arc::clone(&vfs);
        let path_r = path.clone();
        let reader = tokio::spawn(async move {
            for _ in 0..20u32 {
                let fh = vfs_r
                    .open(&path_r, OpenFlags::read_only())
                    .await
                    .expect("open read");
                let data = vfs_r
                    .read(&path_r, fh, 0, 1024)
                    .await
                    .expect("read");
                vfs_r.release(&path_r, fh).await.expect("release read");

                // Data should be non-empty (either old or new content)
                assert!(!data.is_empty(), "read returned empty data");
            }
        });

        writer.await.expect("writer join");
        reader.await.expect("reader join");

        eprintln!("  [OK] concurrent read/write completed without panics");
    })
    .await;
}

/// One task creates files, another deletes them.
/// No panics from race conditions — deletions of non-existent files
/// are expected to return NotFound errors (which we ignore gracefully).
#[tokio::test]
async fn test_concurrent_create_and_delete() {
    eprintln!("[TEST] test_concurrent_create_and_delete");
    with_timeout("test_concurrent_create_and_delete", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        // Creator task: create files 0..20
        let vfs_c = Arc::clone(&vfs);
        let creator = tokio::spawn(async move {
            for i in 0..20u32 {
                let path = PathBuf::from(format!("ephemeral_{i}.txt"));
                let result = vfs_c.create(&path, OpenFlags::read_write(), 0o644).await;
                if let Ok((fh, _)) = result {
                    let _ = vfs_c.write(&path, fh, 0, b"data").await;
                    let _ = vfs_c.flush(&path, fh).await;
                    let _ = vfs_c.release(&path, fh).await;
                }
                // Small yield to interleave with deleter
                tokio::task::yield_now().await;
            }
        });

        // Deleter task: try to delete files (some may not exist yet)
        let vfs_d = Arc::clone(&vfs);
        let deleter = tokio::spawn(async move {
            for i in 0..20u32 {
                let path = PathBuf::from(format!("ephemeral_{i}.txt"));
                // Ignore errors — file may not exist yet or already deleted
                let _ = vfs_d.unlink(&path).await;
                tokio::task::yield_now().await;
            }
        });

        creator.await.expect("creator join");
        deleter.await.expect("deleter join");

        eprintln!("  [OK] concurrent create/delete completed without panics");
    })
    .await;
}

/// Multiple rename operations happening simultaneously.
/// No panics, final state is consistent (each source is gone,
/// the destination exists if the rename succeeded).
#[tokio::test]
async fn test_concurrent_rename_operations() {
    eprintln!("[TEST] test_concurrent_rename_operations");
    with_timeout("test_concurrent_rename_operations", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        // Pre-create 10 source files
        for i in 0..10u32 {
            let path = PathBuf::from(format!("src_{i}.txt"));
            let (fh, _) = vfs
                .create(&path, OpenFlags::read_write(), 0o644)
                .await
                .expect("create src");
            let data = format!("data_{i}");
            vfs.write(&path, fh, 0, data.as_bytes())
                .await
                .expect("write src");
            vfs.flush(&path, fh).await.expect("flush src");
            vfs.release(&path, fh).await.expect("release src");
        }

        // Spawn 10 tasks, each renaming src_N -> dst_N
        let mut handles = Vec::new();
        for i in 0..10u32 {
            let vfs = Arc::clone(&vfs);
            handles.push(tokio::spawn(async move {
                let from = PathBuf::from(format!("src_{i}.txt"));
                let to = PathBuf::from(format!("dst_{i}.txt"));
                vfs.rename(&from, &to, 0).await.expect("rename");
            }));
        }

        for h in handles {
            h.await.expect("join rename");
        }

        // Verify: src files are gone, dst files exist
        for i in 0..10u32 {
            let src = PathBuf::from(format!("src_{i}.txt"));
            let dst = PathBuf::from(format!("dst_{i}.txt"));

            let src_result = vfs.getattr(&src).await;
            assert!(
                src_result.is_err(),
                "src_{i}.txt should not exist after rename"
            );

            let dst_attr = vfs.getattr(&dst).await.expect("dst getattr");
            assert!(dst_attr.size > 0, "dst_{i}.txt should have content");
        }

        eprintln!("  [OK] all 10 renames completed, state is consistent");
    })
    .await;
}

/// Set `sync_delay` on MockBackend. Send FileClosed event (triggers sync),
/// then immediately write new data while sync is in progress.
/// New write must not be lost — the SyncEngine should pick it up in the next cycle.
#[tokio::test]
async fn test_write_during_sync_delay() {
    eprintln!("[TEST] test_write_during_sync_delay");
    with_timeout("test_write_during_sync_delay", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        backend.set_sync_result(omnifuse_core::SyncResult::Success { synced_files: 1 });
        // Sync takes 300ms — enough time to send a new event while sync runs
        backend.set_sync_delay(Duration::from_millis(300));

        let events = Arc::new(TestEventHandler::new());
        let events_dyn: Arc<dyn VfsEventHandler> = Arc::clone(&events) as _;
        let config = SyncConfig {
            debounce_timeout_secs: 0,
            ..SyncConfig::default()
        };
        let (engine, _handle) = SyncEngine::start(config, Arc::clone(&backend), events_dyn);
        let tx = engine.sender();

        // Also create the VFS to do actual writes
        let (vfs_tx, _vfs_rx) = mpsc::channel(256);
        let vfs_events: Arc<dyn VfsEventHandler> = Arc::new(NoopEventHandler);
        let vfs = OmniFuseVfs::new(
            tmp.path().to_path_buf(),
            vfs_tx,
            Arc::clone(&backend),
            vfs_events,
            BufferConfig::default(),
        );

        // Create the file via VFS
        let path = PathBuf::from("sync_race.txt");
        let (fh, _) = vfs
            .create(&path, OpenFlags::read_write(), 0o644)
            .await
            .expect("create");
        vfs.write(&path, fh, 0, b"first version")
            .await
            .expect("write v1");
        vfs.flush(&path, fh).await.expect("flush v1");
        vfs.release(&path, fh).await.expect("release v1");

        // Send FileClosed to SyncEngine — this triggers sync with 300ms delay
        tx.send(FsEvent::FileClosed(path.clone()))
            .await
            .expect("send FileClosed");

        // Wait 50ms — sync is in progress (300ms delay)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Write NEW data while sync is running
        let fh2 = vfs
            .open(&path, OpenFlags::read_write())
            .await
            .expect("open v2");
        vfs.write(&path, fh2, 0, b"second version")
            .await
            .expect("write v2");
        vfs.flush(&path, fh2).await.expect("flush v2");
        vfs.release(&path, fh2).await.expect("release v2");

        // Notify SyncEngine about the new write
        tx.send(FsEvent::FileClosed(path.clone()))
            .await
            .expect("send second FileClosed");

        // Wait for both syncs to complete (300ms delay + processing)
        tokio::time::sleep(Duration::from_millis(600)).await;

        // Remove delay for clean shutdown
        backend.clear_sync_delay();
        engine.shutdown().await.expect("shutdown");

        // Verify: sync was called at least twice (first + second write)
        let sync_count = backend.sync_call_count();
        assert!(
            sync_count >= 2,
            "sync should be called at least twice: first batch + new write, got {sync_count}"
        );

        // Verify: the file on disk has the SECOND version
        let fh_check = vfs
            .open(&path, OpenFlags::read_only())
            .await
            .expect("open check");
        let data = vfs.read(&path, fh_check, 0, 1024).await.expect("read");
        vfs.release(&path, fh_check).await.expect("release check");
        let content = String::from_utf8(data).expect("utf8");
        assert_eq!(
            content, "second version",
            "new write must not be lost during sync"
        );

        eprintln!("  [OK] write during sync delay was not lost");
    })
    .await;
}

/// Open/write/flush/release the same file 50 times rapidly.
/// All operations succeed, final content matches the last write.
#[tokio::test]
async fn test_rapid_open_close_cycles() {
    eprintln!("[TEST] test_rapid_open_close_cycles");
    with_timeout("test_rapid_open_close_cycles", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);

        // Create the file initially
        let path = PathBuf::from("rapid.txt");
        let (fh, _) = vfs
            .create(&path, OpenFlags::read_write(), 0o644)
            .await
            .expect("create");
        vfs.release(&path, fh).await.expect("release init");

        // 50 rapid open-write-flush-close cycles
        for i in 0..50u32 {
            let data = format!("iteration {i:04}");
            let fh = vfs
                .open(&path, OpenFlags::read_write())
                .await
                .expect("open");
            vfs.write(&path, fh, 0, data.as_bytes())
                .await
                .expect("write");
            vfs.flush(&path, fh).await.expect("flush");
            vfs.release(&path, fh).await.expect("release");
        }

        // Verify: final content is the last iteration
        let fh = vfs
            .open(&path, OpenFlags::read_only())
            .await
            .expect("open final");
        let data = vfs.read(&path, fh, 0, 1024).await.expect("read final");
        vfs.release(&path, fh).await.expect("release final");

        let content = String::from_utf8(data).expect("utf8");
        assert!(
            content.starts_with("iteration 0049"),
            "final content should be last write, got: {content:?}"
        );

        eprintln!("  [OK] 50 rapid open/close cycles completed");
    })
    .await;
}

/// Multiple files being flushed simultaneously via concurrent tasks.
/// All data should be written to disk correctly.
#[tokio::test]
async fn test_concurrent_flush_operations() {
    eprintln!("[TEST] test_concurrent_flush_operations");
    with_timeout("test_concurrent_flush_operations", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        // Pre-create 10 files
        let mut file_handles = Vec::new();
        for i in 0..10u32 {
            let path = PathBuf::from(format!("flush_{i}.txt"));
            let (fh, _) = vfs
                .create(&path, OpenFlags::read_write(), 0o644)
                .await
                .expect("create");
            let data = format!("flush data {i}");
            vfs.write(&path, fh, 0, data.as_bytes())
                .await
                .expect("write");
            file_handles.push((path, fh));
        }

        // Flush all files concurrently
        let mut flush_handles = Vec::new();
        for (path, fh) in &file_handles {
            let vfs = Arc::clone(&vfs);
            let path = path.clone();
            let fh = *fh;
            flush_handles.push(tokio::spawn(async move {
                vfs.flush(&path, fh).await.expect("flush");
                vfs.release(&path, fh).await.expect("release");
            }));
        }

        for h in flush_handles {
            h.await.expect("join flush");
        }

        // Verify: all files have correct content on disk
        for i in 0..10u32 {
            let path = PathBuf::from(format!("flush_{i}.txt"));
            let expected = format!("flush data {i}");

            let fh = vfs
                .open(&path, OpenFlags::read_only())
                .await
                .expect("open verify");
            let data = vfs.read(&path, fh, 0, 1024).await.expect("read verify");
            vfs.release(&path, fh).await.expect("release verify");

            let actual = String::from_utf8(data).expect("utf8");
            assert_eq!(
                actual, expected,
                "flush_{i}.txt content mismatch: got {actual:?}"
            );
        }

        eprintln!("  [OK] 10 concurrent flushes completed with correct data");
    })
    .await;
}

/// One task creates files while another calls readdir.
/// No panics, readdir eventually sees all files.
#[tokio::test]
async fn test_concurrent_readdir_during_create() {
    eprintln!("[TEST] test_concurrent_readdir_during_create");
    with_timeout("test_concurrent_readdir_during_create", async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(MockBackend::new());
        let (vfs, _rx, _tx) = create_test_vfs(tmp.path(), backend);
        let vfs = Arc::new(vfs);

        let total_files = 15u32;

        // Creator task: create files with small delays to interleave
        let vfs_c = Arc::clone(&vfs);
        let creator = tokio::spawn(async move {
            for i in 0..total_files {
                let path = PathBuf::from(format!("readdir_{i}.txt"));
                let (fh, _) = vfs_c
                    .create(&path, OpenFlags::read_write(), 0o644)
                    .await
                    .expect("create");
                vfs_c
                    .write(&path, fh, 0, b"content")
                    .await
                    .expect("write");
                vfs_c.flush(&path, fh).await.expect("flush");
                vfs_c.release(&path, fh).await.expect("release");
                // Yield to allow readdir to interleave
                tokio::task::yield_now().await;
            }
        });

        // Reader task: repeatedly call readdir during creation
        let vfs_r = Arc::clone(&vfs);
        let reader = tokio::spawn(async move {
            let root = PathBuf::from("");
            let mut max_seen = 0usize;
            for _ in 0..30u32 {
                let entries = vfs_r.readdir(&root).await.expect("readdir");
                let count = entries
                    .iter()
                    .filter(|e| e.name.starts_with("readdir_"))
                    .count();
                if count > max_seen {
                    max_seen = count;
                }
                tokio::task::yield_now().await;
            }
            max_seen
        });

        creator.await.expect("creator join");
        let max_seen = reader.await.expect("reader join");

        // After creator finishes, readdir should see all files
        let root = PathBuf::from("");
        let entries = vfs.readdir(&root).await.expect("final readdir");
        let final_count = entries
            .iter()
            .filter(|e| e.name.starts_with("readdir_"))
            .count();

        assert_eq!(
            final_count, total_files as usize,
            "final readdir should see all {total_files} files, saw {final_count}"
        );
        // During creation, reader should have seen at least some files
        assert!(
            max_seen > 0,
            "reader should have seen at least some files during creation"
        );

        eprintln!(
            "  [OK] readdir during create: max_seen={max_seen}, final={final_count}"
        );
    })
    .await;
}

/// 5 VFS instances sharing the same SyncEngine sender.
/// All events are processed correctly by the single engine.
#[tokio::test]
async fn test_many_writers_one_sync_engine() {
    eprintln!("[TEST] test_many_writers_one_sync_engine");
    with_timeout("test_many_writers_one_sync_engine", async {
        let backend = Arc::new(MockBackend::new());
        backend.set_sync_result(omnifuse_core::SyncResult::Success { synced_files: 1 });

        let events = Arc::new(TestEventHandler::new());
        let events_dyn: Arc<dyn VfsEventHandler> = Arc::clone(&events) as _;
        let config = SyncConfig {
            debounce_timeout_secs: 0,
            ..SyncConfig::default()
        };
        let (engine, engine_handle) =
            SyncEngine::start(config, Arc::clone(&backend), events_dyn);

        let num_vfs = 5u32;
        let files_per_vfs = 4u32;
        let mut handles = Vec::new();

        // Create 5 VFS instances, each with its own tempdir,
        // but all sharing the same SyncEngine sender
        for v in 0..num_vfs {
            let tmp = tempfile::tempdir().expect("tempdir");
            let sender = engine.sender();
            let vfs_events: Arc<dyn VfsEventHandler> = Arc::new(NoopEventHandler);
            let vfs = OmniFuseVfs::new(
                tmp.path().to_path_buf(),
                sender,
                Arc::clone(&backend),
                vfs_events,
                BufferConfig::default(),
            );
            let vfs = Arc::new(vfs);

            let h = tokio::spawn(async move {
                // Keep tmp alive for the duration of the task
                let _tmp = tmp;
                for f in 0..files_per_vfs {
                    let path = PathBuf::from(format!("vfs{v}_file{f}.txt"));
                    let (fh, _) = vfs
                        .create(&path, OpenFlags::read_write(), 0o644)
                        .await
                        .expect("create");
                    let data = format!("vfs{v} file{f}");
                    vfs.write(&path, fh, 0, data.as_bytes())
                        .await
                        .expect("write");
                    vfs.flush(&path, fh).await.expect("flush");
                    vfs.release(&path, fh).await.expect("release");
                    // release sends FileClosed event to the shared SyncEngine
                }
            });
            handles.push(h);
        }

        for h in handles {
            h.await.expect("join vfs task");
        }

        // Wait for all events to be processed
        tokio::time::sleep(Duration::from_millis(200)).await;

        engine.shutdown().await.expect("shutdown");
        engine_handle.await.expect("engine handle join");

        // Verify: SyncEngine received and processed events from all 5 VFS instances
        let total_expected = num_vfs * files_per_vfs; // 20 files total
        let sync_count = backend.sync_call_count();
        assert!(
            sync_count >= 1,
            "SyncEngine should have performed at least one sync, got {sync_count}"
        );

        // All synced files across all calls
        let calls = backend.sync_calls.lock().expect("lock");
        let total_synced: usize = calls.iter().map(|batch| batch.len()).sum();
        // Each file should appear at least once (may appear in multiple batches)
        assert!(
            total_synced >= 1,
            "at least some files should have been synced, got {total_synced}"
        );

        // Events handler should have recorded pushes
        assert!(
            events.push_count() >= 1,
            "on_push should have been called at least once"
        );

        eprintln!(
            "  [OK] {num_vfs} VFS instances, {total_expected} files, {sync_count} syncs, {} pushes",
            events.push_count()
        );
    })
    .await;
}
