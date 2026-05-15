#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::PathBuf;

use omnifuse_core::{InitResult, SyncResult};
use omnifuse_s3::session::S3Session;
use opendal::{Operator, services};

fn memory_operator() -> Operator {
  Operator::new(services::Memory::default())
    .expect("memory builder")
    .finish()
}

#[tokio::test]
async fn init_downloads_objects_and_base_content() {
  let operator = memory_operator();
  operator.write("docs/readme.md", "hello").await.expect("seed");
  let dir = tempfile::tempdir().expect("tempdir");
  let session = S3Session::attach_operator_for_tests(operator, dir.path()).expect("session");

  let result = session.initialize().await.expect("init");

  assert!(matches!(result, InitResult::Fresh), "expected Fresh, got {result:?}");
  assert_eq!(
    std::fs::read_to_string(dir.path().join("docs/readme.md")).expect("read"),
    "hello"
  );
  assert_eq!(
    std::fs::read_to_string(dir.path().join(".vfs/s3/base/docs/readme.md")).expect("base"),
    "hello"
  );
}

#[tokio::test]
async fn sync_creates_new_object() {
  let operator = memory_operator();
  let dir = tempfile::tempdir().expect("tempdir");
  let session = S3Session::attach_operator_for_tests(operator.clone(), dir.path()).expect("session");
  session.initialize().await.expect("init");

  std::fs::write(dir.path().join("a.txt"), "hello").expect("local write");
  let result = session.sync_dirty(&[PathBuf::from("a.txt")]).await.expect("sync");

  assert!(
    matches!(result, SyncResult::Success { synced_files: 1 }),
    "got {result:?}"
  );
  let remote = operator.read("a.txt").await.expect("remote").to_vec();
  assert_eq!(remote, b"hello");
}

// Text auto-merge with conditional update + retry is exercised by the MinIO matrix in Task 9;
// the OpenDAL memory backend deliberately omits ETag, so `merge_remote_drift` cannot authorize
// a conditional PUT here.

#[tokio::test]
async fn sync_reports_binary_conflict_when_remote_changed() {
  let operator = memory_operator();
  operator.write("blob.bin", vec![0u8, 1, 2]).await.expect("seed");
  let dir = tempfile::tempdir().expect("tempdir");
  let session = S3Session::attach_operator_for_tests(operator.clone(), dir.path()).expect("session");
  session.initialize().await.expect("init");

  std::fs::write(dir.path().join("blob.bin"), [0u8, 9, 2]).expect("local edit");
  operator.write("blob.bin", vec![0u8, 1, 9]).await.expect("remote edit");

  let result = session.sync_dirty(&[PathBuf::from("blob.bin")]).await.expect("sync");

  assert!(matches!(result, SyncResult::Conflict { .. }), "got {result:?}");
}

#[tokio::test]
async fn sync_deletes_remote_when_local_removed() {
  let operator = memory_operator();
  let dir = tempfile::tempdir().expect("tempdir");
  let session = S3Session::attach_operator_for_tests(operator.clone(), dir.path()).expect("session");
  session.initialize().await.expect("init");

  std::fs::write(dir.path().join("gone.txt"), "bye").expect("local write");
  session.sync_dirty(&[PathBuf::from("gone.txt")]).await.expect("create");

  std::fs::remove_file(dir.path().join("gone.txt")).expect("rm");
  let result = session.sync_dirty(&[PathBuf::from("gone.txt")]).await.expect("delete");

  assert!(
    matches!(result, SyncResult::Success { synced_files: 1 }),
    "got {result:?}"
  );
  assert!(operator.stat("gone.txt").await.is_err());
}
