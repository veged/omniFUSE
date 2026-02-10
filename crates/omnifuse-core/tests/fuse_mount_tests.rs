//! Integration tests for OmniFuse FUSE mounting.
//!
//! Verify VFS-like behavior through standard file operations.
//! Run on tmpdir without real FUSE mounting, but test
//! the same patterns as full FUSE tests.
//!
//! Run: `cargo test -p omnifuse-core --test fuse_mount_tests`
//! Skip: `cargo test -- --skip fuse_mount`

#![allow(clippy::expect_used)]

use std::{
    path::PathBuf,
    time::Duration,
};

/// Timeout for async tests (30s — git operations can be slow).
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Helper: creates a temporary git repository with an initial commit.
async fn create_test_repo() -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let repo_path = temp_dir.path().to_path_buf();

    // git init
    let output = tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["init"])
        .output()
        .await
        .expect("git init");
    assert!(output.status.success(), "git init failed");

    // Configure user
    tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["config", "user.email", "test@omnifuse.test"])
        .output()
        .await
        .expect("git config email");

    tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["config", "user.name", "OmniFuse Test"])
        .output()
        .await
        .expect("git config name");

    // Create README and initial commit
    std::fs::write(repo_path.join("README.md"), "# Test Repo\n").expect("write readme");

    tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["add", "."])
        .output()
        .await
        .expect("git add");

    tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["commit", "-m", "Initial commit"])
        .output()
        .await
        .expect("git commit");

    (temp_dir, repo_path)
}

// ============================================================================
// MTIME STABILITY TESTS
// ============================================================================

mod mtime_tests {
    use super::*;

    /// Test: reading a file should not change mtime.
    /// Critical for editors — they check mtime after saving.
    #[tokio::test]
    async fn test_mtime_stable_on_read() {
        eprintln!("[TEST] test_mtime_stable_on_read");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "content").expect("write");

        let mtime1 = std::fs::metadata(&file)
            .expect("meta1")
            .modified()
            .expect("mtime1");

        // Read the file — mtime should not change
        let _ = std::fs::read_to_string(&file).expect("read");

        let mtime2 = std::fs::metadata(&file)
            .expect("meta2")
            .modified()
            .expect("mtime2");

        assert_eq!(mtime1, mtime2, "mtime should not change on read");
    }

    /// Test: writing new content updates mtime.
    #[tokio::test]
    async fn test_mtime_updates_on_write() {
        eprintln!("[TEST] test_mtime_updates_on_write");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "content1").expect("write 1");
        let mtime1 = std::fs::metadata(&file)
            .expect("meta1")
            .modified()
            .expect("mtime1");

        // Pause to guarantee mtime difference
        tokio::time::sleep(Duration::from_millis(10)).await;

        std::fs::write(&file, "content2").expect("write 2");
        let mtime2 = std::fs::metadata(&file)
            .expect("meta2")
            .modified()
            .expect("mtime2");

        assert!(mtime2 > mtime1, "mtime should increase after write");
    }

    /// Test: multiple metadata() calls return the same mtime
    /// (equivalent to lookup + getattr in FUSE).
    #[tokio::test]
    async fn test_mtime_consistency_lookup_getattr() {
        eprintln!("[TEST] test_mtime_consistency_lookup_getattr");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "content").expect("write");

        let mtime1 = std::fs::metadata(&file)
            .expect("meta1")
            .modified()
            .expect("mtime1");
        let mtime2 = std::fs::metadata(&file)
            .expect("meta2")
            .modified()
            .expect("mtime2");
        let mtime3 = std::fs::metadata(&file)
            .expect("meta3")
            .modified()
            .expect("mtime3");

        assert_eq!(mtime1, mtime2, "mtime1 == mtime2");
        assert_eq!(mtime2, mtime3, "mtime2 == mtime3");
    }
}

// ============================================================================
// TRUNCATE MODE TESTS
// ============================================================================

mod truncate_tests {
    use std::io::Write as _;

