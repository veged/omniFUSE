//! omnifuse-git — Git backend for `OmniFuse`.
//!
//! Implements the `omnifuse_core::Backend` trait via git CLI.
//! Ported from `SimpleGitFS`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod engine;
pub mod filter;
pub mod ops;
pub mod repo_source;

use std::{
  path::{Path, PathBuf},
  sync::OnceLock,
  time::Duration
};

use omnifuse_core::{Backend, InitResult, RemoteChange, SyncResult};
use tracing::{debug, info, warn};

use crate::{
  filter::GitignoreFilter,
  ops::{GitOps, StartupSyncResult}
};

/// Git backend configuration.
#[derive(Debug, Clone)]
pub struct GitConfig {
  /// Source: URL or local path.
  pub source: String,
  /// Branch.
  pub branch: String,
  /// Maximum number of push retries.
  pub max_push_retries: u32,
  /// Remote polling interval (seconds).
  pub poll_interval_secs: u64
}

impl Default for GitConfig {
  fn default() -> Self {
    Self {
      source: String::new(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30
    }
  }
}

/// Git backend for `OmniFuse`.
///
/// Implements the `Backend` trait: init -> clone/fetch, sync -> commit+push,
/// poll -> fetch+diff, apply -> pull.
#[derive(Debug)]
pub struct GitBackend {
  /// Configuration.
  config: GitConfig,
  /// Git operations (initialized in `init`).
  ops: OnceLock<GitOps>,
  /// `.gitignore` filter (initialized in `init`).
  filter: OnceLock<GitignoreFilter>
}

impl GitBackend {
  /// Create a new git backend.
  #[must_use]
  pub const fn new(config: GitConfig) -> Self {
    Self {
      config,
      ops: OnceLock::new(),
      filter: OnceLock::new()
    }
  }

  /// Get `GitOps` (after initialization).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend is not initialized.
  fn ops(&self) -> anyhow::Result<&GitOps> {
    self
      .ops
      .get()
      .ok_or_else(|| anyhow::anyhow!("backend not initialized"))
  }

  /// Get the list of changed files between local and remote HEAD.
  async fn diff_remote_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let ops = self.ops()?;
    let repo_path = ops.repo_path();
    let engine = ops.engine();

    let local_head = engine.get_head_commit().await?;
    let remote_head = engine.get_remote_head().await?;

    let Some(remote_head) = remote_head else {
      return Ok(Vec::new());
    };

    if local_head == remote_head {
      return Ok(Vec::new());
    }

    let output = tokio::process::Command::new("git")
      .current_dir(repo_path)
      .args(["diff", "--name-only", &local_head, &remote_head])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(Vec::new());
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|l| repo_path.join(l))
      .collect();

    Ok(files)
  }
}

impl Backend for GitBackend {
  async fn init(&self, _local_dir: &Path) -> anyhow::Result<InitResult> {
    // Prepare the repository (clone if remote)
    let source = crate::repo_source::RepoSource::parse(&self.config.source);
    let repo_path = source.ensure_available(&self.config.branch).await?;

    // Initialize git operations
    let ops = GitOps::new(repo_path.clone(), self.config.branch.clone())?;
    let _ = self.ops.set(ops);

    // Initialize .gitignore filter
    let filter = GitignoreFilter::new(&repo_path);
    let _ = self.filter.set(filter);

    // Startup sync: fetch + pull
    let ops = self.ops()?;
    match ops.startup_sync().await? {
      StartupSyncResult::UpToDate => Ok(InitResult::UpToDate),
      StartupSyncResult::Updated | StartupSyncResult::Merged => Ok(InitResult::Updated),
      StartupSyncResult::Conflicts { files } => Ok(InitResult::Conflicts { files }),
      StartupSyncResult::Offline => Ok(InitResult::Offline)
    }
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    let ops = self.ops()?;

    // Commit dirty files
    if let Err(e) = ops.auto_commit(dirty_files).await {
      let msg = e.to_string();
      if !msg.contains("nothing to commit") {
        return Err(e);
      }
      debug!("sync: no changes to commit");
    }

    // Push with retry (internally: push -> rejected -> pull -> retry)
    match ops.push_with_retry(self.config.max_push_retries).await {
      Ok(()) => Ok(SyncResult::Success {
        synced_files: dirty_files.len()
      }),
      Err(e) => {
        let msg = e.to_string();
        if msg.contains("conflict") {
          warn!("sync: conflicts during push");
          Ok(SyncResult::Conflict {
            synced_files: 0,
            conflict_files: dirty_files.to_vec()
          })
        } else if msg.contains("network") {
          Ok(SyncResult::Offline)
        } else {
          Err(e)
        }
      }
    }
  }

  async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
    let ops = self.ops()?;

    // Fetch and check for new commits
    if !ops.check_remote().await? {
      return Ok(Vec::new());
    }

    // Get the list of changed files
    let changed_files = self.diff_remote_files().await?;

    if changed_files.is_empty() {
      return Ok(Vec::new());
    }

    info!(count = changed_files.len(), "remote changes detected");

    // Return markers — content will be pulled via pull in apply_remote
    let changes = changed_files
      .into_iter()
      .map(|path| RemoteChange::Modified {
        path,
        content: Vec::new()
      })
      .collect();

    Ok(changes)
  }

  async fn apply_remote(&self, _changes: Vec<RemoteChange>) -> anyhow::Result<()> {
    let ops = self.ops()?;

    // Git pull will fetch all changes at once
    let result = ops.engine().pull().await?;
    debug!(?result, "apply_remote: pull completed");

    Ok(())
  }

  fn should_track(&self, path: &Path) -> bool {
    // Always hide .git/
    if path.components().any(|c| c.as_os_str() == ".git") {
      return false;
    }

    // Check gitignore (if filter is initialized)
    if let Some(filter) = self.filter.get() {
      return !filter.is_ignored(path);
    }

    true
  }

  fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.config.poll_interval_secs)
  }

  async fn is_online(&self) -> bool {
    let Ok(ops) = self.ops() else {
      return false;
    };

    ops.engine().fetch().await.is_ok()
  }

  fn name(&self) -> &'static str {
    "git"
  }
}
