//! Git engine — wrapper over git CLI.
//!
//! Ported from `SimpleGitFS` `core/src/git/engine.rs`.

use std::{
  path::{Path, PathBuf},
  sync::Arc
};

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Fetch result.
#[derive(Debug, Clone)]
pub enum FetchResult {
  /// No changes.
  UpToDate,
  /// New commits received.
  Updated {
    /// Number of commits.
    commits: usize
  }
}

/// Push result.
#[derive(Debug, Clone)]
pub enum PushResult {
  /// Push succeeded.
  Success,
  /// Push rejected (pull required).
  Rejected,
  /// No remote configured.
  NoRemote
}

/// Merge result.
#[derive(Debug, Clone)]
pub enum MergeResult {
  /// Already up to date.
  UpToDate,
  /// Fast-forward merge.
  FastForward,
  /// Merge commit created.
  Merged {
    /// Merge commit hash.
    commit: String
  },
  /// Conflicts detected.
  Conflict {
    /// Files with conflicts.
    files: Vec<PathBuf>
  }
}

/// Git engine for repository operations.
#[derive(Debug)]
pub struct GitEngine {
  /// Repository path.
  repo_path: PathBuf,
  /// Tracked branch.
  branch: String,
  /// Remote name.
  remote: String,
  /// Lock for serializing git operations.
  op_lock: Arc<RwLock<()>>
}

impl GitEngine {
  /// Create a new git engine.
  ///
  /// # Errors
  ///
  /// Returns an error if the path is not a git repository.
  pub fn new(repo_path: PathBuf, branch: String) -> anyhow::Result<Self> {
    let git_dir = repo_path.join(".git");
    if !git_dir.exists() {
      anyhow::bail!("not a git repository: {}", repo_path.display());
    }

    Ok(Self {
      repo_path,
      branch,
      remote: "origin".to_string(),
      op_lock: Arc::new(RwLock::new(()))
    })
  }

  /// Repository path.
  #[must_use]
  pub fn repo_path(&self) -> &Path {
    &self.repo_path
  }

  /// Current branch.
  #[must_use]
  pub fn branch(&self) -> &str {
    &self.branch
  }

  /// Add files to the staging area.
  ///
  /// # Errors
  ///
  /// Returns an error if `git add` fails.
  pub async fn stage(&self, files: &[PathBuf]) -> anyhow::Result<()> {
    let _lock = self.op_lock.write().await;

    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(&self.repo_path).arg("add");

    for file in files {
      if let Ok(rel_path) = file.strip_prefix(&self.repo_path) {
        cmd.arg(rel_path);
      } else {
        cmd.arg(file);
      }
    }

    let output = cmd.output().await?;
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git add failed: {stderr}");
    }

