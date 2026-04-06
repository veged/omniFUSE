//! Tauri event handler for VFS events.

use std::{path::Path, sync::Arc};

use omnifuse_core::{
  OperationalEvent,
  events::{LogLevel, VfsEventHandler}
};
use tauri::{AppHandle, Emitter, Runtime};

/// Event handler for Tauri.
///
/// Implements `VfsEventHandler` — converts core events
/// into Tauri IPC events for the frontend.
pub struct TauriEventHandler<R: Runtime = tauri::Wry> {
  /// Application handle.
  app: Arc<AppHandle<R>>
}

impl<R: Runtime> TauriEventHandler<R> {
  /// Create a new event handler.
  pub fn new(app: AppHandle<R>) -> Self {
    Self { app: Arc::new(app) }
  }
}

impl<R: Runtime> VfsEventHandler for TauriEventHandler<R> {
  fn on_event(&self, event: &OperationalEvent) {
    let _ = self.app.emit("vfs:event", event);
  }

  fn on_mounted(&self, source: &str, mount_point: &Path) {
    let _ = self.app.emit(
      "vfs:mounted",
      serde_json::json!({
          "source": source,
          "mountPoint": mount_point.display().to_string()
      })
    );
  }

  fn on_unmounted(&self) {
    let _ = self.app.emit("vfs:unmounted", ());
  }

  fn on_error(&self, message: &str) {
    let _ = self.app.emit(
      "vfs:error",
      serde_json::json!({
          "message": message
      })
    );
  }

  fn on_log(&self, level: LogLevel, message: &str) {
    let _ = self.app.emit(
      "vfs:log",
      serde_json::json!({
          "level": format!("{level:?}").to_lowercase(),
          "message": message
      })
    );
  }

  fn on_file_written(&self, path: &Path, bytes: usize) {
    let _ = self.app.emit(
      "vfs:file-written",
      serde_json::json!({
          "path": path.display().to_string(),
          "bytes": bytes
      })
    );
  }

  fn on_file_dirty(&self, path: &Path) {
    let _ = self.app.emit(
      "vfs:file-dirty",
      serde_json::json!({
          "path": path.display().to_string()
      })
    );
  }

  fn on_file_created(&self, path: &Path) {
    let _ = self.app.emit(
      "vfs:file-created",
      serde_json::json!({
          "path": path.display().to_string()
      })
    );
  }

  fn on_file_deleted(&self, path: &Path) {
    let _ = self.app.emit(
      "vfs:file-deleted",
      serde_json::json!({
          "path": path.display().to_string()
      })
    );
  }

  fn on_file_renamed(&self, old_path: &Path, new_path: &Path) {
    let _ = self.app.emit(
      "vfs:file-renamed",
      serde_json::json!({
          "oldPath": old_path.display().to_string(),
          "newPath": new_path.display().to_string()
      })
    );
  }

  fn on_commit(&self, hash: &str, files_count: usize, message: &str) {
    let _ = self.app.emit(
      "vfs:commit",
      serde_json::json!({
          "hash": hash,
          "filesCount": files_count,
          "message": message
      })
    );
  }

  fn on_push(&self, items_count: usize) {
    let _ = self.app.emit(
      "vfs:push",
      serde_json::json!({
          "commitsCount": items_count
      })
    );
  }

  fn on_push_rejected(&self) {
    let _ = self.app.emit("vfs:push-rejected", ());
  }

