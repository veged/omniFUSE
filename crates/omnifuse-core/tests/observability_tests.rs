//! Observability primitives tests.
//!
//! Run: `cargo test -p omnifuse-core --test observability_tests`

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use omnifuse_core::{Kind, Level, LoggingConfig, Session, Source};

#[test]
fn test_logging_config_filter_directive_uses_level() {
  let config = LoggingConfig {
    level: "debug".to_string(),
    log_file: None
  };

  assert_eq!(config.filter_directive(), "debug");
}

#[test]
fn test_session_generates_mount_id_and_monotonic_operation_ids() {
  let session = Session::new("git", PathBuf::from("/mnt/repo"), PathBuf::from("/tmp/cache"));

  assert_eq!(session.backend(), "git");
  assert!(!session.mount_id().is_empty(), "mount_id should not be empty");
  assert_eq!(session.mount_point(), PathBuf::from("/mnt/repo").as_path());
  assert_eq!(session.local_dir(), PathBuf::from("/tmp/cache").as_path());

  let sync = session.op();
  let poll = session.op();
  let sync_event = session.op_event(&sync, Kind::SyncStart, Level::Info, Source::Sync);
  let poll_event = session.op_event(&poll, Kind::RemotePoll, Level::Info, Source::Sync);

  assert!(poll.id() > sync.id(), "operation ids should increase");
  assert_eq!(sync_event.mount_id, session.mount_id());
  assert_eq!(sync_event.op_id, Some(sync.id()));
  assert_eq!(poll_event.seq, sync_event.seq + 1);
}
