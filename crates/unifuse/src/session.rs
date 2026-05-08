//! Session-oriented path filesystem API.

use std::{
  ffi::OsStr,
  path::{Path, PathBuf}
};

use crate::{DirEntry, FileAttr, FileHandle, FileType, FsError, OpenFlags, StatFs};

/// Filesystem mount specification.
#[derive(Debug, Clone)]
pub struct MountSpec {
  /// Mount point path.
  pub mountpoint: PathBuf,
  /// Filesystem name shown by the platform.
  pub fs_name: String,
  /// Allow access by users other than the mounting user.
  pub allow_other: bool,
  /// Mount as read-only.
  pub read_only: bool
}

impl MountSpec {
  /// Create a mount spec with default mount options.
  #[must_use]
  pub fn new(mountpoint: PathBuf) -> Self {
    Self {
      mountpoint,
      fs_name: "unifuse".to_string(),
      allow_other: false,
      read_only: false
    }
  }
}

/// Context passed to a filesystem session at start.
#[derive(Debug, Clone)]
pub struct MountContext {
  /// Mount specification.
  pub spec: MountSpec
}

impl MountContext {
  /// Create a mount context.
  #[must_use]
  pub fn new(spec: MountSpec) -> Self {
    Self { spec }
  }

  /// Create a lightweight test mount context.
  #[must_use]
  pub fn test(mountpoint: impl AsRef<Path>) -> Self {
    Self::new(MountSpec::new(mountpoint.as_ref().to_path_buf()))
  }
}

/// Reason why a filesystem session is stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmountReason {
  /// Normal unmount.
  Unmounted,
  /// Mount failed after session start.
  MountFailed,
  /// Runtime aborted the mount.
  Aborted
}

/// Result of a completed mount session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountExit {
  /// Reason why the mount session ended.
  pub reason: UnmountReason
}

/// Metadata for a path node.
#[derive(Debug, Clone)]
pub struct NodeMeta {
  /// Node path relative to the filesystem root.
  pub path: PathBuf,
  /// Node attributes.
  pub attr: FileAttr
}

impl NodeMeta {
  /// Create node metadata.
  #[must_use]
  pub fn new(path: PathBuf, attr: FileAttr) -> Self {
    Self { path, attr }
  }
}

/// Opened file or directory node.
#[derive(Debug, Clone)]
pub struct OpenedNode {
  /// Node path relative to the filesystem root.
  pub path: PathBuf,
  /// Open handle.
  pub handle: FileHandle,
  /// Node type.
  pub kind: FileType,
  /// Attributes observed while opening.
  pub attr: FileAttr
}

impl OpenedNode {
  /// Create an opened node descriptor.
  #[must_use]
  pub fn new(path: PathBuf, handle: FileHandle, kind: FileType, attr: FileAttr) -> Self {
    Self {
      path,
      handle,
      kind,
      attr
    }
  }
}

/// Open intent for a node.
#[derive(Debug, Clone, Copy)]
pub struct OpenIntent {
  /// Open flags.
  pub flags: OpenFlags
}

impl OpenIntent {
  /// Read-only open intent.
  #[must_use]
  pub const fn read_only() -> Self {
    Self {
      flags: OpenFlags::read_only()
    }
  }

  /// Write-only open intent.
  #[must_use]
  pub const fn write_only() -> Self {
    Self {
      flags: OpenFlags::write_only()
    }
  }

  /// Read-write open intent.
  #[must_use]
  pub const fn read_write() -> Self {
    Self {
      flags: OpenFlags::read_write()
    }
  }
}

/// Flush operation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushMode {
  /// Normal flush.
  Flush,
  /// Fsync flush.
  Fsync {
    /// Whether only data needs to be synced.
    datasync: bool
  }
}

/// Reason why an opened node is closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
  /// Normal file release.
  Released,
  /// Directory release.
  ReleasedDir,
  /// Cleanup during unmount.
  Cleanup
}

/// Directory page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirPageRequest {
  /// Offset of the first requested entry.
  pub offset: usize,
  /// Optional maximum number of entries.
  pub limit: Option<usize>
}