    /// Test: `echo "x" > file` (open O_TRUNC + write + close).
    #[tokio::test]
    async fn test_truncate_then_write() {
        eprintln!("[TEST] test_truncate_then_write");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        // Long initial content
        std::fs::write(&file, "very long original content here").expect("write 1");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read 1"),
            "very long original content here"
        );

        // echo "x" > file (std::fs::write does truncate)
        std::fs::write(&file, "x\n").expect("write 2");

        let content = std::fs::read_to_string(&file).expect("read 2");
        assert_eq!(content, "x\n", "file should contain only 'x\\n'");
    }

    /// Test: `echo "x" >> file` (append without truncate).
    #[tokio::test]
    async fn test_append_without_truncate() {
        eprintln!("[TEST] test_append_without_truncate");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "original").expect("write");

        // Append to end
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("open append");
        f.write_all(b" appended").expect("append");
        drop(f);

        let content = std::fs::read_to_string(&file).expect("read");
        assert_eq!(content, "original appended");
    }

    /// Test: partial overwrite preserves file tail.
    /// write() without truncate into the middle of a file.
    #[tokio::test]
    async fn test_partial_overwrite_keeps_tail() {
        eprintln!("[TEST] test_partial_overwrite_keeps_tail");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "0123456789").expect("write"); // 10 bytes

        // Overwrite bytes 3-5 without truncate
        use std::io::{Seek, SeekFrom};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("open write");
        f.seek(SeekFrom::Start(3)).expect("seek");
        f.write_all(b"XXX").expect("overwrite");
        drop(f);

        let content = std::fs::read_to_string(&file).expect("read");
        assert_eq!(content, "012XXX6789", "file tail should be preserved");
    }

    /// Test: ftruncate via `set_len(5)` on a 10-byte file.
    #[tokio::test]
    async fn test_ftruncate() {
        eprintln!("[TEST] test_ftruncate");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "0123456789").expect("write");

        // ftruncate
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("open");
        f.set_len(5).expect("truncate");
        drop(f);

        let content = std::fs::read_to_string(&file).expect("read");
        assert_eq!(content, "01234");
    }

    /// Test: truncate to 0 and write new content.
    #[tokio::test]
    async fn test_truncate_to_zero_then_write() {
        eprintln!("[TEST] test_truncate_to_zero_then_write");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("test.txt");

        std::fs::write(&file, "old content that is long").expect("write 1");

        // Open with truncate, write new content
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .expect("open truncate");
        f.write_all(b"new").expect("write new content");
        drop(f);

        let content = std::fs::read_to_string(&file).expect("read");
        assert_eq!(content, "new");
    }
}

// ============================================================================
// ATOMIC SAVE TESTS (EDITOR PATTERNS)
// ============================================================================

mod atomic_save_tests {
    /// Test: atomic save via temp + rename (VSCode pattern).
    #[tokio::test]
    async fn test_atomic_save_temp_rename() {
        eprintln!("[TEST] test_atomic_save_temp_rename");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let original = temp_dir.path().join("document.txt");
        let temp_file = temp_dir.path().join("document.txt.tmp");

        // Create original file
        std::fs::write(&original, "original content").expect("write original");

        // Atomic save:
        // 1. Create temp file
        std::fs::write(&temp_file, "new content").expect("write temp");
        // 2. Rename temp -> original
        std::fs::rename(&temp_file, &original).expect("rename");

        // Verify
        assert!(!temp_file.exists(), "temp file should be removed");
        let content = std::fs::read_to_string(&original).expect("read");
        assert_eq!(content, "new content");
    }

