//! Cross-platform filesystem types.
//!
//! A common subset of POSIX stat and Windows `FILE_INFO`.
//! Platform adapters (rfuse3, winfsp) map from/to platform-specific types.

use std::time::SystemTime;

/// File object type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
  /// Regular file.
  RegularFile,
  /// Directory.
  Directory,
  /// Symbolic link.
  Symlink,
  /// Block device.
  BlockDevice,
  /// Character device.
  CharDevice,
  /// Named pipe (FIFO).
  NamedPipe,
  /// Unix socket.
  Socket
}

/// File attributes (cross-platform).
///
/// A common subset of POSIX stat and Windows `FILE_INFO`.
/// Platform adapters map from/to platform-specific types.
#[derive(Debug, Clone)]
pub struct FileAttr {
  /// File size in bytes.
  pub size: u64,
  /// Size in blocks (512 bytes).
  pub blocks: u64,
  /// Last access time.
  pub atime: SystemTime,
  /// Last modification time.
  pub mtime: SystemTime,
  /// Last metadata change time.
  pub ctime: SystemTime,
  /// Creation time (macOS/Windows).
  pub crtime: SystemTime,
  /// File type.
  pub kind: FileType,
  /// Access permissions (Unix mode: 0o644, 0o755). On Windows maps to readonly.
  pub perm: u16,
  /// Number of hard links.
  pub nlink: u32,
  /// User ID (Unix). On Windows = 0.
  pub uid: u32,
  /// Group ID (Unix). On Windows = 0.
  pub gid: u32,
  /// Device ID (for special files).
  pub rdev: u32,
  /// Flags (BSD flags on macOS / `FILE_ATTRIBUTE_*` on Windows).
  pub flags: u32
}

impl FileAttr {
  /// Create attributes for a regular file with typical values.
  #[must_use]
  pub fn regular(size: u64, perm: u16) -> Self {
    let now = SystemTime::now();
    Self {
      size,
      blocks: size.div_ceil(512),
      atime: now,
      mtime: now,
      ctime: now,
      crtime: now,
      kind: FileType::RegularFile,
      perm,
      nlink: 1,
      uid: 0,
      gid: 0,
      rdev: 0,
      flags: 0
    }
  }

  /// Create attributes for a directory.
  #[must_use]
  pub fn directory(perm: u16) -> Self {
    let now = SystemTime::now();
    Self {
      size: 0,
      blocks: 0,
      atime: now,
      mtime: now,
      ctime: now,
      crtime: now,
      kind: FileType::Directory,
      perm,
      nlink: 2,
      uid: 0,
      gid: 0,
      rdev: 0,
      flags: 0
    }
  }
}

/// Handle for an open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

/// Directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
  /// File name (without path).
  pub name: String,
  /// File type.
  pub kind: FileType
}

/// File open flags.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub struct OpenFlags {
  /// Open for reading.
  pub read: bool,
  /// Open for writing.
  pub write: bool,
  /// Append mode.
  pub append: bool,
  /// Truncate the file to zero length.
  pub truncate: bool,
  /// Create the file if it does not exist.
  pub create: bool,
  /// Error if the file already exists (together with create).
  pub exclusive: bool
}

impl OpenFlags {
  /// Flags for reading.
  #[must_use]
  pub const fn read_only() -> Self {
    Self {
      read: true,
      write: false,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }

  /// Flags for writing.
  #[must_use]
  pub const fn write_only() -> Self {
    Self {
      read: false,
      write: true,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }

  /// Flags for reading and writing.
  #[must_use]
  pub const fn read_write() -> Self {
    Self {
      read: true,
      write: true,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }
}

/// Filesystem information.
#[derive(Debug, Clone)]
pub struct StatFs {
  /// Total number of blocks.
  pub blocks: u64,
  /// Free blocks.
  pub bfree: u64,
  /// Available blocks (for unprivileged users).
  pub bavail: u64,
  /// Total number of inodes.
  pub files: u64,
  /// Free inodes.
  pub ffree: u64,
  /// Block size in bytes.
  pub bsize: u32,
  /// Maximum file name length.
  pub namelen: u32
}

/// FUSE operation errors.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
  /// File or directory not found.
  #[error("not found")]
  NotFound,
  /// Permission denied.
  #[error("permission denied")]
  PermissionDenied,
  /// File or directory already exists.
  #[error("already exists")]
  AlreadyExists,
  /// Expected a directory, but it is not one.
  #[error("not a directory")]
  NotADirectory,
  /// Path is a directory (when a file was expected).
  #[error("is a directory")]
  IsADirectory,
  /// Directory is not empty (on removal).
  #[error("directory not empty")]
  NotEmpty,
  /// Operation not supported.
  #[error("not supported")]
  NotSupported,
  /// I/O error.
  #[error(transparent)]
  Io(#[from] std::io::Error),
  /// Other error.
  #[error("{0}")]
  Other(String)
}

impl FsError {
  /// Convert to libc errno for FUSE.
  #[cfg(unix)]
  #[must_use]
  pub fn to_errno(&self) -> i32 {
    match self {
      Self::NotFound => libc::ENOENT,
      Self::PermissionDenied => libc::EACCES,
      Self::AlreadyExists => libc::EEXIST,
      Self::NotADirectory => libc::ENOTDIR,
      Self::IsADirectory => libc::EISDIR,
      Self::NotEmpty => libc::ENOTEMPTY,
      Self::NotSupported => libc::ENOSYS,
      Self::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
      Self::Other(_) => libc::EIO
    }
  }

  /// Convert to NTSTATUS code for WinFsp.
  #[cfg(windows)]
  #[must_use]
  pub fn to_ntstatus(&self) -> windows::Win32::Foundation::NTSTATUS {
    use windows::Win32::Foundation::{
      NTSTATUS, STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY,
      STATUS_FILE_IS_A_DIRECTORY, STATUS_NOT_A_DIRECTORY, STATUS_NOT_SUPPORTED,
      STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_UNSUCCESSFUL
    };

    match self {
      Self::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
      Self::PermissionDenied => STATUS_ACCESS_DENIED,
      Self::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
      Self::NotADirectory => STATUS_NOT_A_DIRECTORY,
      Self::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
      Self::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
      Self::NotSupported => STATUS_NOT_SUPPORTED,
      Self::Io(_) | Self::Other(_) => STATUS_UNSUCCESSFUL
    }
  }
}
