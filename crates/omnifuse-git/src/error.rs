//! Git backend error taxonomy.

use std::path::PathBuf;

use omnifuse_core::Code;
use thiserror::Error;

/// Structured git backend error.
#[derive(Debug, Error)]
pub enum GitError {
  /// Backend has not been initialized yet.
  #[error("backend not initialized")]
  NotInitialized,
  /// Invalid or missing git repository.
  #[error("not a git repository: {path}")]
  InvalidRepository {
    /// Repository path.
    path: PathBuf
  },
  /// Nothing to commit after staging.
  #[error("nothing to commit")]
  NothingToCommit,
  /// Commit requested with empty file list.
  #[error("no files to commit")]
  NoFilesToCommit,
  /// Network unavailable while talking to remote.
  #[error("network unavailable: {message}")]
  NetworkUnavailable {
    /// Underlying message.
    message: String
  },
  /// Merge or push conflict.
  #[error("{count} file(s) in conflict", count = .files.len())]
  Conflict {
    /// Files in conflict.
    files: Vec<PathBuf>
  },
  /// Remote rejected the push repeatedly.
  #[error("push rejected after {retries} attempts")]
  PushRejected {
    /// Retry count.
    retries: u32
  },
  /// Generic git command failure.
  #[error("{op} failed: {stderr}")]
  CommandFailed {
    /// Command operation.
    op: &'static str,
    /// Stderr output.
    stderr: String
  }
}

/// Classify a git error into shared core taxonomy.
#[must_use]
pub fn classify_git_error(error: &anyhow::Error) -> Option<Code> {
  match error.downcast_ref::<GitError>() {
    Some(GitError::NetworkUnavailable { .. }) => Some(Code::Offline),
    Some(GitError::Conflict { .. } | GitError::PushRejected { .. }) => Some(Code::Conflict),
    Some(GitError::InvalidRepository { .. }) => Some(Code::InvalidConfig),
    Some(GitError::CommandFailed { .. }) => Some(Code::BackendCommandFailed),
    Some(GitError::NotInitialized) => Some(Code::Internal),
    Some(GitError::NothingToCommit | GitError::NoFilesToCommit) | None => None
  }
}

/// Whether the error means there is nothing to commit.
#[must_use]
pub fn is_nothing_to_commit(error: &anyhow::Error) -> bool {
  matches!(error.downcast_ref::<GitError>(), Some(GitError::NothingToCommit))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_classify_git_error() {
    let offline: anyhow::Error = GitError::NetworkUnavailable {
      message: "dns".to_string()
    }
    .into();
    let conflict: anyhow::Error = GitError::Conflict {
      files: vec!["README.md".into()]
    }
    .into();
    let nothing: anyhow::Error = GitError::NothingToCommit.into();

    assert_eq!(classify_git_error(&offline), Some(Code::Offline));
    assert_eq!(classify_git_error(&conflict), Some(Code::Conflict));
    assert_eq!(classify_git_error(&nothing), None);
    assert!(is_nothing_to_commit(&nothing));
  }
}
