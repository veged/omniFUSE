//! Observability primitives tests.
//!
//! Run: `cargo test -p omnifuse-core --test observability_tests`

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use omnifuse_core::{LoggingConfig, ObservabilitySession, OperationKind};

#[test]
fn test_logging_config_filter_directive_uses_level() {
  let config = LoggingConfig {
    level: "debug".to_string(),
    log_file: None
  };

  assert_eq!(config.filter_directive(), "debug");
}

#[test]
fn test_observability_session_generates_mount_id_and_monotonic_operation_ids() {
  let session = ObservabilitySession::new("git", PathBuf::from("/mnt/repo"), PathBuf::from("/tmp/cache"));

  assert_eq!(session.backend(), "git");
  assert!(!session.mount_id().is_empty(), "mount_id should not be empty");
  assert_eq!(session.mount_point(), PathBuf::from("/mnt/repo").as_path());
  assert_eq!(session.local_dir(), PathBuf::from("/tmp/cache").as_path());

  let sync = session.start_operation(OperationKind::Sync, 1, Some(PathBuf::from("README.md")));
  let poll = session.start_operation(OperationKind::Poll, 2, None);

  assert_eq!(sync.mount_id(), session.mount_id());
  assert_eq!(sync.kind(), OperationKind::Sync);
  assert_eq!(sync.attempt(), 1);
  assert_eq!(sync.path(), Some(PathBuf::from("README.md").as_path()));

  assert_eq!(poll.mount_id(), session.mount_id());
  assert_eq!(poll.kind(), OperationKind::Poll);
  assert_eq!(poll.attempt(), 2);
  assert!(poll.path().is_none());
  assert!(poll.op_id() > sync.op_id(), "operation ids should increase");
}
