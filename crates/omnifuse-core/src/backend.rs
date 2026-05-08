//! Backend trait — unified interface for synchronized storage backends.
//!
//! Manages sync autonomously: the VFS core tells WHAT to synchronize,
//! the backend decides HOW.

use std::{
  path::{Path, PathBuf},
  time::Duration
};

use crate::{ErrorKind, PathProtection};

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

  /// Detect and safely apply remote changes.
  fn refresh_remote(
    &self,
    request: RemoteRefresh<'_>
  ) -> impl Future<Output = anyhow::Result<RemoteRefreshResult>> + Send {
    async move {
      let changes = self.poll_remote().await?;
      if changes.is_empty() {
        return Ok(RemoteRefreshResult::Unchanged);
      }

      let affected: Vec<PathBuf> = changes.iter().map(|change| change.path().to_path_buf()).collect();
      let protected: Vec<PathBuf> = affected
        .iter()
        .filter(|path| request.protected_paths.is_protected(path))
        .cloned()
        .collect();

      if !protected.is_empty() {
        return Ok(RemoteRefreshResult::Deferred {
          affected: protected,
          reason: RemoteDeferReason::ProtectedLocalChange
        });
      }

      if matches!(request.mode, RemoteApplyMode::DetectOnly) {
        return Ok(RemoteRefreshResult::Deferred {
          affected,
          reason: RemoteDeferReason::DetectOnly
        });
      }

      let mut changed = Vec::new();
      let mut deleted = Vec::new();
      for change in &changes {
        match change {
          RemoteChange::Modified { path, .. } => changed.push(path.clone()),
          RemoteChange::Deleted { path } => deleted.push(path.clone())
        }
      }

      self.apply_remote(changes).await?;
      Ok(RemoteRefreshResult::Applied { changed, deleted })
    }
  }

  /// Check remote for changes (periodic poll).
  fn poll_remote(&self) -> impl Future<Output = anyhow::Result<Vec<RemoteChange>>> + Send;

  /// Apply remote changes to the local directory.
  ///
  /// Does NOT overwrite dirty files (`SyncEngine` checks for that).
  fn apply_remote(&self, changes: Vec<RemoteChange>) -> impl Future<Output = anyhow::Result<()>> + Send;

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

  /// Classify backend-specific error into shared taxonomy.
  fn classify_error(&self, _error: &anyhow::Error) -> ErrorKind {
    ErrorKind::Internal
  }
}

/// Remote refresh request.
pub struct RemoteRefresh<'a> {
  /// Paths that must not be overwritten by remote changes.
  pub protected_paths: &'a dyn PathProtection,
  /// Remote refresh mode.
  pub mode: RemoteApplyMode
}

/// Remote refresh mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteApplyMode {
  /// Apply changes when protected paths are not affected.
  ApplySafe,
  /// Detect changes without applying them.
  DetectOnly
}

/// Reason why a remote refresh was deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDeferReason {
  /// Remote changes affect locally dirty paths.
  ProtectedLocalChange,
  /// Refresh was requested in detect-only mode.
  DetectOnly
}

/// Result of a remote refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteRefreshResult {
  /// No remote changes were found.
  Unchanged,
  /// Remote changes were applied locally.
  Applied {
    /// Modified paths.
    changed: Vec<PathBuf>,
    /// Deleted paths.
    deleted: Vec<PathBuf>
  },
  /// Remote changes were not applied.
  Deferred {
    /// Paths that caused deferral.
    affected: Vec<PathBuf>,
    /// Deferral reason.
    reason: RemoteDeferReason
  },
  /// Remote is unavailable.
  Offline
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
