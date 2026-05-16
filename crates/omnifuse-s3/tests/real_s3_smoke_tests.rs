#![allow(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  clippy::cloned_ref_to_slice_refs
)]

//! Ignored smoke test against a real S3-compatible bucket. Run only with disposable
//! credentials:
//!
//! ```bash
//! export OMNIFUSE_REAL_S3_BUCKET=...
//! export OMNIFUSE_REAL_S3_ENDPOINT=...
//! export OMNIFUSE_REAL_S3_REGION=...
//! export OMNIFUSE_REAL_S3_ACCESS_KEY_ID=...
//! export OMNIFUSE_REAL_S3_SECRET_ACCESS_KEY=...
//! cargo test -p omnifuse-s3 --test real_s3_smoke_tests -- --ignored
//! ```

use std::path::PathBuf;

use omnifuse_core::Backend;

#[tokio::test]
#[ignore = "requires OMNIFUSE_REAL_S3_* credentials and writes test objects"]
async fn real_s3_roundtrip_smoke() {
  let bucket = std::env::var("OMNIFUSE_REAL_S3_BUCKET").expect("bucket");
  let endpoint = std::env::var("OMNIFUSE_REAL_S3_ENDPOINT").ok();
  let region = std::env::var("OMNIFUSE_REAL_S3_REGION").ok();
  let access_key_id = std::env::var("OMNIFUSE_REAL_S3_ACCESS_KEY_ID").ok();
  let secret_access_key = std::env::var("OMNIFUSE_REAL_S3_SECRET_ACCESS_KEY").ok();
  let prefix = format!("omnifuse-smoke-{}", std::process::id());

  let backend = omnifuse_s3::S3Backend::new(omnifuse_s3::S3Config {
    bucket,
    prefix,
    endpoint,
    region,
    access_key_id,
    secret_access_key,
    session_token: None,
    virtual_host_style: false,
    poll_interval_secs: 60
  });

  let dir = tempfile::tempdir().expect("tempdir");
  backend.init(dir.path()).await.expect("init");

  let file = PathBuf::from("smoke.txt");
  std::fs::write(dir.path().join(&file), "hello").expect("write");
  backend.sync(&[file.clone()]).await.expect("sync");

  // Clean up so repeated runs don't litter the bucket.
  std::fs::remove_file(dir.path().join(&file)).expect("rm local");
  backend.sync(&[file]).await.expect("sync delete");
}