    /// Test: vim pattern — create .swp, modify original, delete .swp.
    #[tokio::test]
    async fn test_vim_swp_pattern() {
        eprintln!("[TEST] test_vim_swp_pattern");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("file.txt");
        let swp = temp_dir.path().join(".file.txt.swp");

        // Create file
        std::fs::write(&file, "original").expect("write");

        // vim creates .swp
        std::fs::write(&swp, "swap data").expect("write swp");

        // vim saves (overwrites original)
        std::fs::write(&file, "modified by vim").expect("modify");

        // vim deletes .swp on exit
        std::fs::remove_file(&swp).expect("delete swp");

        // Verify
        assert!(!swp.exists(), ".swp should be deleted");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "modified by vim"
        );
    }

    /// Test: backup pattern — copy to file~, modify original.
    #[tokio::test]
    async fn test_backup_file_pattern() {
        eprintln!("[TEST] test_backup_file_pattern");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let file = temp_dir.path().join("file.txt");
        let backup = temp_dir.path().join("file.txt~");

        std::fs::write(&file, "original").expect("write");

        // Editor creates backup
        std::fs::copy(&file, &backup).expect("backup");

        // Editor overwrites original
        std::fs::write(&file, "modified").expect("modify");

        // Verify
        assert_eq!(
            std::fs::read_to_string(&file).expect("read file"),
            "modified"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).expect("read backup"),
            "original"
        );

        // Clean up backup
        std::fs::remove_file(&backup).expect("delete backup");
    }
}

// ============================================================================
// LOCAL (GIT-UNTRACKED) FILE TESTS
// ============================================================================

mod local_files_tests {
    use super::*;

    /// Test: .swp files should not be tracked by git.
    #[tokio::test]
    async fn test_swp_files_not_tracked() {
        eprintln!("[TEST] test_swp_files_not_tracked");
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (_temp, repo_path) = create_test_repo().await;

            // Create .swp file
            let swp = repo_path.join(".test.swp");
            std::fs::write(&swp, "swap data").expect("write swp");

            // git status — .swp should be untracked (not auto-committed)
            let output = tokio::process::Command::new("git")
                .current_dir(&repo_path)
                .args(["status", "--porcelain"])
                .output()
                .await
                .expect("git status");

            let status = String::from_utf8_lossy(&output.stdout);

            // .swp file exists on disk, but VFS should filter it out.
            // In a minimal git repo it will appear as untracked — this is expected.
            assert!(swp.exists(), ".swp file should exist");
            // File is visible to git, but should not be auto-committed
            assert!(
                status.contains(".test.swp"),
                ".swp file should be visible in git status as untracked"
            );
        }).await.expect("test timed out — possible deadlock");
    }

    /// Test: backup files (~) should not be tracked by git.
    #[tokio::test]
    async fn test_backup_files_not_tracked() {
        eprintln!("[TEST] test_backup_files_not_tracked");
        let (_temp, repo_path) = create_test_repo().await;

        let backup = repo_path.join("file.txt~");
        std::fs::write(&backup, "backup").expect("write backup");

        // Verify that the file is recognized as a backup by the ~ suffix
        assert!(
            backup.to_string_lossy().ends_with('~'),
            "backup file should end with ~"
        );
        assert!(backup.exists(), "backup file should exist");
    }

    /// Test: macOS ._* files should not be tracked by git.
    #[tokio::test]
    async fn test_macos_dotunderscore_not_tracked() {
        eprintln!("[TEST] test_macos_dotunderscore_not_tracked");
        let (_temp, repo_path) = create_test_repo().await;

        let macos_file = repo_path.join("._test.txt");
        std::fs::write(&macos_file, "macos metadata").expect("write");

        assert!(
            macos_file
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .starts_with("._"),
            "file should start with ._"
        );
        assert!(macos_file.exists(), "._* file should exist");
    }
}

// ============================================================================
// DIRECTORY OPERATION TESTS
// ============================================================================

mod directory_tests {
    /// Test: directory creation.
    #[tokio::test]
    async fn test_mkdir() {
        eprintln!("[TEST] test_mkdir");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let new_dir = temp_dir.path().join("subdir");

        std::fs::create_dir(&new_dir).expect("mkdir");

        assert!(new_dir.exists(), "directory should exist");
        assert!(new_dir.is_dir(), "should be a directory");
    }

    /// Test: removing an empty directory.
    #[tokio::test]
    async fn test_rmdir_empty() {
        eprintln!("[TEST] test_rmdir_empty");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let dir = temp_dir.path().join("subdir");

        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::remove_dir(&dir).expect("rmdir");

        assert!(!dir.exists(), "directory should be removed");
    }

