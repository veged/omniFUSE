#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use omnifuse_core::InitResult;
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
