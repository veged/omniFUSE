//! Newline-delimited JSON wire protocol between the `of` CLI and the daemon.
//!
//! Each request and response carries an explicit schema version `v`. A version
//! mismatch is a hard error — there is no negotiation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol schema version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Daemon request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
  /// Schema version. Must equal [`PROTOCOL_VERSION`].
  pub v: u32,
  /// Client-chosen identifier echoed in the response.
  pub id: u64,
  /// Method name (e.g. `mount.s3`, `list`, `unmount`).
  pub method: String,
  /// Optional method-specific parameters.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub params: Option<Value>
}

impl Request {
  /// Build a new request with the current schema version.
  #[must_use]
  #[allow(clippy::missing_const_for_fn)]
  pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
    Self {
      v: PROTOCOL_VERSION,
      id,
      method: method.into(),
      params
    }
  }
}

/// Daemon response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
  /// Schema version. Must equal [`PROTOCOL_VERSION`].
  pub v: u32,
  /// Echoes the originating request id.
  pub id: u64,
  /// Successful result.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub result: Option<Value>,
  /// Error payload when the request failed.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<ErrorPayload>
}

impl Response {
  /// Build an ok response.
  #[must_use]
  pub const fn ok(id: u64, result: Value) -> Self {
    Self {
      v: PROTOCOL_VERSION,
      id,
      result: Some(result),
      error: None
    }
  }

  /// Build an error response.
  #[must_use]
  pub fn err(id: u64, message: impl Into<String>) -> Self {
    Self {
      v: PROTOCOL_VERSION,
      id,
      result: None,
      error: Some(ErrorPayload {
        message: message.into()
      })
    }
  }
}

/// Error payload returned to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
  /// Human-readable message.
  pub message: String
}

/// Mount entry returned by `list` / `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSummary {
  /// Backend name (`git` / `wiki` / `s3`).
  pub backend: String,
  /// Instance hash (hex of SHA-256 of the normalised mount params).
  pub instance: String,
  /// Mount point on the host filesystem.
  pub mount_point: PathBuf,
  /// Seconds since the mount was established.
  pub age_secs: u64
}

/// Result of `list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResult {
  /// Active mounts.
  pub mounts: Vec<MountSummary>
}

/// Result of `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
  /// Mount summary if the path is currently mounted.
  pub mount: Option<MountSummary>,
  /// Number of dirty files known to the daemon for this mount.
  pub dirty_files: u64,
  /// Cache hit ratio (0.0..=1.0) when known.
  pub cache_hit_ratio: Option<f64>
}

/// Result of `unmount` and `mount.*`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResult {
  /// Affected mount point.
  pub mount_point: PathBuf
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[test]
  fn request_roundtrips() {
    let req = Request::new(7, "mount.s3", Some(serde_json::json!({"bucket": "x"})));
    let line = serde_json::to_string(&req).expect("encode");
    let parsed: Request = serde_json::from_str(&line).expect("decode");
    assert_eq!(parsed.v, PROTOCOL_VERSION);
    assert_eq!(parsed.id, 7);
    assert_eq!(parsed.method, "mount.s3");
    assert_eq!(parsed.params.expect("params"), serde_json::json!({"bucket": "x"}));
  }

  #[test]
  fn response_ok_and_err_are_distinguishable() {
    let ok = Response::ok(1, serde_json::json!({"ok": true}));
    let err = Response::err(2, "boom");
    let ok_line = serde_json::to_string(&ok).expect("encode ok");
    let err_line = serde_json::to_string(&err).expect("encode err");

    let parsed_ok: Response = serde_json::from_str(&ok_line).expect("decode ok");
    let parsed_err: Response = serde_json::from_str(&err_line).expect("decode err");
    assert!(parsed_ok.result.is_some());
    assert!(parsed_ok.error.is_none());
    assert!(parsed_err.result.is_none());
    assert_eq!(parsed_err.error.expect("error").message, "boom");
  }
}
