//! High-level git operations.
//!
//! Combines low-level git commands into operations
//! suitable for the VFS sync workflow.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::engine::{FetchResult, GitEngine, MergeResult, PushResult};

/// High-level git operations.
#[derive(Debug)]
pub struct GitOps {
  /// Underlying git engine.
  engine: GitEngine
}

impl GitOps {
  /// Create a new wrapper for git operations.
  ///
  /// # Errors
  ///
  /// Returns an error if the repository cannot be opened.
  pub fn new(repo_path: PathBuf, branch: String) -> anyhow::Result<Self> {
    let engine = GitEngine::new(repo_path, branch)?;
    Ok(Self { engine })
  }

  /// Get the engine.
  #[must_use]
  pub const fn engine(&self) -> &GitEngine {
    &self.engine
  }

  /// Repository path.
  #[must_use]
  pub fn repo_path(&self) -> &Path {
    self.engine.repo_path()
  }

  /// Startup synchronization.
  ///
  /// Fetch + merge.
  ///
  /// # Errors
  ///
  /// Returns an error if sync fails.
  pub async fn startup_sync(&self) -> anyhow::Result<StartupSyncResult> {
    info!("startup sync");

    match self.engine.fetch().await {
      Ok(FetchResult::UpToDate) => {
        debug!("startup sync: already up to date");
        return Ok(StartupSyncResult::UpToDate);
      }
      Ok(FetchResult::Updated { commits }) => {
        debug!(commits, "startup sync: new commits received");
      }
      Err(e) => {
        warn!("startup sync: fetch failed, working offline: {e}");
        return Ok(StartupSyncResult::Offline);
      }
    }

    match self.engine.pull().await {
      Ok(MergeResult::UpToDate) => Ok(StartupSyncResult::UpToDate),
      Ok(MergeResult::FastForward) => {
        info!("startup sync: fast-forward merge");
        Ok(StartupSyncResult::Updated)
      }
      Ok(MergeResult::Merged { commit }) => {
        info!(commit = %commit, "startup sync: merge commit created");
        Ok(StartupSyncResult::Merged)
      }
      Ok(MergeResult::Conflict { files }) => {
        warn!(files = ?files, "startup sync: conflicts detected");
        Ok(StartupSyncResult::Conflicts { files })
      }
      Err(e) => {
        warn!("startup sync: pull failed: {e}");
        Ok(StartupSyncResult::Offline)
      }
    }
  }

  /// Commit specific files.
  ///
  /// # Errors
  ///
  /// Returns an error if the commit fails.
  pub async fn commit_changes(&self, files: &[PathBuf], message: &str) -> anyhow::Result<String> {
    if files.is_empty() {
      anyhow::bail!("no files to commit");
    }

    self.engine.stage(files).await?;
    let hash = self.engine.commit(message).await?;
    info!(hash = %hash, files = files.len(), "commit created");
    Ok(hash)
  }