  fn on_sync(&self, result: &str) {
    let _ = self.app.emit(
      "vfs:sync",
      serde_json::json!({
          "result": result
      })
    );
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::expect_used)]

  use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::mpsc::channel,
    time::{Duration, SystemTime, UNIX_EPOCH}
  };

  use omnifuse_core::{
    Backend, BufferConfig, Disposition, ErrorKind, EventSeverity, FuseMountOptions, InitResult, LoggingConfig,
    MountConfig, ObservabilitySession, OperationKind, OperationalEvent, RemoteChange, SyncConfig, SyncResult,
    run_mount
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

    fn poll_remote(&self) -> impl Future<Output = anyhow::Result<Vec<RemoteChange>>> + Send {
      async { Ok(Vec::new()) }
    }

    fn apply_remote(&self, _changes: Vec<RemoteChange>) -> impl Future<Output = anyhow::Result<()>> + Send {
      async { Ok(()) }
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

    fn classify_error(&self, error: &anyhow::Error) -> ErrorKind {
      if error.to_string().contains("init failed for test") {
        ErrorKind::InvalidConfig
      } else {
        ErrorKind::Internal
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
  fn test_on_event_emits_structured_operational_payload() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let (tx, rx) = channel();

    handle.listen_any("vfs:event", move |event: Event| {
      tx.send(serde_json::from_str::<Value>(event.payload()).expect("deserialize payload"))
        .expect("send payload to test");
    });

    let handler = TauriEventHandler::new(handle);
    let session = ObservabilitySession::new(
      "git",
      PathBuf::from("/mnt/omnifuse"),
      PathBuf::from("/tmp/omnifuse-cache")
    );
    let context = session.start_operation(OperationKind::Poll, 3, None);

    handler.on_event(&OperationalEvent::RemotePollFailed {
      context: context.clone(),
      severity: EventSeverity::Warn,
      error_kind: ErrorKind::Offline,
      message: "network unavailable".to_string(),
      disposition: Disposition::AutoRetrying,
      elapsed_ms: 250
    });

    let payload = rx
      .recv_timeout(Duration::from_secs(1))
      .expect("receive vfs:event payload");

    assert_eq!(payload["type"], "remote_poll_failed");
    assert_eq!(payload["context"]["mount_id"], session.mount_id());
    assert_eq!(payload["context"]["kind"], "poll");
    assert_eq!(payload["context"]["attempt"].as_u64(), Some(3));
    assert_eq!(payload["context"]["op_id"].as_u64(), Some(context.op_id()));
    assert_eq!(payload["error_kind"], "offline");
    assert_eq!(payload["disposition"], "auto_retrying");
    assert_eq!(payload["severity"], "warn");
    assert_eq!(payload["message"], "network unavailable");
    assert_eq!(payload["elapsed_ms"].as_u64(), Some(250));
  }

  #[tokio::test]
  async fn test_run_mount_failure_emits_tauri_error_and_mount_failed_event() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let (tx, rx) = channel::<(String, Value)>();

    let event_tx = tx.clone();
    handle.listen_any("vfs:event", move |event: Event| {
      event_tx
        .send((
          "vfs:event".to_string(),
          serde_json::from_str::<Value>(event.payload()).expect("deserialize vfs:event payload")
        ))
        .expect("send vfs:event payload to test");
    });

    handle.listen_any("vfs:error", move |event: Event| {
      tx.send((
        "vfs:error".to_string(),
        serde_json::from_str::<Value>(event.payload()).expect("deserialize vfs:error payload")
      ))
      .expect("send vfs:error payload to test");
    });

    let handler = TauriEventHandler::new(handle);
    let result = run_mount(test_mount_config("mount-failure"), InitFailingBackend, handler).await;

    assert!(result.is_err(), "run_mount should fail in this scenario");

    let mut payloads = Vec::new();
    while let Ok(payload) = rx.recv_timeout(Duration::from_millis(100)) {
      payloads.push(payload);
      if payloads.len() >= 4 {
        break;
      }
    }

    let mount_started = payloads
      .iter()
      .filter(|(name, _)| name == "vfs:event")
      .map(|(_, payload)| payload)
      .find(|payload| payload["type"] == "mount_started");
    assert!(mount_started.is_some(), "expected mount_started event");

    let mount_failed = payloads
      .iter()
      .filter(|(name, _)| name == "vfs:event")
      .map(|(_, payload)| payload)
      .find(|payload| payload["type"] == "mount_failed")
      .expect("expected mount_failed event");

    let error_payload = payloads
      .iter()
      .find(|(name, _)| name == "vfs:error")
      .map(|(_, payload)| payload)
      .expect("expected vfs:error payload");

    assert_eq!(mount_failed["context"]["kind"], "mount");
    assert_eq!(mount_failed["source"], "mount");
    assert_eq!(mount_failed["severity"], "error");
    assert_eq!(mount_failed["disposition"], "fatal");
    assert_eq!(error_payload["message"], mount_failed["message"]);

    if omnifuse_core::is_fuse_available() {
      assert_eq!(mount_failed["error_kind"], "invalid_config");
      assert_eq!(mount_failed["message"], "init failed for test");
    } else {
      let message = mount_failed["message"]
        .as_str()
        .expect("mount_failed message should be string");
      assert_eq!(mount_failed["error_kind"], "internal");
      assert!(
        message.contains("FUSE/WinFsp"),
        "expected FUSE availability error, got: {message}"
      );
    }
  }
}
