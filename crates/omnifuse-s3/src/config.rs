//! S3 backend configuration.

use std::time::Duration;

/// S3-compatible backend configuration.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct S3Config {
  /// Bucket name.
  pub bucket: String,
  /// Object prefix mounted as the filesystem root.
  pub prefix: String,
  /// S3 endpoint URL.
  pub endpoint: Option<String>,
  /// S3 region. Use `auto` for R2 and MinIO when appropriate.
  pub region: Option<String>,
  /// Access key ID.
  pub access_key_id: Option<String>,
  /// Secret access key.
  pub secret_access_key: Option<String>,
  /// Temporary session token.
  pub session_token: Option<String>,
  /// Use virtual-hosted style requests.
  pub virtual_host_style: bool,
  /// Remote polling interval in seconds.
  pub poll_interval_secs: u64
}

impl S3Config {
  /// Remote polling interval.
  #[must_use]
  pub const fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.poll_interval_secs)
  }
}
