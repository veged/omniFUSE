//! File mutation pipeline primitives.

use std::{
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering}
  },
  time::SystemTime
};

use tokio::sync::mpsc;
use unifuse::{FileAttr, FileType, FsError};

use crate::{
  Disposition, ErrorSource, ObservabilitySession, OperationKind, OperationalEvent,
  buffer::{FileBuffer, FileBufferManager},
  events::VfsEventHandler,
  sync_engine::FsEvent
};

/// Coordinates buffer, disk, dirty events and user-facing file events.
pub struct FileMutationPipeline {
  local_dir: PathBuf,
  buffer_manager: Arc<FileBufferManager>,
  dirty_sink: DirtySink,
  events: Arc<dyn VfsEventHandler>,
  session: Arc<ObservabilitySession>
}

impl FileMutationPipeline {
  /// Create a file mutation pipeline.
  #[must_use]
  pub const fn new(
    local_dir: PathBuf,
    buffer_manager: Arc<FileBufferManager>,
    dirty_sink: DirtySink,
    events: Arc<dyn VfsEventHandler>,
    session: Arc<ObservabilitySession>
  ) -> Self {
    Self {
      local_dir,
      buffer_manager,
      dirty_sink,
      events,
      session
    }
  }

  /// Open an existing file mutation session.
  ///
  /// # Errors
  ///
  /// Returns an error when the local file cannot be read into the buffer.
  pub async fn open_file(self: &Arc<Self>, path: &Path) -> Result<OpenFileMutation, FsError> {
    let full_path = self.full_path(path);
    let buffer = self.buffer_manager.get_or_load(&full_path).await.map_err(FsError::Io)?;

    Ok(OpenFileMutation {
      path: path.to_path_buf(),
      full_path,
      buffer,
      pipeline: Arc::clone(self),
      dirty_notified: AtomicBool::new(false)
    })
  }

  /// Create a file, cache an empty buffer and start its mutation session.
  ///
  /// # Errors
  ///
  /// Returns an error when the local file cannot be created or inspected.
  pub async fn create_file(self: &Arc<Self>, path: &Path, mode: u32) -> Result<(OpenFileMutation, FileAttr), FsError> {
    let full_path = self.full_path(path);
    tokio::fs::write(&full_path, b"").await.map_err(FsError::Io)?;

    #[cfg(unix)]
    set_mode_unix(&full_path, mode)?;

    let buffer = self.buffer_manager.cache(&full_path, Vec::new()).await;
    self.mark_modified(path);
    self.events.on_file_created(path);

    let meta = tokio::fs::symlink_metadata(&full_path).await.map_err(FsError::Io)?;
    let file = OpenFileMutation {
      path: path.to_path_buf(),
      full_path,
      buffer,
      pipeline: Arc::clone(self),
      dirty_notified: AtomicBool::new(true)
    };

    Ok((file, metadata_to_attr(&meta)))
  }

  /// Remove a file from disk and from the buffer cache.
  ///
  /// # Errors
  ///
  /// Returns an error when the local file cannot be removed.
  pub async fn unlink(&self, path: &Path) -> Result<(), FsError> {
    let full_path = self.full_path(path);
    self.buffer_manager.remove(&full_path).await;
    tokio::fs::remove_file(&full_path).await.map_err(FsError::Io)?;

    self.queue_modified(path);
    self.events.on_file_deleted(path);
    Ok(())
  }

  /// Rename a file or directory and mark both paths dirty.
  ///
  /// # Errors
  ///
  /// Returns an error when the local path cannot be renamed.
  pub async fn rename(&self, from: &Path, to: &Path) -> Result<(), FsError> {
    let full_from = self.full_path(from);
    let full_to = self.full_path(to);

    tokio::fs::rename(&full_from, &full_to).await.map_err(FsError::Io)?;
    self.buffer_manager.remove(&full_from).await;

    self.queue_modified(from);
    self.queue_modified(to);
    self.events.on_file_renamed(from, to);
    Ok(())
  }

  /// Create a symbolic link and mark the link path dirty.
  ///
  /// # Errors
  ///
  /// Returns an error when symlinks are unsupported or the link cannot be created.
  pub async fn symlink(&self, target: &Path, link: &Path) -> Result<FileAttr, FsError> {
    let full_link = self.full_path(link);

    #[cfg(unix)]
    tokio::fs::symlink(target, &full_link).await.map_err(FsError::Io)?;

    #[cfg(not(unix))]
    return Err(FsError::NotSupported);

    let meta = tokio::fs::symlink_metadata(&full_link).await.map_err(FsError::Io)?;

    self.queue_modified(link);
    Ok(metadata_to_attr(&meta))
  }

  fn full_path(&self, path: &Path) -> PathBuf {
    self.local_dir.join(path)
  }

  fn queue_modified(&self, path: &Path) {
    if matches!(
      self.dirty_sink.mark_modified(path.to_path_buf()),
      DirtySendResult::Dropped
    ) {
      self.emit_queue_full_warning();
    }
  }

