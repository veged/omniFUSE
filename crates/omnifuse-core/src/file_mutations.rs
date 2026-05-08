//! File mutation pipeline primitives.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::sync_engine::FsEvent;

/// Non-blocking sink for file mutation events consumed by `SyncEngine`.
pub struct DirtySink {
  tx: mpsc::Sender<FsEvent>
}

impl DirtySink {
  /// Create a dirty event sink.
  #[must_use]
  pub const fn new(tx: mpsc::Sender<FsEvent>) -> Self {
    Self { tx }
  }

  /// Notify that a file was modified.
  pub fn mark_modified(&self, path: PathBuf) -> DirtySendResult {
    self.send(FsEvent::FileModified(path))
  }

  /// Notify that a file was closed.
  pub fn mark_closed(&self, path: PathBuf) -> DirtySendResult {
    self.send(FsEvent::FileClosed(path))
  }

  fn send(&self, event: FsEvent) -> DirtySendResult {
    match self.tx.try_send(event) {
      Ok(()) => DirtySendResult::Sent,
      Err(_) => DirtySendResult::Dropped
    }
  }
}

/// Result of sending a dirty event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtySendResult {
  /// Event was queued.
  Sent,
  /// Event was dropped because the queue cannot accept it now.
  Dropped
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn dirty_sink_sends_modified_and_closed_events() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let sink = DirtySink::new(tx);

    sink.mark_modified(PathBuf::from("a.md"));
    sink.mark_closed(PathBuf::from("a.md"));

    assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("a.md")));
    assert!(matches!(rx.recv().await, Some(FsEvent::FileClosed(path)) if path == PathBuf::from("a.md")));
  }

  #[tokio::test]
  async fn dirty_sink_reports_full_queue_without_panic() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let sink = DirtySink::new(tx);

    sink.mark_modified(PathBuf::from("a.md"));
    let result = sink.mark_modified(PathBuf::from("b.md"));

    assert!(matches!(result, DirtySendResult::Dropped));
  }
}
