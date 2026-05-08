//! omnifuse-git — Git backend for `OmniFuse`.
//!
//! Implements the `omnifuse_core::Backend` trait via git CLI.
//! Ported from `SimpleGitFS`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod engine;
pub mod error;
pub mod filter;
pub mod ops;
pub mod repo_source;
pub mod sync_lifecycle;
pub mod tracking;

use std::{
  path::{Path, PathBuf},
  sync::OnceLock,
  time::Duration
};

pub use error::{GitError, classify_git_error};
use omnifuse_core::{Backend, InitResult, RemoteRefresh, RemoteRefreshResult, SyncResult};

use crate::{
  sync_lifecycle::{GitInit, GitSync, GitSyncLifecycle},
  tracking::GitTrackingRules
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
  pub poll_interval_secs: u64,
  /// Local working directory (clone target).
  pub local_dir: PathBuf
}

impl Default for GitConfig {
  fn default() -> Self {
    Self {
      source: String::new(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30,
      local_dir: PathBuf::new()
    }
  }
}

/// Git backend for `OmniFuse`.
///
/// Implements the `Backend` trait: init -> clone/fetch, sync -> commit+push,
/// refresh -> fetch+safe pull.
#[derive(Debug)]
pub struct GitBackend {
  /// Configuration.
  config: GitConfig,
  /// Git lifecycle (initialized in `init`).
  lifecycle: OnceLock<GitSyncLifecycle>
}

impl GitBackend {
  /// Create a new git backend.
  #[must_use]
  pub const fn new(config: GitConfig) -> Self {
    Self {
      config,
      lifecycle: OnceLock::new()
    }
  }

  /// Get lifecycle (after initialization).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend is not initialized.
  fn lifecycle(&self) -> anyhow::Result<&GitSyncLifecycle> {
    self.lifecycle.get().ok_or_else(|| GitError::NotInitialized.into())
  }
}

impl Backend for GitBackend {
  async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
    let (lifecycle, init) = GitSyncLifecycle::open(self.config.clone(), local_dir).await?;
    let _ = self.lifecycle.set(lifecycle);
    Ok(map_init(init))
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    match self.lifecycle()?.sync_local(dirty_files).await? {
      GitSync::Success { synced_files } => Ok(SyncResult::Success { synced_files }),
      GitSync::Conflict { files } => Ok(SyncResult::Conflict {
        synced_files: 0,
        conflict_files: files
      }),
      GitSync::Offline => Ok(SyncResult::Offline)
    }
  }

  async fn refresh_remote(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    self.lifecycle()?.refresh_remote_protected(request).await
  }

  fn should_track(&self, path: &Path) -> bool {
    if GitTrackingRules::contains_git_directory(path) {
      return false;
    }

    self
      .lifecycle
      .get()
      .is_none_or(|lifecycle| lifecycle.should_track(path))
  }

  fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.config.poll_interval_secs)
  }

  async fn is_online(&self) -> bool {
    match self.lifecycle() {
      Ok(lifecycle) => lifecycle.is_online().await,
      Err(_) => false
    }
  }

  fn name(&self) -> &'static str {
    "git"
  }

  fn classify_error(&self, error: &anyhow::Error) -> omnifuse_core::ErrorKind {
    self.lifecycle.get().map_or_else(
      || classify_git_error(error).unwrap_or(omnifuse_core::ErrorKind::Internal),
      |lifecycle| lifecycle.classify(error)
    )
  }
}

fn map_init(init: GitInit) -> InitResult {
  match init {
    GitInit::UpToDate => InitResult::UpToDate,
    GitInit::Updated => InitResult::Updated,
    GitInit::Conflicts { files } => InitResult::Conflicts { files },
    GitInit::Offline => InitResult::Offline
  }
}