  fn mark_modified(&self, path: &Path) {
    self.queue_modified(path);
    self.emit_file_marked_dirty(path);
  }

  fn mark_closed(&self, path: &Path) {
    if matches!(
      self.dirty_sink.mark_closed(path.to_path_buf()),
      DirtySendResult::Dropped
    ) {
      self.emit_queue_full_warning();
    }
  }

  fn emit_queue_full_warning(&self) {
    let context = self.session.start_operation(OperationKind::File, 1, None);
    self.events.on_event(&OperationalEvent::UserVisibleWarning {
      context,
      message: "sync event queue is full".to_string(),
      source: ErrorSource::Vfs,
      disposition: Disposition::Retryable
    });
  }

  fn emit_file_marked_dirty(&self, path: &Path) {
    let context = self
      .session
      .start_operation(OperationKind::File, 1, Some(path.to_path_buf()));
    self.events.on_event(&OperationalEvent::FileMarkedDirty {
      context,
      path: path.to_path_buf()
    });
  }

  fn emit_file_flushed(&self, path: &Path) {
    let context = self
      .session
      .start_operation(OperationKind::File, 1, Some(path.to_path_buf()));
    self.events.on_event(&OperationalEvent::FileFlushed {
      context,
      path: path.to_path_buf()
    });
  }
}

/// Mutation session for one opened file.
pub struct OpenFileMutation {
  path: PathBuf,
  full_path: PathBuf,
  buffer: Arc<FileBuffer>,
  pipeline: Arc<FileMutationPipeline>,
  dirty_notified: AtomicBool
}

impl OpenFileMutation {
  /// Read bytes from the open file buffer.
  pub async fn read(&self, offset: u64, size: u32) -> Result<Vec<u8>, FsError> {
    Ok(self.buffer.read(offset, size).await)
  }

  /// Write bytes into the open file buffer and notify the sync pipeline.
  ///
  /// # Errors
  ///
  /// This operation currently cannot fail, but keeps `FsError` for parity with VFS calls.
  pub async fn write(&self, offset: u64, data: &[u8]) -> Result<u32, FsError> {
    let written = self.buffer.write(offset, data).await;
    self.dirty_notified.store(true, Ordering::SeqCst);
    self.pipeline.mark_modified(&self.path);
    self.pipeline.events.on_file_written(&self.path, written);

    #[allow(clippy::cast_possible_truncation)]
    let written_u32 = written as u32;
    Ok(written_u32)
  }

  /// Truncate the open file in buffer and on disk.
  ///
  /// # Errors
  ///
  /// Returns an error when the local file cannot be opened, truncated or inspected.
  pub async fn truncate(&self, size: u64) -> Result<FileAttr, FsError> {
    self.buffer.truncate(size).await;

    let file = tokio::fs::OpenOptions::new()
      .write(true)
      .open(&self.full_path)
      .await
      .map_err(FsError::Io)?;
    file.set_len(size).await.map_err(FsError::Io)?;

    self.dirty_notified.store(true, Ordering::SeqCst);
    self.pipeline.mark_modified(&self.path);

    let meta = tokio::fs::symlink_metadata(&self.full_path)
      .await
      .map_err(FsError::Io)?;
    Ok(metadata_to_attr(&meta))
  }

  /// Flush pending buffer content to disk.
  ///
  /// # Errors
  ///
  /// Returns an error when the buffer cannot be written to disk.
  pub async fn flush(&self) -> Result<(), FsError> {
    self
      .pipeline
      .buffer_manager
      .flush(&self.full_path)
      .await
      .map_err(FsError::Io)?;
    self.pipeline.emit_file_flushed(&self.path);
    Ok(())
  }

  /// Flush the open file and notify the sync engine that it was closed.
  ///
  /// # Errors
  ///
  /// Returns an error when flushing fails.
  pub async fn close(&self) -> Result<(), FsError> {
    self.flush().await?;
    self.pipeline.mark_closed(&self.path);
    Ok(())
  }
}

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

/// Convert `tokio::fs::Metadata` to `FileAttr`.
#[must_use]
pub(crate) fn metadata_to_attr(meta: &std::fs::Metadata) -> FileAttr {
  let kind = if meta.is_dir() {
    FileType::Directory
  } else if meta.is_symlink() {
    FileType::Symlink
  } else {
    FileType::RegularFile
  };

  let now = SystemTime::now();

  FileAttr {
    size: meta.len(),
    blocks: meta.len().div_ceil(512),
    atime: meta.accessed().unwrap_or(now),
    mtime: meta.modified().unwrap_or(now),
    ctime: meta.modified().unwrap_or(now),
    crtime: meta.created().unwrap_or(now),
    kind,
    perm: unix_perm(meta),
    nlink: unix_nlink(meta),
    uid: unix_uid(meta),
    gid: unix_gid(meta),
    rdev: 0,
    flags: 0
  }
}

#[cfg(unix)]
fn unix_perm(meta: &std::fs::Metadata) -> u16 {
  use std::os::unix::fs::MetadataExt;
  #[allow(clippy::cast_possible_truncation)]
  let perm = meta.mode() as u16 & 0o7777;
  perm
}