    debug!(files = files.len(), "staged files");
    Ok(())
  }

  /// Create a commit.
  ///
  /// # Errors
  ///
  /// Returns an error if `git commit` fails.
  pub async fn commit(&self, message: &str) -> anyhow::Result<String> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["commit", "-m", message])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      if stderr.contains("nothing to commit") {
        anyhow::bail!("nothing to commit");
      }
      anyhow::bail!("git commit failed: {stderr}");
    }

    let hash = self.get_head_commit().await?;
    info!(hash = %hash, "commit created");
    Ok(hash)
  }

  /// Get the HEAD commit hash.
  ///
  /// # Errors
  ///
  /// Returns an error if `git rev-parse` fails.
  pub async fn get_head_commit(&self) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["rev-parse", "HEAD"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git rev-parse failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Get the remote HEAD commit hash.
  ///
  /// # Errors
  ///
  /// Returns an error if `git rev-parse` fails.
  pub async fn get_remote_head(&self) -> anyhow::Result<Option<String>> {
    let ref_name = format!("{}/{}", self.remote, self.branch);

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["rev-parse", &ref_name])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(None);
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
  }

  /// Fetch from remote.
  ///
  /// # Errors
  ///
  /// Returns an error if fetch fails.
  pub async fn fetch(&self) -> anyhow::Result<FetchResult> {
    let _lock = self.op_lock.write().await;

    let before = self.get_remote_head().await?;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["fetch", &self.remote, &self.branch])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      if stderr.contains("Could not resolve host") || stderr.contains("Connection refused") {
        warn!("fetch failed (network): {}", stderr.trim());
        anyhow::bail!("network unavailable: {}", stderr.trim());
      }
      anyhow::bail!("git fetch failed: {stderr}");
    }

    let after = self.get_remote_head().await?;

    if before == after {
      debug!("fetch: up to date");
      Ok(FetchResult::UpToDate)
    } else {
      info!("fetch: new commits received");
      Ok(FetchResult::Updated { commits: 1 })
    }
  }

  /// Pull from remote (fetch + rebase).
  ///
  /// # Errors
  ///
  /// Returns an error if pull fails.
  pub async fn pull(&self) -> anyhow::Result<MergeResult> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["pull", "--rebase", &self.remote, &self.branch])
      .output()
      .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
      if stderr.contains("CONFLICT")
        || stderr.contains("Automatic merge failed")
        || stdout.contains("CONFLICT")
        || stderr.contains("could not apply")
      {
        // Abort rebase on conflicts
        let _ = tokio::process::Command::new("git")
          .current_dir(&self.repo_path)
          .args(["rebase", "--abort"])
          .output()
          .await;

        let conflict_files = self.get_conflict_files().await.unwrap_or_default();
        warn!(files = ?conflict_files, "pull/rebase: conflicts detected");
        return Ok(MergeResult::Conflict { files: conflict_files });
      }
      anyhow::bail!("git pull failed: {stderr}");
    }

    if stdout.contains("Already up to date") || (stdout.contains("Current branch") && stdout.contains("is up to date"))
    {
      debug!("pull: already up to date");
      Ok(MergeResult::UpToDate)
    } else if stdout.contains("Fast-forward") {
      info!("pull: fast-forward");
      Ok(MergeResult::FastForward)
    } else {
      let hash = self.get_head_commit().await?;
      info!(hash = %hash, "pull: rebased/merged");
      Ok(MergeResult::Merged { commit: hash })
    }
  }

  /// Push to remote.
  ///
  /// # Errors
  ///
  /// Returns an error if push fails.
  pub async fn push(&self) -> anyhow::Result<PushResult> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["push", &self.remote, &self.branch])
      .output()
      .await?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
      if stderr.contains("rejected") || stderr.contains("non-fast-forward") {
        warn!("push rejected: pull required");
        return Ok(PushResult::Rejected);
      }
      if stderr.contains("No configured push destination") {
        return Ok(PushResult::NoRemote);
      }
      anyhow::bail!("git push failed: {stderr}");
    }

    info!("push: success");
    Ok(PushResult::Success)
  }

  /// Get the list of files with conflicts.
  async fn get_conflict_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["diff", "--name-only", "--diff-filter=U"])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(Vec::new());
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|l| self.repo_path.join(l))
      .collect();

    Ok(files)
  }

  /// Check for uncommitted changes.
  ///
  /// # Errors
  ///
  /// Returns an error if `git status` fails.
  pub async fn has_changes(&self) -> anyhow::Result<bool> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["status", "--porcelain"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git status failed: {stderr}");
    }

    Ok(!output.stdout.is_empty())
  }

  /// Get the list of modified files.
  ///
  /// # Errors
  ///
  /// Returns an error if `git status` fails.
  pub async fn modified_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["status", "--porcelain"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git status failed: {stderr}");
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .filter_map(|line| {
        if line.len() > 3 {
          Some(self.repo_path.join(&line[3..]))
        } else {
          None
        }
      })
      .collect();

    Ok(files)
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
  use std::path::PathBuf;

  use super::*;

  /// Timeout for async tests (30s — git operations can be slow).
  const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

  /// Create a test git repository with an initial commit.
  pub(crate) async fn create_test_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().to_path_buf();

    // git init (explicit branch name to avoid dependence on global config)
    tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["init", "-b", "main"])
      .output()
      .await
      .expect("git init");

    // git config (for commits in tests)
    tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["config", "user.email", "test@test.com"])
      .output()
      .await
      .expect("git config email");

    tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["config", "user.name", "Test"])
      .output()
      .await
      .expect("git config name");

    // Initial commit
    tokio::fs::write(path.join("README.md"), "# Test").await.expect("write");
    tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["add", "."])
      .output()
      .await
      .expect("git add");
    tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["commit", "-m", "initial"])
      .output()
      .await
      .expect("git commit");

    (tmp, path)
  }

  /// Create a bare repo + two clones.
  pub(crate) async fn create_bare_and_two_clones() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().to_path_buf();

    let bare_path = base.join("bare.git");
    let clone1_path = base.join("clone1");
    let clone2_path = base.join("clone2");

    // Create bare repo (explicit branch name to avoid dependence on global config)
    tokio::process::Command::new("git")
      .args(["init", "--bare", "-b", "main"])
      .arg(&bare_path)
      .output()
      .await
      .expect("git init --bare");

    // Clone 1
    tokio::process::Command::new("git")
      .args(["clone"])
      .arg(&bare_path)
      .arg(&clone1_path)
      .output()
      .await
      .expect("clone 1");

    // Configure clone 1
    for clone_path in [&clone1_path, &clone2_path] {
      if clone_path == &clone1_path {
        // Initial commit in clone 1
        tokio::process::Command::new("git")
          .current_dir(&clone1_path)
          .args(["config", "user.email", "test@test.com"])
          .output()
          .await
          .expect("config");
        tokio::process::Command::new("git")
          .current_dir(&clone1_path)
          .args(["config", "user.name", "Test"])
          .output()
          .await
          .expect("config");

        tokio::fs::write(clone1_path.join("README.md"), "# Shared")
          .await
          .expect("write");
        tokio::process::Command::new("git")
          .current_dir(&clone1_path)
          .args(["add", "."])
          .output()
          .await
          .expect("add");
        tokio::process::Command::new("git")
          .current_dir(&clone1_path)
          .args(["commit", "-m", "initial"])
          .output()
          .await
          .expect("commit");
        tokio::process::Command::new("git")
          .current_dir(&clone1_path)
          .args(["push", "-u", "origin", "main"])
          .output()
          .await
          .expect("push");
      }
    }

    // Clone 2
    tokio::process::Command::new("git")
      .args(["clone"])
      .arg(&bare_path)
      .arg(&clone2_path)
      .output()
      .await
      .expect("clone 2");

    // Configure clone 2
    tokio::process::Command::new("git")
      .current_dir(&clone2_path)
      .args(["config", "user.email", "test2@test.com"])
      .output()
      .await
      .expect("config");
    tokio::process::Command::new("git")
      .current_dir(&clone2_path)
      .args(["config", "user.name", "Test2"])
      .output()
      .await
      .expect("config");

    (tmp, bare_path, clone1_path, clone2_path)
  }

  #[tokio::test]
  async fn test_new_valid_repo() {
    eprintln!("[TEST] test_new_valid_repo");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string());
    assert!(engine.is_ok(), "should open a valid repo");
    assert_eq!(engine.expect("engine").repo_path(), &path);
  }

  #[tokio::test]
  async fn test_new_invalid_path() {
    eprintln!("[TEST] test_new_invalid_path");
    let tmp = tempfile::tempdir().expect("tempdir");
    let result = GitEngine::new(tmp.path().to_path_buf(), "main".to_string());
    assert!(result.is_err(), "non-git directory should return an error");
  }

  #[tokio::test]
  async fn test_stage_files() {
    eprintln!("[TEST] test_stage_files");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string()).expect("engine");

    // Create a new file and stage it
    let new_file = path.join("added.txt");
    tokio::fs::write(&new_file, "new content").await.expect("write");

    engine.stage(&[new_file]).await.expect("stage");

    // Verify via git status
    let output = tokio::process::Command::new("git")
      .current_dir(&path)
      .args(["status", "--porcelain"])
      .output()
      .await
      .expect("status");
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(status.contains("A  added.txt"), "file should be staged: {status}");
  }

  #[tokio::test]
  async fn test_commit_creates_commit() {
    eprintln!("[TEST] test_commit_creates_commit");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string()).expect("engine");

    let before = engine.get_head_commit().await.expect("head");

    tokio::fs::write(path.join("new.txt"), "data").await.expect("write");
    engine.stage(&[path.join("new.txt")]).await.expect("stage");
    let hash = engine.commit("test commit").await.expect("commit");

    assert_ne!(hash, before, "commit hash should change");
    assert!(!hash.is_empty(), "hash should not be empty");
  }

  #[tokio::test]
  async fn test_commit_empty_errors() {
    eprintln!("[TEST] test_commit_empty_errors");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    let result = engine.commit("empty").await;
    assert!(result.is_err(), "commit without changes should return an error");
  }

  #[tokio::test]
  async fn test_get_head_commit() {
    eprintln!("[TEST] test_get_head_commit");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    let hash = engine.get_head_commit().await.expect("head");
    assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 characters");
  }

  #[tokio::test]
  async fn test_has_changes_new_file() {
    eprintln!("[TEST] test_has_changes_new_file");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string()).expect("engine");

    tokio::fs::write(path.join("untracked.txt"), "data")
      .await
      .expect("write");
    assert!(
      engine.has_changes().await.expect("has_changes"),
      "new file = has changes"
    );
  }

  #[tokio::test]
  async fn test_has_changes_clean() {
    eprintln!("[TEST] test_has_changes_clean");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    assert!(
      !engine.has_changes().await.expect("has_changes"),
      "clean repo = no changes"
    );
  }

  #[tokio::test]
  async fn test_modified_files() {
    eprintln!("[TEST] test_modified_files");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string()).expect("engine");

    tokio::fs::write(path.join("mod.txt"), "modified").await.expect("write");

    let files = engine.modified_files().await.expect("modified_files");
    assert!(!files.is_empty(), "should have modified files");
  }

  #[tokio::test]
  async fn test_push_no_remote() {
    eprintln!("[TEST] test_push_no_remote");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    // Repository without remote — push should return an error
    let result = engine.push().await;
    assert!(result.is_err(), "push without remote should return an error");
  }

  #[tokio::test]
  async fn test_push_to_bare() {
    eprintln!("[TEST] test_push_to_bare");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
      let engine = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine");

      // Create a commit and push
      tokio::fs::write(clone1.join("new.txt"), "data").await.expect("write");
      engine.stage(&[clone1.join("new.txt")]).await.expect("stage");
      engine.commit("new file").await.expect("commit");

      let result = engine.push().await.expect("push");
      assert!(matches!(result, PushResult::Success), "push to bare: {result:?}");
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  #[tokio::test]
  async fn test_pull_fast_forward() {
    eprintln!("[TEST] test_pull_fast_forward");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;

      // Commit and push from clone1
      tokio::fs::write(clone1.join("from_clone1.txt"), "data")
        .await
        .expect("write");
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      engine1.stage(&[clone1.join("from_clone1.txt")]).await.expect("stage");
      engine1.commit("from clone1").await.expect("commit");
      engine1.push().await.expect("push");

      // Pull from clone2
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");
      let result = engine2.pull().await.expect("pull");
      assert!(
        matches!(result, MergeResult::FastForward),
        "pull should be fast-forward: {result:?}"
      );

      // Verify the file appeared
      assert!(clone2.join("from_clone1.txt").exists(), "file should appear after pull");
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Two clones edit different lines of the same file — auto-merge on pull.
  #[tokio::test]
  async fn test_merge_different_lines_auto_merges() {
    eprintln!("[TEST] test_merge_different_lines_auto_merges");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Base file: three lines, created in clone1
      let shared = clone1.join("shared.txt");
      tokio::fs::write(&shared, "line1\nline2\nline3\n")
        .await
        .expect("write shared");
      engine1.stage(&[shared.clone()]).await.expect("stage");
      engine1.commit("add shared.txt").await.expect("commit");
      engine1.push().await.expect("push");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone1 modifies line 1
      tokio::fs::write(&shared, "MODIFIED1\nline2\nline3\n")
        .await
        .expect("write clone1");
      engine1.stage(&[shared]).await.expect("stage clone1");
      engine1.commit("modify line 1").await.expect("commit clone1");
      engine1.push().await.expect("push clone1");

      // Clone2 modifies line 3
      let shared2 = clone2.join("shared.txt");
      tokio::fs::write(&shared2, "line1\nline2\nMODIFIED3\n")
        .await
        .expect("write clone2");
      engine2.stage(&[shared2.clone()]).await.expect("stage clone2");
      engine2.commit("modify line 3").await.expect("commit clone2");

      // Pull in clone2 — should auto-merge without conflicts
      let result = engine2.pull().await.expect("pull clone2");
      assert!(
        !matches!(result, MergeResult::Conflict { .. }),
        "different lines should not conflict: {result:?}"
      );

      // Verify both edits are present
      let content = tokio::fs::read_to_string(&shared2).await.expect("read merged");
      assert!(content.contains("MODIFIED1"), "clone1 edit should be present");
      assert!(content.contains("MODIFIED3"), "clone2 edit should be present");
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Two clones edit the same line — conflict on pull.
  #[tokio::test]
  async fn test_merge_same_line_conflicts() {
    eprintln!("[TEST] test_merge_same_line_conflicts");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Base file: three lines, created in clone1
      let shared = clone1.join("shared.txt");
      tokio::fs::write(&shared, "line1\nline2\nline3\n")
        .await
        .expect("write shared");
      engine1.stage(&[shared.clone()]).await.expect("stage");
      engine1.commit("add shared.txt").await.expect("commit");
      engine1.push().await.expect("push");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone1 modifies line 1
      tokio::fs::write(&shared, "CHANGE_FROM_CLONE1\nline2\nline3\n")
        .await
        .expect("write clone1");
      engine1.stage(&[shared]).await.expect("stage clone1");
      engine1.commit("clone1: modify line 1").await.expect("commit clone1");
      engine1.push().await.expect("push clone1");

      // Clone2 also modifies line 1
      let shared2 = clone2.join("shared.txt");
      tokio::fs::write(&shared2, "CHANGE_FROM_CLONE2\nline2\nline3\n")
        .await
        .expect("write clone2");
      engine2.stage(&[shared2]).await.expect("stage clone2");
      engine2.commit("clone2: modify line 1").await.expect("commit clone2");

      // Pull in clone2 — should return a conflict
      let result = engine2.pull().await.expect("pull clone2");
      assert!(
        matches!(result, MergeResult::Conflict { .. }),
        "same line should conflict: {result:?}"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Fetch without new commits on remote returns UpToDate.
  #[tokio::test]
  async fn test_fetch_up_to_date() {
    eprintln!("[TEST] test_fetch_up_to_date");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
      let engine = GitEngine::new(clone1, "main".to_string()).expect("engine");

      // Fetch without any new commits
      let result = engine.fetch().await.expect("fetch");
      assert!(
        matches!(result, FetchResult::UpToDate),
        "fetch without new commits should return UpToDate: {result:?}"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// One clone deletes a file, another edits it — conflict.
  #[tokio::test]
  async fn test_delete_vs_edit_conflict() {
    eprintln!("[TEST] test_delete_vs_edit_conflict");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;

      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Create shared file file.txt in clone1
      tokio::fs::write(clone1.join("file.txt"), "original content")
        .await
        .expect("write");
      engine1.stage(&[clone1.join("file.txt")]).await.expect("stage");
      engine1.commit("add file.txt").await.expect("commit");
      engine1.push().await.expect("push setup");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone 1 deletes the file and pushes
      tokio::fs::remove_file(clone1.join("file.txt")).await.expect("remove");
      engine1.stage(&[clone1.join("file.txt")]).await.expect("stage");
      engine1.commit("delete file").await.expect("commit");
      let push_result = engine1.push().await.expect("push1");
      assert!(matches!(push_result, PushResult::Success), "push1: {push_result:?}");

      // Clone 2 edits the same file
      tokio::fs::write(clone2.join("file.txt"), "edited content")
        .await
        .expect("write");
      engine2.stage(&[clone2.join("file.txt")]).await.expect("stage");
      engine2.commit("edit file").await.expect("commit");

      // Push will be rejected
      let push_result = engine2.push().await.expect("push2");
      assert!(
        matches!(push_result, PushResult::Rejected),
        "push2 should be Rejected: {push_result:?}"
      );

      // Pull — conflict (delete vs edit)
      let merge_result = engine2.pull().await.expect("pull");
      assert!(
        matches!(merge_result, MergeResult::Conflict { .. }),
        "delete vs edit should produce a conflict: {merge_result:?}"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// modified_files returns the correct list after editing.
  #[tokio::test]
  async fn test_modified_files_after_edit() {
    eprintln!("[TEST] test_modified_files_after_edit");
    let (_tmp, repo_path) = create_test_repo().await;
    let engine = GitEngine::new(repo_path.clone(), "main".to_string()).expect("engine");

    // Modify file (README.md created in create_test_repo)
    tokio::fs::write(repo_path.join("README.md"), "changed content")
      .await
      .expect("write");

    let modified = engine.modified_files().await.expect("modified_files");
    assert!(
      modified.iter().any(|f| f.to_string_lossy().contains("README.md")),
      "README.md should be in modified: {modified:?}"
    );
  }

  /// Commit without staging — error (nothing committed).
  #[tokio::test]
  async fn test_commit_empty_index_errors() {
    eprintln!("[TEST] test_commit_empty_index_errors");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    // Commit without changes in the index — should return an error
    let result = engine.commit("empty commit").await;
    assert!(result.is_err(), "commit without staged files should return an error");
    let err_msg = result.expect_err("err").to_string();
    assert!(
      err_msg.contains("nothing to commit") || err_msg.contains("git commit failed"),
      "error should contain 'nothing to commit' or 'git commit failed': {err_msg}"
    );
  }

  /// Create a file, modify it — modified_files() returns the specific path.
  #[tokio::test]
  async fn test_modified_files_list() {
    eprintln!("[TEST] test_modified_files_list");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path.clone(), "main".to_string()).expect("engine");

    // Create and commit a file
    let file = path.join("tracked.txt");
    tokio::fs::write(&file, "original").await.expect("write");
    engine.stage(&[file.clone()]).await.expect("stage");
    engine.commit("add tracked.txt").await.expect("commit");

    // Modify the file
    tokio::fs::write(&file, "modified content").await.expect("write mod");

    let files = engine.modified_files().await.expect("modified_files");
    assert!(
      files.iter().any(|f| f.to_string_lossy().contains("tracked.txt")),
      "tracked.txt should be in modified_files: {files:?}"
    );
  }

  /// After the initial commit, get_head_commit() returns a valid hex SHA-1.
  #[tokio::test]
  async fn test_get_head_commit_returns_sha() {
    eprintln!("[TEST] test_get_head_commit_returns_sha");
    let (_tmp, path) = create_test_repo().await;
    let engine = GitEngine::new(path, "main".to_string()).expect("engine");

    let sha = engine.get_head_commit().await.expect("head");
    assert_eq!(sha.len(), 40, "SHA-1 should be 40 characters, got: {}", sha.len());
    // Verify all characters are valid hex
    assert!(
      sha.chars().all(|c| c.is_ascii_hexdigit()),
      "SHA-1 should contain only hex characters: {sha}"
    );
  }

  /// Reverse of test_delete_vs_edit_conflict: one clone edits a file,
  /// another deletes it — conflict on pull (edit vs delete).
  #[tokio::test]
  async fn test_edit_vs_delete_conflict() {
    eprintln!("[TEST] test_edit_vs_delete_conflict");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;

      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Create shared file in clone1
      tokio::fs::write(clone1.join("shared.txt"), "original content")
        .await
        .expect("write");
      engine1.stage(&[clone1.join("shared.txt")]).await.expect("stage");
      engine1.commit("add shared.txt").await.expect("commit");
      engine1.push().await.expect("push setup");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone 1 EDITS the file and pushes
      tokio::fs::write(clone1.join("shared.txt"), "edited by clone1")
        .await
        .expect("write edit");
      engine1.stage(&[clone1.join("shared.txt")]).await.expect("stage edit");
      engine1.commit("clone1: edit shared.txt").await.expect("commit edit");
      let push_result = engine1.push().await.expect("push1");
      assert!(matches!(push_result, PushResult::Success), "push1: {push_result:?}");

      // Clone 2 DELETES the file
      tokio::fs::remove_file(clone2.join("shared.txt")).await.expect("remove");
      engine2.stage(&[clone2.join("shared.txt")]).await.expect("stage delete");
      engine2
        .commit("clone2: delete shared.txt")
        .await
        .expect("commit delete");

      // Push will be rejected
      let push_result = engine2.push().await.expect("push2");
      assert!(
        matches!(push_result, PushResult::Rejected),
        "push2 should be Rejected: {push_result:?}"
      );

      // Pull — conflict (edit vs delete)
      let merge_result = engine2.pull().await.expect("pull");
      assert!(
        matches!(merge_result, MergeResult::Conflict { .. }),
        "edit vs delete should produce a conflict: {merge_result:?}"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Two clones of one bare repo: both commit to different files, one pushes,
  /// second push rejected — pull (rebase) — retry push — success.
  #[tokio::test]
  async fn test_push_retry_with_local_rebase() {
    eprintln!("[TEST] test_push_retry_with_local_rebase");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;

      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Clone1 commits and pushes file A
      tokio::fs::write(clone1.join("file_a.txt"), "data from clone1")
        .await
        .expect("write a");
      engine1.stage(&[clone1.join("file_a.txt")]).await.expect("stage a");
      engine1.commit("clone1: add file_a.txt").await.expect("commit a");
      let push1 = engine1.push().await.expect("push1");
      assert!(matches!(push1, PushResult::Success), "first push: {push1:?}");

      // Clone2 commits file B (unaware of file_a.txt)
      tokio::fs::write(clone2.join("file_b.txt"), "data from clone2")
        .await
        .expect("write b");
      engine2.stage(&[clone2.join("file_b.txt")]).await.expect("stage b");
      engine2.commit("clone2: add file_b.txt").await.expect("commit b");

      // Push from clone2 — rejected (remote changed)
      let push2 = engine2.push().await.expect("push2");
      assert!(
        matches!(push2, PushResult::Rejected),
        "push from clone2 should be Rejected: {push2:?}"
      );

      // Pull (rebase) — fetch changes from clone1
      let merge = engine2.pull().await.expect("pull");
      assert!(
        !matches!(merge, MergeResult::Conflict { .. }),
        "different files should not conflict: {merge:?}"
      );

      // Retry push — should succeed now
      let push_retry = engine2.push().await.expect("push retry");
      assert!(
        matches!(push_retry, PushResult::Success),
        "retry push after rebase should be Success: {push_retry:?}"
      );

      // Verify both files exist in clone2
      assert!(
        clone2.join("file_a.txt").exists(),
        "file_a.txt should appear after pull"
      );
      assert!(clone2.join("file_b.txt").exists(), "file_b.txt should remain");
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// After a commit, has_changes returns false.
  #[tokio::test]
  async fn test_git_status_after_commit() {
    eprintln!("[TEST] test_git_status_after_commit");
    let (_tmp, repo_path) = create_test_repo().await;
    let engine = GitEngine::new(repo_path.clone(), "main".to_string()).expect("engine");

    // Create a new file
    tokio::fs::write(repo_path.join("new.txt"), "new").await.expect("write");
    assert!(engine.has_changes().await.expect("has_changes"), "should have changes");

    // Stage + commit
    engine.stage(&[repo_path.join("new.txt")]).await.expect("stage");
    engine.commit("add new.txt").await.expect("commit");

    assert!(
      !engine.has_changes().await.expect("has_changes"),
      "no changes after commit"
    );
  }

  /// Two clones edit different lines of the same file.
  /// Clone1 — line 1, clone2 — line 3.
  /// Push clone1, push clone2 (rejected), pull clone2 — auto-merge without conflicts.
  #[tokio::test]
  async fn test_two_repos_edit_different_lines_auto_merge() {
    eprintln!("[TEST] test_two_repos_edit_different_lines_auto_merge");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Create a file with three lines in clone1, push
      let file1 = clone1.join("data.txt");
      tokio::fs::write(&file1, "line1\nline2\nline3\n")
        .await
        .expect("write base");
      engine1.stage(&[file1.clone()]).await.expect("stage base");
      engine1.commit("base file with three lines").await.expect("commit base");
      engine1.push().await.expect("push base");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone1 modifies line 1 — "modified1"
      tokio::fs::write(&file1, "modified1\nline2\nline3\n")
        .await
        .expect("write clone1");
      engine1.stage(&[file1]).await.expect("stage clone1");
      engine1.commit("clone1: modify line 1").await.expect("commit clone1");
      engine1.push().await.expect("push clone1");

      // Clone2 modifies line 3 — "modified3"
      let file2 = clone2.join("data.txt");
      tokio::fs::write(&file2, "line1\nline2\nmodified3\n")
        .await
        .expect("write clone2");
      engine2.stage(&[file2.clone()]).await.expect("stage clone2");
      engine2.commit("clone2: modify line 3").await.expect("commit clone2");

      // Push clone2 — should be rejected
      let push_result = engine2.push().await.expect("push clone2");
      assert!(
        matches!(push_result, PushResult::Rejected),
        "push clone2 should be Rejected: {push_result:?}"
      );

      // Pull clone2 — auto-merge (different lines)
      let merge_result = engine2.pull().await.expect("pull clone2");
      assert!(
        !matches!(merge_result, MergeResult::Conflict { .. }),
        "different lines should not conflict: {merge_result:?}"
      );

      // Verify final content — both edits present
      let content = tokio::fs::read_to_string(&file2).await.expect("read merged");
      assert!(content.contains("modified1"), "clone1 edit (line 1) should be present");
      assert!(content.contains("line2"), "line 2 should be unchanged");
      assert!(content.contains("modified3"), "clone2 edit (line 3) should be present");
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Two clones edit the same line of the same file.
  /// Push clone1, push clone2 (rejected), pull clone2 — conflict.
  #[tokio::test]
  async fn test_two_repos_edit_same_line_conflict() {
    eprintln!("[TEST] test_two_repos_edit_same_line_conflict");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Create a file with three lines in clone1, push
      let file1 = clone1.join("data.txt");
      tokio::fs::write(&file1, "line1\nline2\nline3\n")
        .await
        .expect("write base");
      engine1.stage(&[file1.clone()]).await.expect("stage base");
      engine1.commit("base file with three lines").await.expect("commit base");
      engine1.push().await.expect("push base");

      // Sync clone2
      engine2.pull().await.expect("pull sync");

      // Clone1 modifies line 2
      tokio::fs::write(&file1, "line1\nchanged_by_clone1\nline3\n")
        .await
        .expect("write clone1");
      engine1.stage(&[file1]).await.expect("stage clone1");
      engine1.commit("clone1: modify line 2").await.expect("commit clone1");
      engine1.push().await.expect("push clone1");

      // Clone2 also modifies line 2 (conflict)
      let file2 = clone2.join("data.txt");
      tokio::fs::write(&file2, "line1\nchanged_by_clone2\nline3\n")
        .await
        .expect("write clone2");
      engine2.stage(&[file2]).await.expect("stage clone2");
      engine2.commit("clone2: modify line 2").await.expect("commit clone2");

      // Push clone2 — rejected
      let push_result = engine2.push().await.expect("push clone2");
      assert!(
        matches!(push_result, PushResult::Rejected),
        "push clone2 should be Rejected: {push_result:?}"
      );

      // Pull clone2 — conflict (same line)
      let merge_result = engine2.pull().await.expect("pull clone2");
      assert!(
        matches!(merge_result, MergeResult::Conflict { .. }),
        "same line should conflict: {merge_result:?}"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }

  /// Clone1 pushes a commit, clone2 fetches.
  /// After fetch, clone2 refs should update (origin/main points to the new commit).
  #[tokio::test]
  async fn test_fetch_updates_refs() {
    eprintln!("[TEST] test_fetch_updates_refs");
    tokio::time::timeout(TEST_TIMEOUT, async {
      let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
      let engine1 = GitEngine::new(clone1.clone(), "main".to_string()).expect("engine1");
      let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");

      // Remember clone2 remote HEAD before fetch
      let before = engine2.get_remote_head().await.expect("remote head before");

      // Clone1 creates a commit and pushes
      tokio::fs::write(clone1.join("fetched.txt"), "fetch test")
        .await
        .expect("write");
      engine1.stage(&[clone1.join("fetched.txt")]).await.expect("stage");
      engine1.commit("commit to verify fetch").await.expect("commit");
      engine1.push().await.expect("push");

      // Clone2 fetches
      let fetch_result = engine2.fetch().await.expect("fetch");
      assert!(
        matches!(fetch_result, FetchResult::Updated { .. }),
        "fetch should return Updated: {fetch_result:?}"
      );

      // Clone2 remote HEAD should change after fetch
      let after = engine2.get_remote_head().await.expect("remote head after");
      assert_ne!(before, after, "remote ref should update after fetch");

      // Clone2 remote HEAD should match clone1 HEAD
      let clone1_head = engine1.get_head_commit().await.expect("clone1 head");
      assert_eq!(
        after.as_deref(),
        Some(clone1_head.as_str()),
        "clone2 remote ref should point to clone1 commit"
      );
    })
    .await
    .expect("test timed out — possible deadlock");
  }
}
