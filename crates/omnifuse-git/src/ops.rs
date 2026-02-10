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
