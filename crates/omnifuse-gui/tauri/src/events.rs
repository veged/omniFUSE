//! Tauri event handler for VFS events.

use std::{path::Path, sync::Arc};

use omnifuse_core::events::{LogLevel, VfsEventHandler};
use tauri::{AppHandle, Emitter};

/// Event handler for Tauri.
///
/// Implements `VfsEventHandler` — converts core events
/// into Tauri IPC events for the frontend.
pub struct TauriEventHandler {
  /// Application handle.
  app: Arc<AppHandle>
}

impl TauriEventHandler {
  /// Create a new event handler.
  pub fn new(app: AppHandle) -> Self {
    Self { app: Arc::new(app) }
  }
}

impl VfsEventHandler for TauriEventHandler {
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
