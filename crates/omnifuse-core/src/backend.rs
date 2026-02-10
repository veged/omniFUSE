//! Backend trait — unified interface for synchronized storage backends.
//!
//! Manages sync autonomously: the VFS core tells WHAT to synchronize,
//! the backend decides HOW.

use std::{
  path::{Path, PathBuf},
  time::Duration
};

/// Backend for a synchronized storage.
///
/// Manages sync autonomously: the VFS core tells WHAT to synchronize,
/// the backend decides HOW.
pub trait Backend: Send + Sync + 'static {
  /// Initialization: prepare the local directory.
  ///
  /// Git: clone (if remote URL) or verify an existing repo.
  /// Wiki: create local dir, fetch tree, write files.
  fn init(&self, local_dir: &Path) -> impl Future<Output = anyhow::Result<InitResult>> + Send;

  /// Synchronize local changes with remote.
  ///
  /// Called by `SyncEngine` after debounce/close trigger.
  /// The backend decides how to merge on conflicts.
  fn sync(&self, dirty_files: &[PathBuf]) -> impl Future<Output = anyhow::Result<SyncResult>> + Send;

  /// Check remote for changes (periodic poll).
  fn poll_remote(
    &self
  ) -> impl Future<Output = anyhow::Result<Vec<RemoteChange>>> + Send;

  /// Apply remote changes to the local directory.
  ///
  /// Does NOT overwrite dirty files (`SyncEngine` checks for that).
  fn apply_remote(
    &self,
    changes: Vec<RemoteChange>
  ) -> impl Future<Output = anyhow::Result<()>> + Send;

  /// Should this file be synchronized with remote?
  ///
  /// Git: check `.gitignore`, hide `.git/`.
  /// Wiki: only `.md` files.
  fn should_track(&self, path: &Path) -> bool;

  /// Interval for polling remote for changes.
  fn poll_interval(&self) -> Duration;

  /// Check remote availability.
  fn is_online(&self) -> impl Future<Output = bool> + Send;

  /// Backend name for logs and UI.
  fn name(&self) -> &'static str;
}

/// Initialization result.
#[derive(Debug, Clone)]
pub enum InitResult {
  /// Fresh clone/fetch.
  Fresh,
  /// Local directory is already up to date.
  UpToDate,
  /// Updated from remote.
  Updated,
  /// There are conflicts during initialization.
  Conflicts {
    /// Files with conflicts.
    files: Vec<PathBuf>
  },
  /// Remote is unavailable, working with local state.
  Offline
}

/// Synchronization result.
#[derive(Debug, Clone)]
pub enum SyncResult {
  /// All files synchronized successfully.
  Success {
    /// Number of synchronized files.
    synced_files: usize
  },
  /// Some files have conflicts.
  Conflict {
    /// Number of synchronized files.
    synced_files: usize,
    /// Files with conflicts.
    conflict_files: Vec<PathBuf>
  },
  /// Remote is unavailable.
  Offline
}

/// Remote change.
#[derive(Debug, Clone)]
pub enum RemoteChange {
  /// File was modified.
  Modified {
    /// File path.
    path: PathBuf,
    /// New content.
    content: Vec<u8>
  },
  /// File was deleted.
  Deleted {
    /// File path.
    path: PathBuf
  }
}

impl RemoteChange {
  /// Get the file path from the change.
  #[must_use]
  pub fn path(&self) -> &Path {
    match self {
      Self::Modified { path, .. } | Self::Deleted { path } => path
    }
  }
}
