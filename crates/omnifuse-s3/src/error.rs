//! S3 backend error taxonomy.

use std::path::PathBuf;

use omnifuse_core::Code;
use opendal::ErrorKind;
use thiserror::Error;

/// Structured S3 backend error.
#[derive(Debug, Error)]
pub enum S3Error {
  /// Invalid S3 configuration.
  #[error("invalid config: {0}")]
  InvalidConfig(String),
  /// Invalid local path for object mapping.
  #[error("invalid s3 path: {0}")]
  InvalidPath(PathBuf),
  /// Local and remote changes cannot be merged safely.
  #[error("s3 object conflict: {path}")]
  Conflict {
    /// Local relative path.
    path: PathBuf
  },
  /// Remote write precondition failed and the caller should re-read remote state.
  #[error("s3 write precondition failed: {path}")]
  PreconditionFailed {
    /// Local relative path.
    path: PathBuf
  },
  /// Required OpenDAL capability is missing for this service.
  #[error("missing required capability: {0}")]
  MissingCapability(&'static str),
  /// Backend has not been initialized.
  #[error("backend not initialized")]
  NotInitialized,
  /// OpenDAL operation failed.
  #[error("opendal operation failed: {0}")]
  OpenDal(#[from] opendal::Error)
}

/// Classify S3 errors into the shared core taxonomy.
#[must_use]
pub fn classify_s3_error(error: &anyhow::Error) -> Option<Code> {
  match error.downcast_ref::<S3Error>() {
    Some(S3Error::InvalidConfig(_) | S3Error::InvalidPath(_)) => Some(Code::InvalidConfig),
    Some(S3Error::Conflict { .. } | S3Error::PreconditionFailed { .. }) => Some(Code::Conflict),
    Some(S3Error::MissingCapability(_)) => Some(Code::InvalidConfig),
    Some(S3Error::NotInitialized) => Some(Code::Internal),
    Some(S3Error::OpenDal(error)) => classify_opendal_kind(error.kind()),
    None => error
      .downcast_ref::<opendal::Error>()
      .and_then(|error| classify_opendal_kind(error.kind()))
  }
}

fn classify_opendal_kind(kind: ErrorKind) -> Option<Code> {
  match kind {
    ErrorKind::ConfigInvalid => Some(Code::InvalidConfig),
    ErrorKind::NotFound => Some(Code::NotFound),
    ErrorKind::PermissionDenied => Some(Code::PermissionDenied),
    ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists => Some(Code::Conflict),
    ErrorKind::Unsupported | ErrorKind::IsADirectory | ErrorKind::NotADirectory | ErrorKind::RangeNotSatisfied => {
      Some(Code::InvalidInput)
    }
    ErrorKind::RateLimited => Some(Code::Offline),
    ErrorKind::Unexpected | ErrorKind::IsSameFile => Some(Code::Internal),
    _ => Some(Code::Internal)
  }
}