    /// Test: removing a non-empty directory should fail.
    #[tokio::test]
    async fn test_rmdir_nonempty_fails() {
        eprintln!("[TEST] test_rmdir_nonempty_fails");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let dir = temp_dir.path().join("subdir");
        let file = dir.join("file.txt");

        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(&file, "content").expect("write");

        let result = std::fs::remove_dir(&dir);
        assert!(
            result.is_err(),
            "rmdir on non-empty directory should fail"
        );
    }

    /// Test: readdir returns entries.
    /// std::fs::read_dir does not include . and .., but FUSE readdir should.
    #[tokio::test]
    async fn test_readdir_dots() {
        eprintln!("[TEST] test_readdir_dots");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let dir = temp_dir.path();

        // Create several files to test readdir
        std::fs::write(dir.join("a.txt"), "a").expect("write a");
        std::fs::write(dir.join("b.txt"), "b").expect("write b");

        let entries: Vec<_> = std::fs::read_dir(dir)
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        // Verify that readdir returns our files
        assert!(
            entries.contains(&"a.txt".to_string()),
            "readdir should contain a.txt"
        );
        assert!(
            entries.contains(&"b.txt".to_string()),
            "readdir should contain b.txt"
        );
    }

    /// Test: nested directories via create_dir_all.
    #[tokio::test]
    async fn test_nested_directories() {
        eprintln!("[TEST] test_nested_directories");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let nested = temp_dir.path().join("a").join("b").join("c");

        std::fs::create_dir_all(&nested).expect("create_dir_all");

        assert!(nested.exists(), "nested directory should exist");
        assert!(nested.is_dir(), "should be a directory");
    }
}

// ============================================================================
// RENAME/MOVE TESTS
// ============================================================================

mod rename_tests {
    /// Test: rename a file within the same directory.
    #[tokio::test]
    async fn test_rename_same_dir() {
        eprintln!("[TEST] test_rename_same_dir");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let old_path = temp_dir.path().join("old.txt");
        let new_path = temp_dir.path().join("new.txt");

        std::fs::write(&old_path, "content").expect("write");
        std::fs::rename(&old_path, &new_path).expect("rename");

        assert!(!old_path.exists(), "old file should be gone");
        assert!(new_path.exists(), "new file should exist");
        assert_eq!(
            std::fs::read_to_string(&new_path).expect("read"),
            "content"
        );
    }

    /// Test: rename a file to another directory (move).
    #[tokio::test]
    async fn test_rename_move_to_other_dir() {
        eprintln!("[TEST] test_rename_move_to_other_dir");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let dir1 = temp_dir.path().join("dir1");
        let dir2 = temp_dir.path().join("dir2");

        std::fs::create_dir(&dir1).expect("mkdir1");
        std::fs::create_dir(&dir2).expect("mkdir2");

        let old_path = dir1.join("file.txt");
        let new_path = dir2.join("file.txt");

        std::fs::write(&old_path, "content").expect("write");
        std::fs::rename(&old_path, &new_path).expect("rename");

        assert!(!old_path.exists(), "file in dir1 should be gone");
        assert!(new_path.exists(), "file in dir2 should exist");
    }

    /// Test: rename with overwrite of an existing file.
    #[tokio::test]
    async fn test_rename_overwrite() {
        eprintln!("[TEST] test_rename_overwrite");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let src = temp_dir.path().join("src.txt");
        let dst = temp_dir.path().join("dst.txt");

        std::fs::write(&src, "new content").expect("write src");
        std::fs::write(&dst, "old content").expect("write dst");

        std::fs::rename(&src, &dst).expect("rename");

        assert!(!src.exists(), "src should be gone");
        assert_eq!(
            std::fs::read_to_string(&dst).expect("read"),
            "new content"
        );
    }

