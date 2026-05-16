//! omnifuse-s3 - S3-compatible backend for `OmniFuse` powered by `OpenDAL`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(
  clippy::doc_markdown,
  clippy::match_same_arms,
  clippy::significant_drop_tightening,
  clippy::similar_names
)]

pub mod config;
pub mod error;
pub mod manifest;
pub mod operator;
pub mod path;
pub mod session;

use std::{
  path::{Path, PathBuf},
  sync::{Arc, OnceLock},
  time::Duration
};

pub use config::S3Config;
pub use error::{S3Error, classify_s3_error};
use omnifuse_core::{Backend, FilesystemCache, InitResult, RemoteRefresh, RemoteRefreshResult, SyncResult};

use crate::{path::is_internal_path, session::S3Session};

/// S3-compatible backend powered by `OpenDAL`.
pub struct S3Backend {
  config: S3Config,
  session: OnceLock<S3Session>,
  cache: Option<Arc<FilesystemCache>>
}

impl S3Backend {
  /// Create a backend without a persistent cache.
  #[must_use]
  pub const fn new(config: S3Config) -> Self {
    Self {
      config,
      session: OnceLock::new(),
      cache: None
    }
  }

  /// Attach a persistent cache. Calls past `init` are not affected.
  #[must_use]
  pub fn with_cache(mut self, cache: Arc<FilesystemCache>) -> Self {
    self.cache = Some(cache);
    self
  }

  fn session(&self) -> anyhow::Result<&S3Session> {
    self.session.get().ok_or_else(|| S3Error::NotInitialized.into())
  }
}

impl Backend for S3Backend {
  async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
    if self.session.get().is_none() {
      let session = S3Session::attach(&self.config, local_dir, self.cache.clone())?;
      let _ = self.session.set(session);
    }
    self.session()?.initialize().await
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    self.session()?.sync_dirty(dirty_files).await
  }

  async fn refresh_remote(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    self.session()?.refresh_remote(request).await
  }

  fn should_track(&self, path: &Path) -> bool {
    !is_internal_path(path)
  }

  fn poll_interval(&self) -> Duration {
    self.config.poll_interval()
  }

  async fn is_online(&self) -> bool {
    // `stat("")` is unreliable on S3 (empty key is NotFound); use OpenDAL's `check()` which
    // probes the service and returns `Ok` for healthy operators.
    match self.session() {
      Ok(session) => session.operator().check().await.is_ok(),
      Err(_) => false
    }
  }

  fn name(&self) -> &'static str {
    "s3"
  }

  fn classify_error(&self, error: &anyhow::Error) -> omnifuse_core::Code {
    classify_s3_error(error)
  }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  fn test_config() -> S3Config {
    S3Config {
      bucket: "test-bucket".to_string(),
      prefix: String::new(),
      endpoint: None,
      region: None,
      access_key_id: None,
      secret_access_key: None,
      session_token: None,
      virtual_host_style: false,
      poll_interval_secs: 60
    }
  }

  #[test]
  fn s3_backend_excludes_internal_vfs_paths() {
    let backend = S3Backend::new(test_config());
    assert!(backend.should_track(Path::new("docs/file.txt")));
    assert!(!backend.should_track(Path::new(".vfs/s3/manifest.json")));
    assert!(!backend.should_track(Path::new("dir/.vfs/file")));
  }

  #[test]
  fn s3_backend_reports_name() {
    let backend = S3Backend::new(test_config());
    assert_eq!(backend.name(), "s3");
  }

  #[test]
  fn s3_backend_poll_interval_matches_config() {
    let backend = S3Backend::new(test_config());
    assert_eq!(backend.poll_interval(), Duration::from_secs(60));
  }
}
