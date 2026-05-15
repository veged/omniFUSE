#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Live provider matrix against MinIO.
//!
//! Start MinIO before running these tests:
//!
//! ```bash
//! docker run -d --rm --name omnifuse-minio \
//!   -p 9000:9000 -p 9001:9001 \
//!   -e MINIO_ROOT_USER=minioadmin \
//!   -e MINIO_ROOT_PASSWORD=minioadmin \
//!   minio/minio server /data --console-address ":9001"
//!
//! docker exec omnifuse-minio mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
//! docker exec omnifuse-minio mc mb local/omnifuse-test || true
//!
//! cargo test -p omnifuse-s3 --test minio_matrix_tests -- --ignored
//! ```
//!
//! The same suite runs against R2, Yandex Object Storage and Backblaze B2 by overriding
//! `OMNIFUSE_MINIO_ENDPOINT`, `OMNIFUSE_MINIO_BUCKET`, `OMNIFUSE_MINIO_REGION`,
//! `OMNIFUSE_MINIO_ACCESS_KEY_ID`, `OMNIFUSE_MINIO_SECRET_ACCESS_KEY`.

use std::path::{Path, PathBuf};

use omnifuse_core::{Backend, PathProtection, RemoteApplyMode, RemoteRefresh, RemoteRefreshResult, SyncResult};
use omnifuse_s3::{S3Backend, S3Config};

const ENDPOINT_ENV: &str = "OMNIFUSE_MINIO_ENDPOINT";
const BUCKET_ENV: &str = "OMNIFUSE_MINIO_BUCKET";
const REGION_ENV: &str = "OMNIFUSE_MINIO_REGION";
const ACCESS_ENV: &str = "OMNIFUSE_MINIO_ACCESS_KEY_ID";
const SECRET_ENV: &str = "OMNIFUSE_MINIO_SECRET_ACCESS_KEY";

fn minio_config(prefix: String) -> S3Config {
  S3Config {
    bucket: std::env::var(BUCKET_ENV).unwrap_or_else(|_| "omnifuse-test".to_string()),
    prefix,
    endpoint: Some(std::env::var(ENDPOINT_ENV).unwrap_or_else(|_| "http://127.0.0.1:9000".to_string())),
    region: Some(std::env::var(REGION_ENV).unwrap_or_else(|_| "us-east-1".to_string())),
    access_key_id: Some(std::env::var(ACCESS_ENV).unwrap_or_else(|_| "minioadmin".to_string())),
    secret_access_key: Some(std::env::var(SECRET_ENV).unwrap_or_else(|_| "minioadmin".to_string())),
    session_token: None,
    virtual_host_style: false,
    poll_interval_secs: 60
  }
}

fn unique_prefix(tag: &str) -> String {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or_default();
  format!("omnifuse-matrix/{tag}-{}-{nanos}", std::process::id())
}

struct NoProtection;
impl PathProtection for NoProtection {
  fn is_protected(&self, _path: &Path) -> bool {
    false
  }
}

async fn cleanup(backend: &S3Backend, dir: &Path, files: &[&str]) {
  for file in files {
    let _ = std::fs::remove_file(dir.join(file));
  }
  let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
  let _ = backend.sync(&paths).await;
}

#[tokio::test]
#[ignore = "requires MinIO (see module docs)"]
async fn capability_check_passes_on_minio() {
  let backend = S3Backend::new(minio_config(unique_prefix("capability")));
  let dir = tempfile::tempdir().expect("tempdir");
  // `init` invokes the capability gate inside `S3Session::attach`. If MinIO's OpenDAL profile
  // drops any required capability, this test fails loudly before any user-facing wiring.
  backend.init(dir.path()).await.expect("init");
}

#[tokio::test]
#[ignore = "requires MinIO (see module docs)"]
async fn create_uses_if_not_exists_and_records_etag() {
  let backend = S3Backend::new(minio_config(unique_prefix("create")));
  let dir = tempfile::tempdir().expect("tempdir");
  backend.init(dir.path()).await.expect("init");

  let file = PathBuf::from("a.txt");
  std::fs::write(dir.path().join(&file), "hello").expect("write");

  let result = backend.sync(&[file.clone()]).await.expect("sync");
  assert!(
    matches!(result, SyncResult::Success { synced_files: 1 }),
    "got {result:?}"
  );

  cleanup(&backend, dir.path(), &["a.txt"]).await;
}

