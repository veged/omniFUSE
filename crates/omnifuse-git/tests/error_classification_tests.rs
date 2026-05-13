//! Git error classification tests.

#![allow(clippy::expect_used)]

use omnifuse_core::Code;
use omnifuse_git::{GitError, classify_git_error};

#[test]
fn test_git_error_classification_maps_network_and_conflict() {
  let network: anyhow::Error = GitError::NetworkUnavailable {
    message: "dns".to_string()
  }
  .into();
  let conflict: anyhow::Error = GitError::Conflict {
    files: vec!["README.md".into()]
  }
  .into();

  assert_eq!(classify_git_error(&network), Some(Code::Offline));
  assert_eq!(classify_git_error(&conflict), Some(Code::Conflict));
}

#[test]
fn test_git_error_classification_ignores_nothing_to_commit() {
  let nothing: anyhow::Error = GitError::NothingToCommit.into();
  assert_eq!(classify_git_error(&nothing), None);
}
