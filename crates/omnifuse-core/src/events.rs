//! VFS event handler for UI (Tauri, CLI, logs).

use std::path::Path;

/// Log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
  /// Debug information.
  Debug,
  /// Informational message.
  Info,
  /// Warning.
  Warn,
  /// Error.
  Error
}

/// Event handler for UI (Tauri, CLI, logs).
///
/// All methods have default implementations (noop), so
/// the implementor can override only the ones needed.
#[allow(unused_variables)]
pub trait VfsEventHandler: Send + Sync + 'static {
  /// Filesystem mounted.
  fn on_mounted(&self, source: &str, mount_point: &Path) {}

  /// Filesystem unmounted.
  fn on_unmounted(&self) {}

  /// An error occurred.
  fn on_error(&self, message: &str) {}

  /// Log message.
  fn on_log(&self, level: LogLevel, message: &str) {}

  /// File written.
  fn on_file_written(&self, path: &Path, bytes: usize) {}

  /// File marked as dirty.
  fn on_file_dirty(&self, path: &Path) {}

  /// File created.
  fn on_file_created(&self, path: &Path) {}

  /// File deleted.
  fn on_file_deleted(&self, path: &Path) {}

  /// File renamed.
  fn on_file_renamed(&self, old_path: &Path, new_path: &Path) {}

  /// Commit performed.
  fn on_commit(&self, hash: &str, files_count: usize, message: &str) {}

  /// Push performed.
  fn on_push(&self, items_count: usize) {}

  /// Push rejected.
  fn on_push_rejected(&self) {}

  /// Synchronization completed.
  fn on_sync(&self, result: &str) {}
}

/// No-op event handler (for tests and CLI without UI).
pub struct NoopEventHandler;

impl VfsEventHandler for NoopEventHandler {}
