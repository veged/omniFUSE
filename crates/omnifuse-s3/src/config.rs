//! S3 backend configuration.

use std::time::Duration;

use omnifuse_core::InstanceHash;

/// S3-compatible backend configuration.
#[derive(Clone, Eq, PartialEq)]
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

impl std::fmt::Debug for S3Config {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("S3Config")
      .field("bucket", &self.bucket)
      .field("prefix", &self.prefix)
      .field("endpoint", &self.endpoint)
      .field("region", &self.region)
      .field("access_key_id", &self.access_key_id.as_ref().map(|_| "<redacted>"))
      .field(
        "secret_access_key",
        &self.secret_access_key.as_ref().map(|_| "<redacted>")
      )
      .field("session_token", &self.session_token.as_ref().map(|_| "<redacted>"))
      .field("virtual_host_style", &self.virtual_host_style)
      .field("poll_interval_secs", &self.poll_interval_secs)
      .finish()
  }
}

impl S3Config {
  /// Remote polling interval.
  #[must_use]
  pub const fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.poll_interval_secs)
  }

  /// Stable instance hash derived from the parameters that identify the remote.
  ///
  /// Credentials and transport flags are intentionally excluded — two clients
  /// pointing at the same bucket/prefix see the same content.
  #[must_use]
  pub fn instance_hash(&self) -> InstanceHash {
    InstanceHash::from_parts(&instance_identity_parts(
      self.endpoint.as_deref(),
      self.region.as_deref(),
      &self.bucket,
      &self.prefix
    ))
  }
}

/// Normalized identity parts for S3 content.
///
/// Credentials and transport flags are intentionally excluded: two clients
/// pointing at the same bucket/prefix see the same content.
#[must_use]
pub fn instance_identity_parts<'a>(
  endpoint: Option<&'a str>,
  region: Option<&'a str>,
  bucket: &'a str,
  prefix: &'a str
) -> [&'a str; 5] {
  [
    "s3",
    endpoint.unwrap_or(""),
    region.unwrap_or(""),
    bucket,
    prefix.trim_matches('/')
  ]
}
