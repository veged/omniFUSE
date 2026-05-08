//! Operational event integration tests.
//!
//! Run: `cargo test -p omnifuse-core --test operational_event_tests -- --nocapture`

#![allow(clippy::expect_used)]

use std::{path::PathBuf, sync::Arc, time::Duration};

use omnifuse_core::{
  FsEvent, OperationOutcome, OperationalEvent, SyncConfig, SyncEngine, SyncResult,
  test_utils::{MockBackend, TEST_TIMEOUT, TestEventHandler}
};

async fn safe_shutdown(engine: &SyncEngine, handle: tokio::task::JoinHandle<()>) {
  tokio::time::timeout(TEST_TIMEOUT, engine.shutdown())
    .await
    .expect("shutdown timed out")
    .expect("shutdown failed");

  tokio::time::timeout(TEST_TIMEOUT, handle)
    .await
    .expect("join timed out")
    .expect("join failed");
}

#[tokio::test]
async fn test_sync_engine_emits_sync_deferred_offline_event() {
  let backend = Arc::new(MockBackend::new());
  backend.set_sync_result(SyncResult::Offline);

  let events = Arc::new(TestEventHandler::new());
  let events_dyn: Arc<dyn omnifuse_core::events::VfsEventHandler> = events.clone();

  let config = SyncConfig {
    debounce_timeout_secs: 0,
    ..SyncConfig::default()
  };

  let (engine, handle) = SyncEngine::start(config, backend, events_dyn);
  engine
    .sender()
    .send(FsEvent::FileClosed(PathBuf::from("offline.md")))
    .await
    .expect("send file closed");

  tokio::time::sleep(Duration::from_millis(100)).await;

  assert!(
    events.operational_events().iter().any(|event| matches!(
      event,
      OperationalEvent::SyncFinished {
        outcome: OperationOutcome::Deferred,
        ..
      }
    )),
    "expected deferred sync operational event"
  );

  safe_shutdown(&engine, handle).await;
}

#[tokio::test]
async fn test_sync_engine_emits_remote_poll_failed_event() {
  let mut backend_inner = MockBackend::new();
  backend_inner.poll_interval_dur = Duration::from_millis(10);
  let backend = Arc::new(backend_inner);
  backend.set_refresh_error("network unavailable");

  let events = Arc::new(TestEventHandler::new());
  let events_dyn: Arc<dyn omnifuse_core::events::VfsEventHandler> = events.clone();

  let (engine, handle) = SyncEngine::start(SyncConfig::default(), backend, events_dyn);

  tokio::time::sleep(Duration::from_millis(50)).await;

  assert!(
    events
      .operational_events()
      .iter()
      .any(|event| matches!(event, OperationalEvent::RemotePollFailed { .. })),
    "expected remote poll failed operational event"
  );

  safe_shutdown(&engine, handle).await;
}