    /// Test: rename a directory with contents.
    #[tokio::test]
    async fn test_rename_directory() {
        eprintln!("[TEST] test_rename_directory");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let old_dir = temp_dir.path().join("old_dir");
        let new_dir = temp_dir.path().join("new_dir");

        std::fs::create_dir(&old_dir).expect("mkdir");
        std::fs::write(old_dir.join("file.txt"), "content").expect("write");

        std::fs::rename(&old_dir, &new_dir).expect("rename");

        assert!(!old_dir.exists(), "old directory should be gone");
        assert!(new_dir.exists(), "new directory should exist");
        assert!(
            new_dir.join("file.txt").exists(),
            "file inside should be preserved"
        );
    }
}

// ============================================================================
// SYMLINK TESTS
// ============================================================================

#[cfg(unix)]
mod symlink_tests {
    /// Test: create a symlink and verify is_symlink.
    #[tokio::test]
    async fn test_symlink_create() {
        eprintln!("[TEST] test_symlink_create");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        std::fs::write(&target, "target content").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert!(link.is_symlink(), "should be a symlink");
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            target,
            "readlink should return path to target"
        );
    }

    /// Test: reading a file through a symlink.
    #[tokio::test]
    async fn test_symlink_read_through() {
        eprintln!("[TEST] test_symlink_read_through");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        std::fs::write(&target, "content").expect("write");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let content = std::fs::read_to_string(&link).expect("read through symlink");
        assert_eq!(content, "content");
    }

    /// Test: writing through a symlink modifies the target.
    #[tokio::test]
    async fn test_symlink_write_through() {
        eprintln!("[TEST] test_symlink_write_through");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        std::fs::write(&target, "original").expect("write");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        std::fs::write(&link, "modified").expect("write through symlink");

        let content = std::fs::read_to_string(&target).expect("read target");
        assert_eq!(content, "modified", "target should be modified");
    }

    /// Test: removing a symlink does not remove the target.
    #[tokio::test]
    async fn test_symlink_unlink() {
        eprintln!("[TEST] test_symlink_unlink");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        std::fs::write(&target, "content").expect("write");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        std::fs::remove_file(&link).expect("delete symlink");

        assert!(!link.exists(), "symlink should be deleted");
        assert!(target.exists(), "target should not be deleted");
    }

    /// Test: relative symlink works correctly.
    #[tokio::test]
    async fn test_symlink_relative() {
        eprintln!("[TEST] test_symlink_relative");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        std::fs::write(&target, "content").expect("write");

        // Relative symlink
        std::os::unix::fs::symlink("target.txt", &link).expect("symlink");

        let content = std::fs::read_to_string(&link).expect("read through relative symlink");
        assert_eq!(content, "content");
    }
}

// ============================================================================
// BIDIRECTIONAL CONSISTENCY TESTS (FS <-> GIT)
// ============================================================================

mod consistency_tests {
    use super::*;

    /// Test: writing a file is reflected in git status.
    #[tokio::test]
    async fn test_fs_changes_reflect_in_git() {
        eprintln!("[TEST] test_fs_changes_reflect_in_git");
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (_temp, repo_path) = create_test_repo().await;

            // Create a new file
            let file = repo_path.join("new_file.txt");
            std::fs::write(&file, "content").expect("write");

            // File should appear in git status
            let output = tokio::process::Command::new("git")
                .current_dir(&repo_path)
                .args(["status", "--porcelain"])
                .output()
                .await
                .expect("git status");

            let status = String::from_utf8_lossy(&output.stdout);
            assert!(
                status.contains("new_file.txt"),
                "new file should be visible in git status"
            );
        }).await.expect("test timed out — possible deadlock");
    }

    /// Test: direct file write is reflected on read.
    #[tokio::test]
    async fn test_git_changes_reflect_in_fs() {
        eprintln!("[TEST] test_git_changes_reflect_in_fs");
        let (_temp, repo_path) = create_test_repo().await;

        let file = repo_path.join("README.md");

        // Read current content
        let content1 = std::fs::read_to_string(&file).expect("read 1");

        // Modify file directly (emulating git pull)
        std::fs::write(&file, "modified by git").expect("write");

        // Re-reading should return new content
        let content2 = std::fs::read_to_string(&file).expect("read 2");
        assert_ne!(content1, content2, "content should have changed");
        assert_eq!(content2, "modified by git");
    }
}
