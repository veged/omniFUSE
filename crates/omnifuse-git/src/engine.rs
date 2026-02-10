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

    Ok(Some(
      String::from_utf8_lossy(&output.stdout).trim().to_string()
    ))
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
        return Ok(MergeResult::Conflict {
          files: conflict_files
        });
      }
      anyhow::bail!("git pull failed: {stderr}");
    }

    if stdout.contains("Already up to date")
      || (stdout.contains("Current branch") && stdout.contains("is up to date"))
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