impl DirPageRequest {
  /// Request all entries from the beginning.
  #[must_use]
  pub const fn all() -> Self {
    Self { offset: 0, limit: None }
  }
}

/// A directory entry with optional metadata.
#[derive(Debug, Clone)]
pub struct DirEntryMeta {
  /// Entry name.
  pub name: String,
  /// Entry type.
  pub kind: FileType,
  /// Optional full node metadata.
  pub meta: Option<NodeMeta>
}

impl From<DirEntry> for DirEntryMeta {
  fn from(entry: DirEntry) -> Self {
    Self {
      name: entry.name,
      kind: entry.kind,
      meta: None
    }
  }
}

/// Directory page.
#[derive(Debug, Clone)]
pub struct DirPage {
  /// Page entries.
  pub entries: Vec<DirEntryMeta>,
  /// Offset for the next page, if more entries may exist.
  pub next_offset: Option<usize>
}

impl DirPage {
  /// Create a directory page with all entries.
  #[must_use]
  pub fn all(entries: Vec<DirEntryMeta>) -> Self {
    Self {
      entries,
      next_offset: None
    }
  }
}

/// Filesystem tree mutation.
#[derive(Debug)]
pub enum FsMutation<'a> {
  /// Create and open a file.
  Create {
    /// File path.
    path: &'a Path,
    /// Open flags.
    flags: OpenFlags,
    /// Unix mode.
    mode: u32
  },
  /// Create a directory.
  Mkdir {
    /// Directory path.
    path: &'a Path,
    /// Unix mode.
    mode: u32
  },
  /// Remove a file.
  Unlink {
    /// File path.
    path: &'a Path
  },
  /// Remove a directory.
  Rmdir {
    /// Directory path.
    path: &'a Path
  },
  /// Rename a file or directory.
  Rename {
    /// Old path.
    from: &'a Path,
    /// New path.
    to: &'a Path,
    /// Platform rename flags.
    flags: u32
  },
  /// Create a symlink.
  Symlink {
    /// Link target.
    target: &'a Path,
    /// Link path.
    link: &'a Path
  },
  /// Read a symlink target.
  Readlink {
    /// Link path.
    path: &'a Path
  },
  /// Set attributes.
  SetAttr {
    /// Node path.
    path: &'a Path,
    /// New size.
    size: Option<u64>,
    /// New access time.
    atime: Option<std::time::SystemTime>,
    /// New modification time.
    mtime: Option<std::time::SystemTime>,
    /// New Unix mode.
    mode: Option<u32>
  },
  /// Set an extended attribute.
  SetXattr {
    /// Node path.
    path: &'a Path,
    /// Attribute name.
    name: &'a OsStr,
    /// Attribute value.
    value: &'a [u8],
    /// Platform flags.
    flags: i32
  },
  /// Remove an extended attribute.
  RemoveXattr {
    /// Node path.
    path: &'a Path,
    /// Attribute name.
    name: &'a OsStr
  }
}