#[tokio::test]
#[ignore = "requires MinIO (see module docs)"]
async fn stale_etag_triggers_text_merge_retry() {
  let prefix = unique_prefix("update");
  let primary = S3Backend::new(minio_config(prefix.clone()));
  let primary_dir = tempfile::tempdir().expect("primary tempdir");
  primary.init(primary_dir.path()).await.expect("primary init");

  // Seed via a sibling backend so the primary picks up a real ETag, not its own.
  let seeder = S3Backend::new(minio_config(prefix.clone()));
  let seed_dir = tempfile::tempdir().expect("seed tempdir");
  seeder.init(seed_dir.path()).await.expect("seed init");
  std::fs::write(seed_dir.path().join("doc.txt"), "aaa\nbbb\nccc\n").expect("seed write");
  seeder.sync(&[PathBuf::from("doc.txt")]).await.expect("seed sync");

  primary
    .refresh_remote(RemoteRefresh {
      protected_paths: &NoProtection,
      mode: RemoteApplyMode::ApplySafe
    })
    .await
    .expect("refresh primary");

  // Diverge: primary edits line 1, seeder edits line 3.
  std::fs::write(primary_dir.path().join("doc.txt"), "AAA\nbbb\nccc\n").expect("primary edit");
  std::fs::write(seed_dir.path().join("doc.txt"), "aaa\nbbb\nCCC\n").expect("seed edit");
  seeder
    .sync(&[PathBuf::from("doc.txt")])
    .await
    .expect("seed second sync");

  // Primary's manifest ETag is now stale; conditional update must report Precondition,
  // refetch remote, run the three-way merge, and retry with the merged ETag.
  let result = primary.sync(&[PathBuf::from("doc.txt")]).await.expect("primary sync");
  assert!(
    matches!(result, SyncResult::Success { synced_files: 1 }),
    "got {result:?}"
  );

  let merged = std::fs::read_to_string(primary_dir.path().join("doc.txt")).expect("merged");
  assert!(merged.contains("AAA"), "expected AAA in {merged}");
  assert!(merged.contains("CCC"), "expected CCC in {merged}");

  cleanup(&primary, primary_dir.path(), &["doc.txt"]).await;
}

#[tokio::test]
#[ignore = "requires MinIO (see module docs)"]
async fn binary_drift_reports_conflict() {
  let prefix = unique_prefix("binary");
  let primary = S3Backend::new(minio_config(prefix.clone()));
  let primary_dir = tempfile::tempdir().expect("primary tempdir");
  primary.init(primary_dir.path()).await.expect("primary init");

  let seeder = S3Backend::new(minio_config(prefix));
  let seed_dir = tempfile::tempdir().expect("seed tempdir");
  seeder.init(seed_dir.path()).await.expect("seed init");
  std::fs::write(seed_dir.path().join("blob.bin"), [0u8, 1, 2]).expect("seed write");
  seeder.sync(&[PathBuf::from("blob.bin")]).await.expect("seed sync");

  primary
    .refresh_remote(RemoteRefresh {
      protected_paths: &NoProtection,
      mode: RemoteApplyMode::ApplySafe
    })
    .await
    .expect("refresh primary");

  std::fs::write(primary_dir.path().join("blob.bin"), [0u8, 9, 2]).expect("primary edit");
  std::fs::write(seed_dir.path().join("blob.bin"), [0xff_u8, 0xfe, 0xfd]).expect("seed edit");
  seeder.sync(&[PathBuf::from("blob.bin")]).await.expect("seed update");

  let result = primary.sync(&[PathBuf::from("blob.bin")]).await.expect("primary sync");
  assert!(matches!(result, SyncResult::Conflict { .. }), "got {result:?}");

  // Resolve by accepting remote bytes, then delete.
  std::fs::write(primary_dir.path().join("blob.bin"), [0xff_u8, 0xfe, 0xfd]).expect("resolve");
  primary
    .sync(&[PathBuf::from("blob.bin")])
    .await
    .expect("post-resolve sync");
  cleanup(&primary, primary_dir.path(), &["blob.bin"]).await;
}

#[tokio::test]
#[ignore = "requires MinIO (see module docs)"]
async fn delete_propagates_and_refresh_sees_no_drift() {
  let backend = S3Backend::new(minio_config(unique_prefix("delete")));
  let dir = tempfile::tempdir().expect("tempdir");
  backend.init(dir.path()).await.expect("init");

  std::fs::write(dir.path().join("gone.txt"), "bye").expect("write");
  backend.sync(&[PathBuf::from("gone.txt")]).await.expect("sync create");

  std::fs::remove_file(dir.path().join("gone.txt")).expect("rm");
  backend.sync(&[PathBuf::from("gone.txt")]).await.expect("sync delete");

  let result = backend
    .refresh_remote(RemoteRefresh {
      protected_paths: &NoProtection,
      mode: RemoteApplyMode::ApplySafe
    })
    .await
    .expect("refresh");
  assert!(matches!(result, RemoteRefreshResult::Unchanged), "got {result:?}");
}
