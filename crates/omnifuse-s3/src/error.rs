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
pub fn classify_s3_error(error: &anyhow::Error) -> Code {
  if let Some(error) = error.downcast_ref::<S3Error>() {
    return match error {
      S3Error::InvalidConfig(_) | S3Error::InvalidPath(_) | S3Error::MissingCapability(_) => Code::InvalidConfig,
      S3Error::Conflict { .. } | S3Error::PreconditionFailed { .. } => Code::Conflict,
      S3Error::NotInitialized => Code::Internal,
      S3Error::OpenDal(error) => classify_opendal_kind(error.kind())
    };
  }
  error
    .downcast_ref::<opendal::Error>()
    .map_or(Code::Internal, |error| classify_opendal_kind(error.kind()))
}

#[must_use]
pub(crate) const fn classify_opendal_kind(kind: ErrorKind) -> Code {
  match kind {
    ErrorKind::ConfigInvalid => Code::InvalidConfig,
    ErrorKind::NotFound => Code::NotFound,
    ErrorKind::PermissionDenied => Code::PermissionDenied,
    ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists => Code::Conflict,
    ErrorKind::Unsupported | ErrorKind::IsADirectory | ErrorKind::NotADirectory | ErrorKind::RangeNotSatisfied => {
      Code::InvalidInput
    }
    ErrorKind::RateLimited => Code::Offline,
    _ => Code::Internal
  }
}
