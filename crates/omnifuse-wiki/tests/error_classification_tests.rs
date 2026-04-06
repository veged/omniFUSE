//! Wiki error classification tests.

#![allow(clippy::expect_used)]

use omnifuse_core::ErrorKind;
use omnifuse_wiki::{WikiError, classify_wiki_error};

#[test]
fn test_wiki_error_classification_maps_access_and_offline() {
  let offline: anyhow::Error = WikiError::Transport("connection refused".to_string()).into();
  let denied: anyhow::Error = WikiError::AccessDenied.into();

  assert_eq!(classify_wiki_error(&offline), Some(ErrorKind::Offline));
  assert_eq!(classify_wiki_error(&denied), Some(ErrorKind::PermissionDenied));
}

#[test]
fn test_wiki_error_classification_maps_not_found_and_conflict() {
  let missing: anyhow::Error = WikiError::PageNotFound.into();
  let conflict: anyhow::Error = WikiError::ChangesConflict.into();

  assert_eq!(classify_wiki_error(&missing), Some(ErrorKind::NotFound));
  assert_eq!(classify_wiki_error(&conflict), Some(ErrorKind::Conflict));
}