  /// Auto-commit with timestamp.
  ///
  /// # Errors
  ///
  /// Returns an error if the commit fails.
  pub async fn auto_commit(&self, files: &[PathBuf]) -> anyhow::Result<String> {
    let message = format!(
      "[auto] {} file(s) changed at {}",
      files.len(),
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    self.commit_changes(files, &message).await
  }

  /// Push with retries and auto-pull on rejection.
  ///
  /// # Errors
  ///
  /// Returns an error if push ultimately fails.
  pub async fn push_with_retry(&self, max_retries: u32) -> anyhow::Result<()> {
    let mut retries = 0;

    loop {
      match self.engine.push().await? {
        PushResult::Success => {
          info!("push succeeded");
          return Ok(());
        }
        PushResult::NoRemote => {
          warn!("no remote configured, skipping push");
          return Ok(());
        }
        PushResult::Rejected => {
          if retries >= max_retries {
            anyhow::bail!("push rejected after {max_retries} attempts");
          }

          retries += 1;
          warn!(retries, "push rejected, pulling and retrying");

          match self.engine.pull().await? {
            MergeResult::Conflict { files } => {
              anyhow::bail!("{} file(s) in conflict", files.len());
            }
            _ => {
              tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
          }
        }
      }
    }
  }

  /// Check for remote changes.
  ///
  /// # Errors
  ///
  /// Returns an error if fetch fails.
  pub async fn check_remote(&self) -> anyhow::Result<bool> {
    let local_head = self.engine.get_head_commit().await?;
    self.engine.fetch().await?;
    let remote_head = self.engine.get_remote_head().await?;
    Ok(remote_head.is_some_and(|r| r != local_head))
  }
}

/// Startup sync result.
#[derive(Debug, Clone)]
pub enum StartupSyncResult {
  /// Already up to date.
  UpToDate,
  /// Updated from remote.
  Updated,
  /// Merge commit created.
  Merged,
  /// Conflicts detected.
  Conflicts {
    /// Files with conflicts.
    files: Vec<PathBuf>
  },
  /// Working offline.
  Offline
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;
  use crate::engine::tests::{create_bare_and_two_clones, create_test_repo};

  /// Timeout for async tests (30s — git operations can be slow).
  const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

  #[tokio::test]
  async fn test_commit_changes() {
    eprintln!("[TEST] test_commit_changes");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    let file = path.join("new.txt");
    tokio::fs::write(&file, "data").await.expect("write");

    let hash = ops.commit_changes(&[file], "test commit").await.expect("commit");
    assert!(!hash.is_empty(), "hash should not be empty");
    assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 characters");
  }

  #[tokio::test]
  async fn test_auto_commit_message() {
    eprintln!("[TEST] test_auto_commit_message");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    let file = path.join("auto.txt");
    tokio::fs::write(&file, "auto data").await.expect("write");

    let hash = ops.auto_commit(&[file]).await.expect("auto_commit");
    assert!(!hash.is_empty(), "auto_commit should create a commit");

    // Verify commit message
    let output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["log", "-1", "--format=%s"])
      .output()
      .await
      .expect("git log");
    let message = String::from_utf8_lossy(&output.stdout);
    assert!(
      message.contains("[auto]"),
      "message should contain [auto]: {message}"
    );
    assert!(
      message.contains("1 file(s) changed"),
      "message should indicate file count: {message}"
    );
  }

  #[tokio::test]
  async fn test_push_with_retry_bare() {
    eprintln!("[TEST] test_push_with_retry_bare");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
      let ops = GitOps::new(clone1.clone(), "main".to_string()).expect("ops");

      let file = clone1.join("pushed.txt");
      tokio::fs::write(&file, "push data").await.expect("write");
      ops.commit_changes(&[file], "push test").await.expect("commit");

      ops.push_with_retry(3).await.expect("push_with_retry");
    }).await.expect("test timed out — possible deadlock");
  }

  #[tokio::test]
  async fn test_push_rejected_retry() {
    eprintln!("[TEST] test_push_rejected_retry");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let ops1 = GitOps::new(clone1.clone(), "main".to_string()).expect("ops1");
      let ops2 = GitOps::new(clone2.clone(), "main".to_string()).expect("ops2");

      // clone1 pushes a change to a different file
      let file1 = clone1.join("from_clone1.txt");
      tokio::fs::write(&file1, "data from clone1").await.expect("write");
      ops1.commit_changes(&[file1], "from clone1").await.expect("commit1");
      ops1.push_with_retry(1).await.expect("push1");

      // clone2 commits a change (to a different file, no conflict)
      let file2 = clone2.join("from_clone2.txt");
      tokio::fs::write(&file2, "data from clone2").await.expect("write");
      ops2
        .commit_changes(&[file2], "from clone2")
        .await
        .expect("commit2");

      // push_with_retry should: push — rejected — pull — retry — success
      ops2.push_with_retry(3).await.expect("push_with_retry");

      // Verify both files are present
      assert!(
        clone2.join("from_clone1.txt").exists(),
        "file from clone1 should exist after retry"
      );
    }).await.expect("test timed out — possible deadlock");
  }

  #[tokio::test]
  async fn test_startup_sync_clean() {
    eprintln!("[TEST] test_startup_sync_clean");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
      let ops = GitOps::new(clone1, "main".to_string()).expect("ops");

      let result = ops.startup_sync().await.expect("startup_sync");
      assert!(
        matches!(result, StartupSyncResult::UpToDate),
        "clean repo: {result:?}"
      );
    }).await.expect("test timed out — possible deadlock");
  }

  /// Full commit_changes flow: create file — commit_changes — git log shows the commit.
  #[tokio::test]
  async fn test_commit_changes_stages_and_commits() {
    eprintln!("[TEST] test_commit_changes_stages_and_commits");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    // Remember HEAD before the commit
    let head_before = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["rev-parse", "HEAD"])
      .output()
      .await
      .expect("rev-parse before");
    let head_before = String::from_utf8_lossy(&head_before.stdout).trim().to_string();

    // Create a file
    let file = path.join("staged_and_committed.txt");
    tokio::fs::write(&file, "test data").await.expect("write");

    // commit_changes automatically stages and commits
    let msg = "test full cycle stage+commit";
    let hash = ops.commit_changes(&[file], msg).await.expect("commit_changes");

    // Verify hash differs from previous HEAD
    assert_ne!(hash, head_before, "commit hash should differ from previous HEAD");

    // Verify git log
    let output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["log", "-1", "--format=%s"])
      .output()
      .await
      .expect("git log");
    let message = String::from_utf8_lossy(&output.stdout);
    assert!(
      message.contains(msg),
      "git log should contain commit message: {message}"
    );

    // Verify the file is in the commit
    let diff_output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
      .output()
      .await
      .expect("diff-tree");
    let files_in_commit = String::from_utf8_lossy(&diff_output.stdout);
    assert!(
      files_in_commit.contains("staged_and_committed.txt"),
      "staged_and_committed.txt should be in the commit: {files_in_commit}"
    );
  }

  /// auto_commit format: "[auto] N file(s) changed at YYYY-MM-DD HH:MM:SS".
  /// Verify the count is correct for multiple files.
  #[tokio::test]
  async fn test_auto_commit_message_format() {
    eprintln!("[TEST] test_auto_commit_message_format");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    // Create 3 files
    let files: Vec<PathBuf> = (1..=3)
      .map(|i| {
        let p = path.join(format!("file_{i}.txt"));
        std::fs::write(&p, format!("content {i}")).expect("write");
        p
      })
      .collect();

    let hash = ops.auto_commit(&files).await.expect("auto_commit");
    assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 characters");

    // Read commit message via git log
    let output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["log", "-1", "--format=%s"])
      .output()
      .await
      .expect("git log");
    let message = String::from_utf8_lossy(&output.stdout);
    let message = message.trim();

    assert!(
      message.contains("[auto]"),
      "message should contain '[auto]': {message}"
    );
    assert!(
      message.contains("3 file(s) changed"),
      "message should contain '3 file(s) changed': {message}"
    );
    // Verify timestamp in YYYY-MM-DD format
    assert!(
      message.contains(&chrono::Local::now().format("%Y-%m-%d").to_string()),
      "message should contain current date: {message}"
    );
  }

  /// commit_changes — git log --oneline contains the commit message.
  #[tokio::test]
  async fn test_git_log_after_commit() {
    eprintln!("[TEST] test_git_log_after_commit");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    let file = path.join("log_test.txt");
    tokio::fs::write(&file, "log data").await.expect("write");

    let commit_msg = "test message for git log";
    let hash = ops.commit_changes(&[file], commit_msg).await.expect("commit");

    // git log --oneline should contain the message
    let output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["log", "--oneline", "-5"])
      .output()
      .await
      .expect("git log --oneline");
    let log_output = String::from_utf8_lossy(&output.stdout);

    assert!(
      log_output.contains(commit_msg),
      "git log --oneline should contain commit message: {log_output}"
    );
    // Verify the short hash from the log is the beginning of the full hash
    let short_hash = &hash[..7];
    assert!(
      log_output.contains(short_hash),
      "git log --oneline should contain short hash {short_hash}: {log_output}"
    );
  }

  /// Sync with an empty file list does not panic but returns an error.
  #[tokio::test]
  async fn test_sync_empty_changeset() {
    eprintln!("[TEST] test_sync_empty_changeset");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path, "main".to_string()).expect("ops");

    // Empty file list — commit_changes should return an error
    let result = ops.commit_changes(&[], "empty commit").await;
    assert!(result.is_err(), "commit with empty file list should return an error");
    assert!(
      result.expect_err("err").to_string().contains("no files to commit"),
      "error should contain 'no files to commit'"
    );
  }

  /// Full cycle through bare repo: init — write — stage — commit — push — verify.
  #[tokio::test]
  async fn test_full_cycle_init_write_commit_push() {
    eprintln!("[TEST] test_full_cycle_init_write_commit_push");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, bare_path, clone1, _clone2) = create_bare_and_two_clones().await;
      let ops = GitOps::new(clone1.clone(), "main".to_string()).expect("ops");

      // Create a file
      let file = clone1.join("cycle_test.txt");
      tokio::fs::write(&file, "full cycle").await.expect("write");

      // Commit
      let hash = ops
        .commit_changes(&[file], "full cycle: create file")
        .await
        .expect("commit");
      assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 characters");

      // Push
      ops.push_with_retry(1).await.expect("push");

      // Verify the commit reached bare repo
      let output = tokio::process::Command::new("git")
        .current_dir(&bare_path)
        .args(["log", "--oneline", "-1"])
        .output()
        .await
        .expect("git log in bare");
      let log_line = String::from_utf8_lossy(&output.stdout);
      assert!(
        log_line.contains("full cycle"),
        "commit should be in bare repo: {log_line}"
      );
    }).await.expect("test timed out — possible deadlock");
  }

  /// Bare + clone1. File is added in clone1, commit, push.
  /// Clone2 is created (via new clone). startup_sync — Updated or UpToDate.
  /// File should be present.
  #[tokio::test]
  async fn test_startup_sync_with_remote() {
    eprintln!("[TEST] test_startup_sync_with_remote");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, bare_path, clone1, _clone2_orig) = create_bare_and_two_clones().await;

      // Add a file to clone1, commit, push
      let ops1 = GitOps::new(clone1.clone(), "main".to_string()).expect("ops1");
      let file = clone1.join("synced_file.txt");
      tokio::fs::write(&file, "content for synchronization").await.expect("write");
      ops1.commit_changes(&[file], "add synced_file.txt").await.expect("commit");
      ops1.push_with_retry(1).await.expect("push");

      // Create a new clone (clone3) from bare
      let clone3_path = _tmp.path().join("clone3");
      tokio::process::Command::new("git")
        .args(["clone"])
        .arg(&bare_path)
        .arg(&clone3_path)
        .output()
        .await
        .expect("clone3");

      // Configure git config for clone3
      tokio::process::Command::new("git")
        .current_dir(&clone3_path)
        .args(["config", "user.email", "test3@test.com"])
        .output()
        .await
        .expect("config email");
      tokio::process::Command::new("git")
        .current_dir(&clone3_path)
        .args(["config", "user.name", "Test3"])
        .output()
        .await
        .expect("config name");

      // startup_sync on clone3
      let ops3 = GitOps::new(clone3_path.clone(), "main".to_string()).expect("ops3");
      let result = ops3.startup_sync().await.expect("startup_sync");
      assert!(
        matches!(result, StartupSyncResult::UpToDate | StartupSyncResult::Updated | StartupSyncResult::Merged),
        "startup_sync should be UpToDate/Updated/Merged: {result:?}"
      );

      // Verify the file exists in clone3
      assert!(
        clone3_path.join("synced_file.txt").exists(),
        "synced_file.txt should be in the new clone after startup_sync"
      );
    }).await.expect("test timed out — possible deadlock");
  }

  /// Create 3 files, commit_changes with all three — one commit.
  /// git log shows one entry (after the initial one).
  #[tokio::test]
  async fn test_commit_with_multiple_files() {
    eprintln!("[TEST] test_commit_with_multiple_files");
    let (_tmp, path) = create_test_repo().await;
    let ops = GitOps::new(path.clone(), "main".to_string()).expect("ops");

    // Remember the commit count before
    let before_output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["rev-list", "--count", "HEAD"])
      .output()
      .await
      .expect("rev-list before");
    let before_count: usize = String::from_utf8_lossy(&before_output.stdout)
      .trim()
      .parse()
      .expect("parse count");

    // Create 3 files
    let files: Vec<PathBuf> = (1..=3)
      .map(|i| {
        let p = path.join(format!("multi_{i}.txt"));
        std::fs::write(&p, format!("file content {i}")).expect("write");
        p
      })
      .collect();

    // Commit all three with a single call
    let hash = ops
      .commit_changes(&files, "commit with three files")
      .await
      .expect("commit_changes");
    assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 characters");

    // Verify exactly 1 commit was added
    let after_output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["rev-list", "--count", "HEAD"])
      .output()
      .await
      .expect("rev-list after");
    let after_count: usize = String::from_utf8_lossy(&after_output.stdout)
      .trim()
      .parse()
      .expect("parse count");

    assert_eq!(
      after_count - before_count,
      1,
      "should be exactly 1 new commit, not {}", after_count - before_count
    );

    // Verify the latest commit message
    let log_output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["log", "-1", "--format=%s"])
      .output()
      .await
      .expect("git log");
    let message = String::from_utf8_lossy(&log_output.stdout);
    assert!(
      message.trim().contains("commit with three files"),
      "commit message should match: {message}"
    );

    // Verify all 3 files are in the commit
    let show_output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
      .output()
      .await
      .expect("diff-tree");
    let changed_files = String::from_utf8_lossy(&show_output.stdout);
    for i in 1..=3 {
      assert!(
        changed_files.contains(&format!("multi_{i}.txt")),
        "multi_{i}.txt should be in the commit: {changed_files}"
      );
    }
  }
}