/// Session-oriented path filesystem trait.
pub trait SessionPathFs: Send + Sync + 'static {
  /// Mount session state.
  type MountState: Send + Sync + 'static;

  /// Start a mount session.
  fn start(&self, context: MountContext) -> impl Future<Output = Result<Self::MountState, FsError>> + Send;

  /// Stop a mount session.
  fn stop(&self, state: Self::MountState, reason: UnmountReason) -> impl Future<Output = Result<(), FsError>> + Send;

  /// Look up node metadata.
  fn lookup(&self, state: &Self::MountState, path: &Path) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;

  /// Open a node.
  fn open(
    &self,
    state: &Self::MountState,
    path: &Path,
    intent: OpenIntent
  ) -> impl Future<Output = Result<OpenedNode, FsError>> + Send;

  /// Read bytes into the caller-provided buffer.
  fn read_into(
    &self,
    file: &OpenedNode,
    offset: u64,
    out: &mut [u8]
  ) -> impl Future<Output = Result<usize, FsError>> + Send;

  /// Write bytes from the caller-provided buffer.
  fn write_from(
    &self,
    file: &OpenedNode,
    offset: u64,
    data: &[u8]
  ) -> impl Future<Output = Result<usize, FsError>> + Send;

  /// Flush an opened node.
  fn flush(&self, file: &OpenedNode, mode: FlushMode) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;

  /// Close an opened node.
  fn close(&self, file: OpenedNode, reason: CloseReason) -> impl Future<Output = Result<(), FsError>> + Send;

  /// Read a directory page.
  fn read_dir(
    &self,
    state: &Self::MountState,
    path: &Path,
    page: DirPageRequest
  ) -> impl Future<Output = Result<DirPage, FsError>> + Send;

  /// Apply a filesystem mutation.
  fn mutate(
    &self,
    state: &Self::MountState,
    mutation: FsMutation<'_>
  ) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;

  /// Get filesystem statistics.
  fn statfs(&self, state: &Self::MountState) -> impl Future<Output = Result<StatFs, FsError>> + Send;
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;

  #[derive(Default)]
  struct RecordingSessionFs {
    started: AtomicUsize,
    stopped: AtomicUsize
  }

  impl RecordingSessionFs {
    fn started_count(&self) -> usize {
      self.started.load(Ordering::SeqCst)
    }

    fn stopped_count(&self) -> usize {
      self.stopped.load(Ordering::SeqCst)
    }
  }

  impl SessionPathFs for RecordingSessionFs {
    type MountState = ();

    async fn start(&self, _context: MountContext) -> Result<Self::MountState, FsError> {
      self.started.fetch_add(1, Ordering::SeqCst);
      Ok(())
    }

    async fn stop(&self, _state: Self::MountState, _reason: UnmountReason) -> Result<(), FsError> {
      self.stopped.fetch_add(1, Ordering::SeqCst);
      Ok(())
    }

    async fn lookup(&self, _state: &Self::MountState, _path: &Path) -> Result<NodeMeta, FsError> {
      Err(FsError::NotSupported)
    }

    async fn open(&self, _state: &Self::MountState, _path: &Path, _intent: OpenIntent) -> Result<OpenedNode, FsError> {
      Err(FsError::NotSupported)
    }

    async fn read_into(&self, _file: &OpenedNode, _offset: u64, _out: &mut [u8]) -> Result<usize, FsError> {
      Err(FsError::NotSupported)
    }

    async fn write_from(&self, _file: &OpenedNode, _offset: u64, _data: &[u8]) -> Result<usize, FsError> {
      Err(FsError::NotSupported)
    }

    async fn flush(&self, file: &OpenedNode, _mode: FlushMode) -> Result<NodeMeta, FsError> {
      Ok(NodeMeta::new(file.path.clone(), file.attr.clone()))
    }

    async fn close(&self, _file: OpenedNode, _reason: CloseReason) -> Result<(), FsError> {
      Ok(())
    }

    async fn read_dir(
      &self,
      _state: &Self::MountState,
      _path: &Path,
      _page: DirPageRequest
    ) -> Result<DirPage, FsError> {
      Err(FsError::NotSupported)
    }

    async fn mutate(&self, _state: &Self::MountState, _mutation: FsMutation<'_>) -> Result<NodeMeta, FsError> {
      Err(FsError::NotSupported)
    }

    async fn statfs(&self, _state: &Self::MountState) -> Result<StatFs, FsError> {
      Err(FsError::NotSupported)
    }
  }

  #[tokio::test]
  async fn recording_session_fs_start_and_stop_are_called_on_shared_ref() {
    let fs = RecordingSessionFs::default();
    let state = fs.start(MountContext::test("/mnt/test")).await.expect("start");

    assert_eq!(fs.started_count(), 1);

    fs.stop(state, UnmountReason::Unmounted).await.expect("stop");

    assert_eq!(fs.stopped_count(), 1);
  }

  #[test]
  fn opened_node_carries_path_handle_kind_and_attr() {
    let attr = FileAttr::regular(5, 0o644);
    let node = OpenedNode::new(
      PathBuf::from("doc.md"),
      FileHandle(7),
      FileType::RegularFile,
      attr.clone()
    );

    assert_eq!(node.path.as_path(), Path::new("doc.md"));
    assert_eq!(node.handle, FileHandle(7));
    assert_eq!(node.attr.size, 5);
  }
}