#[cfg(not(unix))]
const fn unix_perm(meta: &std::fs::Metadata) -> u16 {
  if meta.permissions().readonly() { 0o444 } else { 0o644 }
}

#[cfg(unix)]
fn unix_nlink(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  #[allow(clippy::cast_possible_truncation)]
  let nlink = meta.nlink() as u32;
  nlink
}

#[cfg(not(unix))]
const fn unix_nlink(_meta: &std::fs::Metadata) -> u32 {
  1
}

#[cfg(unix)]
fn unix_uid(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  meta.uid()
}

#[cfg(not(unix))]
const fn unix_uid(_meta: &std::fs::Metadata) -> u32 {
  0
}

#[cfg(unix)]
fn unix_gid(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::MetadataExt;
  meta.gid()
}

#[cfg(not(unix))]
const fn unix_gid(_meta: &std::fs::Metadata) -> u32 {
  0
}

#[cfg(unix)]
fn set_mode_unix(path: &Path, mode: u32) -> Result<(), FsError> {
  use std::os::unix::fs::PermissionsExt;
  let perms = std::fs::Permissions::from_mode(mode);
  std::fs::set_permissions(path, perms).map_err(FsError::Io)
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::{BufferConfig, events::VfsEventHandler, test_utils::TestEventHandler};

  fn test_pipeline(
    local_dir: &Path
  ) -> (
    Arc<FileMutationPipeline>,
    mpsc::Receiver<FsEvent>,
    Arc<TestEventHandler>
  ) {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let events = Arc::new(TestEventHandler::new());
    let events_dyn: Arc<dyn VfsEventHandler> = events.clone();
    let session = Arc::new(ObservabilitySession::new(
      "test",
      PathBuf::new(),
      local_dir.to_path_buf()
    ));
    let pipeline = Arc::new(FileMutationPipeline::new(
      local_dir.to_path_buf(),
      Arc::new(FileBufferManager::new(BufferConfig::default())),
      DirtySink::new(tx),
      events_dyn,
      session
    ));

    (pipeline, rx, events)
  }

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

  #[tokio::test]
  async fn open_file_mutation_writes_flushes_and_closes() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("doc.md");
    tokio::fs::write(&path, "").await.expect("seed");
    let (pipeline, mut rx, events) = test_pipeline(tmp.path());

    let file = pipeline.open_file(Path::new("doc.md")).await.expect("open");
    assert_eq!(file.write(0, b"hello").await.expect("write"), 5);
    file.flush().await.expect("flush");
    file.close().await.expect("close");

    assert_eq!(tokio::fs::read_to_string(&path).await.expect("read"), "hello");
    assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("doc.md")));
    assert!(matches!(rx.recv().await, Some(FsEvent::FileClosed(path)) if path == PathBuf::from("doc.md")));
    assert!(
      events
        .written_calls
        .lock()
        .expect("lock")
        .iter()
        .any(|(path, bytes)| path == Path::new("doc.md") && *bytes == 5)
    );
  }

  #[tokio::test]
  async fn close_flushes_dirty_buffer_once() {
    let tmp = tempfile::tempdir().expect("tmp");
    tokio::fs::write(tmp.path().join("doc.md"), "").await.expect("seed");
    let (pipeline, _rx, _events) = test_pipeline(tmp.path());

    let file = pipeline.open_file(Path::new("doc.md")).await.expect("open");
    file.write(0, b"hello").await.expect("write");
    file.close().await.expect("close");

    assert_eq!(
      tokio::fs::read_to_string(tmp.path().join("doc.md"))
        .await
        .expect("read"),
      "hello"
    );
  }

  #[tokio::test]
  async fn create_file_caches_empty_buffer_and_marks_dirty() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (pipeline, mut rx, events) = test_pipeline(tmp.path());

    let (_file, attr) = pipeline.create_file(Path::new("new.md"), 0o644).await.expect("create");

    assert_eq!(attr.kind, FileType::RegularFile);
    assert!(tmp.path().join("new.md").exists());
    assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("new.md")));
    assert!(
      events
        .created_calls
        .lock()
        .expect("lock")
        .iter()
        .any(|path| path == Path::new("new.md"))
    );
  }

  #[tokio::test]
  async fn rename_removes_old_buffer_and_marks_both_paths_dirty() {
    let tmp = tempfile::tempdir().expect("tmp");
    tokio::fs::write(tmp.path().join("old.md"), "content")
      .await
      .expect("seed");
    let (pipeline, mut rx, events) = test_pipeline(tmp.path());
    let _file = pipeline.open_file(Path::new("old.md")).await.expect("open");

    pipeline
      .rename(Path::new("old.md"), Path::new("new.md"))
      .await
      .expect("rename");

    assert!(!tmp.path().join("old.md").exists());
    assert!(tmp.path().join("new.md").exists());
    assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("old.md")));
    assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("new.md")));
    assert_eq!(events.renamed_calls.lock().expect("lock").len(), 1);
  }
}
