//! Wiki backend error taxonomy.

use omnifuse_core::ErrorKind;
use thiserror::Error;

/// Structured wiki backend error.
#[derive(Debug, Error)]
pub enum WikiError {
  /// Invalid configuration or request setup.
  #[error("invalid config: {0}")]
  InvalidConfig(String),
  /// Page is missing.
  #[error("page not found")]
  PageNotFound,
  /// Access denied.
  #[error("access denied")]
  AccessDenied,
  /// Remote changes conflict.
  #[error("changes conflict")]
  ChangesConflict,
  /// Slug is occupied or reserved.
  #[error("slug is occupied or reserved")]
  SlugOccupiedOrReserved,
  /// Transport/network failure.
  #[error("network unavailable: {0}")]
  Transport(String),
  /// Request failed after connection was established.
  #[error("request failed: {0}")]
  RequestFailed(String),
  /// Redirect to unexpected host.
  #[error(
    "HTTP {status}: server returned a redirect to {location}. Check that base_url points to the API host (not the web UI)"
  )]
  Redirect {
    /// Status code.
    status: u16,
    /// Redirect location.
    location: String
  },
  /// JSON deserialization failure.
  #[error("deserialization ({status}): {message}")]
  Deserialization {
    /// Status code.
    status: u16,
    /// Source error string.
    message: String
  },
  /// Generic HTTP failure.
  #[error("HTTP {status}: {body}")]
  HttpStatus {
    /// Status code.
    status: u16,
    /// Response body.
    body: String
  },
  /// Backend not initialized.
  #[error("backend not initialized")]
  NotInitialized
}

/// Classify wiki error into shared core taxonomy.
#[must_use]
pub fn classify_wiki_error(error: &anyhow::Error) -> Option<ErrorKind> {
  match error.downcast_ref::<WikiError>() {
    Some(WikiError::InvalidConfig(_)) => Some(ErrorKind::InvalidConfig),
    Some(WikiError::PageNotFound) => Some(ErrorKind::NotFound),
    Some(WikiError::AccessDenied) => Some(ErrorKind::PermissionDenied),
    Some(WikiError::ChangesConflict) => Some(ErrorKind::Conflict),
    Some(WikiError::SlugOccupiedOrReserved) => Some(ErrorKind::InvalidInput),
    Some(WikiError::Transport(_)) => Some(ErrorKind::Offline),
    Some(WikiError::RequestFailed(_)) => Some(ErrorKind::Internal),
    Some(WikiError::Redirect { .. } | WikiError::Deserialization { .. } | WikiError::HttpStatus { .. }) => {
      Some(ErrorKind::ProtocolViolation)
    }
    Some(WikiError::NotInitialized) => Some(ErrorKind::Internal),
    None => None
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_classify_wiki_error() {
    let offline: anyhow::Error = WikiError::Transport("connection refused".to_string()).into();
    let denied: anyhow::Error = WikiError::AccessDenied.into();
    let missing: anyhow::Error = WikiError::PageNotFound.into();
    let conflict: anyhow::Error = WikiError::ChangesConflict.into();

    assert_eq!(classify_wiki_error(&offline), Some(ErrorKind::Offline));
    assert_eq!(classify_wiki_error(&denied), Some(ErrorKind::PermissionDenied));
    assert_eq!(classify_wiki_error(&missing), Some(ErrorKind::NotFound));
    assert_eq!(classify_wiki_error(&conflict), Some(ErrorKind::Conflict));
  }
}
