//! Tauri bridge for product events.

use std::sync::Arc;

use omnifuse_core::{Event, Sink};
use tauri::{AppHandle, Emitter, Runtime};

/// Tauri event bridge.
pub struct TauriEventHandler<R: Runtime = tauri::Wry> {
  app: Arc<AppHandle<R>>
}

impl<R: Runtime> TauriEventHandler<R> {
  /// Create a new event bridge.
  pub fn new(app: AppHandle<R>) -> Self {
    Self { app: Arc::new(app) }
  }
}

impl<R: Runtime> Sink for TauriEventHandler<R> {
  fn emit(&self, event: Event) {
    let _ = self.app.emit("omnifuse:event", event);
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::expect_used)]
  #![allow(clippy::manual_async_fn)]

  use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::mpsc::channel,
    time::{Duration, SystemTime, UNIX_EPOCH}
  };

  use omnifuse_core::{
    Action, Backend, BufferConfig, Code, EventError, FuseMountOptions, InitResult, Kind, Level, LoggingConfig,
    MountConfig, RemoteRefresh, RemoteRefreshResult, Session, Source, SyncConfig, SyncResult, run_mount
  };
  use serde_json::Value;
  use tauri::{Event, Listener};

  use super::*;

  struct InitFailingBackend;

  impl Backend for InitFailingBackend {
    fn init(&self, _local_dir: &Path) -> impl Future<Output = anyhow::Result<InitResult>> + Send {
      async { anyhow::bail!("init failed for test") }
    }

    fn sync(&self, _dirty_files: &[PathBuf]) -> impl Future<Output = anyhow::Result<SyncResult>> + Send {
      async { Ok(SyncResult::Success { synced_files: 0 }) }
    }

    fn refresh_remote(
      &self,
      _request: RemoteRefresh<'_>
    ) -> impl Future<Output = anyhow::Result<RemoteRefreshResult>> + Send {
      async { Ok(RemoteRefreshResult::Unchanged) }
    }

    fn should_track(&self, _path: &Path) -> bool {
      true
    }

    fn poll_interval(&self) -> Duration {
      Duration::from_secs(60)
    }

    fn is_online(&self) -> impl Future<Output = bool> + Send {
      async { true }
    }

    fn name(&self) -> &'static str {
      "test"
    }

    fn classify_error(&self, error: &anyhow::Error) -> Code {
      if error.to_string().contains("init failed for test") {
        Code::InvalidConfig
      } else {
        Code::Internal
      }
    }
  }

  fn test_mount_config(name: &str) -> MountConfig {
    let unique = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("current time after epoch")
      .as_nanos();
    let root = std::env::temp_dir().join(format!("omnifuse-gui-{name}-{unique}"));

    MountConfig {
      mount_point: root.join("mount"),
      local_dir: root.join("local"),
      sync: SyncConfig::default(),
      buffer: BufferConfig::default(),
      mount_options: FuseMountOptions {
        fs_name: "omnifuse-test".to_string(),
        allow_other: false,
        read_only: false
      },
      logging: LoggingConfig::default()
    }
  }

  #[test]
  fn test_emit_product_event_payload() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let (tx, rx) = channel();

    handle.listen_any("omnifuse:event", move |event: Event| {
      tx.send(serde_json::from_str::<Value>(event.payload()).expect("deserialize payload"))
        .expect("send payload to test");
    });

    let handler = TauriEventHandler::new(handle);
    let session = Session::new(
      "git",
      PathBuf::from("/mnt/omnifuse"),
      PathBuf::from("/tmp/omnifuse-cache")
    );
    let op = session.op();

    handler.emit(
      session
        .op_event(&op, Kind::RemoteFail, Level::Warn, Source::Sync)
        .data(serde_json::json!({
            "elapsedMs": 250
        }))
        .error(EventError::new("network unavailable", Code::Offline, Action::Wait))
    );

    let payload = rx
      .recv_timeout(Duration::from_secs(1))
      .expect("receive omnifuse:event payload");

    assert_eq!(payload["version"].as_u64(), Some(1));
    assert_eq!(payload["kind"], "remote.fail");
    assert_eq!(payload["mountId"], session.mount_id());
    assert_eq!(payload["opId"].as_u64(), Some(op.id()));
    assert_eq!(payload["source"], "sync");
    assert_eq!(payload["level"], "warn");
    assert_eq!(payload["data"]["elapsedMs"].as_u64(), Some(250));
    assert_eq!(payload["error"]["code"], "offline");
    assert_eq!(payload["error"]["message"], "network unavailable");
    assert_eq!(payload["error"]["action"], "wait");
  }

  #[tokio::test]
  async fn test_run_mount_failure_emits_mount_fail_event() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let (tx, rx) = channel::<Value>();

    handle.listen_any("omnifuse:event", move |event: Event| {
      tx.send(serde_json::from_str::<Value>(event.payload()).expect("deserialize payload"))
        .expect("send payload to test");
    });

    let handler = TauriEventHandler::new(handle);
    let result = run_mount(test_mount_config("mount-failure"), InitFailingBackend, handler).await;

    assert!(result.is_err(), "run_mount should fail in this scenario");

    let mut payloads = Vec::new();
    while let Ok(payload) = rx.recv_timeout(Duration::from_millis(100)) {
      payloads.push(payload);
      if payloads.len() >= 3 {
        break;
      }
    }

    let mount_start = payloads.iter().find(|payload| payload["kind"] == "mount.start");
    assert!(mount_start.is_some(), "expected mount.start event");

    let mount_fail = payloads
      .iter()
      .find(|payload| payload["kind"] == "mount.fail")
      .expect("expected mount.fail event");

    assert_eq!(mount_fail["source"], "mount");
    assert_eq!(mount_fail["level"], "error");
    assert_eq!(mount_fail["error"]["action"], "stop");

    if omnifuse_core::is_fuse_available() {
      assert_eq!(mount_fail["error"]["code"], "invalid_config");
      assert_eq!(mount_fail["error"]["message"], "init failed for test");
    } else {
      let message = mount_fail["error"]["message"]
        .as_str()
        .expect("mount_fail message should be string");
      assert_eq!(mount_fail["error"]["code"], "internal");
      assert!(
        message.contains("FUSE/WinFsp"),
        "expected FUSE availability error, got: {message}"
      );
    }
  }
}
