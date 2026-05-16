//! OpenDAL S3 operator construction.

use opendal::{Operator, services};

use crate::{S3Config, S3Error};

/// Build an OpenDAL S3 operator.
///
/// # Errors
///
/// Returns an error if S3 config is invalid.
pub fn build_operator(config: &S3Config) -> Result<Operator, S3Error> {
  if config.bucket.trim().is_empty() {
    return Err(S3Error::InvalidConfig("bucket must not be empty".to_string()));
  }

  let mut builder = services::S3::default().bucket(&config.bucket);
  let root = normalize_root(&config.prefix);
  if !root.is_empty() {
    builder = builder.root(&root);
  }

  if let Some(endpoint) = non_empty(config.endpoint.as_deref()) {
    builder = builder.endpoint(endpoint);
  }
  if let Some(region) = non_empty(config.region.as_deref()) {
    builder = builder.region(region);
  }
  if let Some(access_key_id) = non_empty(config.access_key_id.as_deref()) {
    builder = builder.access_key_id(access_key_id);
  }
  if let Some(secret_access_key) = non_empty(config.secret_access_key.as_deref()) {
    builder = builder.secret_access_key(secret_access_key);
  }
  if let Some(session_token) = non_empty(config.session_token.as_deref()) {
    builder = builder.session_token(session_token);
  }
  if config.virtual_host_style {
    builder = builder.enable_virtual_host_style();
  }

  Ok(Operator::new(builder)?.finish())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
  value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_root(prefix: &str) -> String {
  let trimmed = prefix.trim_matches('/');
  if trimmed.is_empty() {
    String::new()
  } else {
    format!("/{trimmed}/")
  }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn empty_bucket_is_invalid() {
    let config = S3Config {
      bucket: String::new(),
      prefix: String::new(),
      endpoint: None,
      region: None,
      access_key_id: None,
      secret_access_key: None,
      session_token: None,
      virtual_host_style: false,
      poll_interval_secs: 60
    };

    assert!(build_operator(&config).is_err());
  }
}
