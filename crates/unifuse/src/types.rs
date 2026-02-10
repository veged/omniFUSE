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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[test]
  fn test_fs_error_to_errno() {
    assert_eq!(FsError::NotFound.to_errno(), libc::ENOENT);
    assert_eq!(FsError::PermissionDenied.to_errno(), libc::EACCES);
    assert_eq!(FsError::AlreadyExists.to_errno(), libc::EEXIST);
    assert_eq!(FsError::NotADirectory.to_errno(), libc::ENOTDIR);
    assert_eq!(FsError::IsADirectory.to_errno(), libc::EISDIR);
    assert_eq!(FsError::NotEmpty.to_errno(), libc::ENOTEMPTY);
    assert_eq!(FsError::NotSupported.to_errno(), libc::ENOSYS);
    assert_eq!(FsError::Other("err".to_string()).to_errno(), libc::EIO);
  }

  #[test]
  fn test_fs_error_display() {
    assert_eq!(FsError::NotFound.to_string(), "not found");
    assert_eq!(FsError::PermissionDenied.to_string(), "permission denied");
    assert_eq!(FsError::AlreadyExists.to_string(), "already exists");
    assert_eq!(FsError::NotSupported.to_string(), "not supported");
    assert_eq!(FsError::Other("custom".to_string()).to_string(), "custom");
  }

  #[test]
  fn test_file_attr_regular() {
    let attr = FileAttr::regular(1024, 0o644);
    assert_eq!(attr.size, 1024);
    assert_eq!(attr.blocks, 2); // 1024 / 512 = 2
    assert_eq!(attr.kind, FileType::RegularFile);
    assert_eq!(attr.perm, 0o644);
    assert_eq!(attr.nlink, 1);
  }

  #[test]
  fn test_file_attr_directory() {
    let attr = FileAttr::directory(0o755);
    assert_eq!(attr.size, 0);
    assert_eq!(attr.blocks, 0);
    assert_eq!(attr.kind, FileType::Directory);
    assert_eq!(attr.perm, 0o755);
    assert_eq!(attr.nlink, 2);
  }

  #[test]
  fn test_open_flags_constructors() {
    let ro = OpenFlags::read_only();
    assert!(ro.read);
    assert!(!ro.write);
    assert!(!ro.append);

    let wo = OpenFlags::write_only();
    assert!(!wo.read);
    assert!(wo.write);

    let rw = OpenFlags::read_write();
    assert!(rw.read);
    assert!(rw.write);
    assert!(!rw.truncate);
    assert!(!rw.create);
    assert!(!rw.exclusive);
  }

  #[test]
  fn test_file_type_equality() {
    assert_eq!(FileType::RegularFile, FileType::RegularFile);
    assert_eq!(FileType::Directory, FileType::Directory);
    assert_ne!(FileType::RegularFile, FileType::Directory);
    assert_ne!(FileType::Symlink, FileType::RegularFile);
  }

  #[test]
  fn test_fs_error_io_wraps_error() {
    // Verify that FsError::Io correctly wraps io::Error and returns errno
    let io_err = std::io::Error::from_raw_os_error(libc::ECONNREFUSED);
    let fs_err = FsError::Io(io_err);

    assert_eq!(
      fs_err.to_errno(),
      libc::ECONNREFUSED,
      "FsError::Io should return errno from the wrapped error"
    );

    // Verify that io::Error without os_error returns EIO
    let io_err_no_os = std::io::Error::new(std::io::ErrorKind::Other, "custom");
    let fs_err_no_os = FsError::Io(io_err_no_os);
    assert_eq!(
      fs_err_no_os.to_errno(),
      libc::EIO,
      "FsError::Io without raw_os_error should return EIO"
    );
  }

  #[test]
  fn test_file_attr_size_and_blocks() {
    // Verify block calculation: ceil(1000 / 512) = 2
    let attr = FileAttr::regular(1000, 0o644);
    assert_eq!(attr.size, 1000, "size should be 1000");
    assert_eq!(attr.blocks, 2, "ceil(1000 / 512) = 2 blocks");

    // Additional check: exact multiple of 512
    let attr_exact = FileAttr::regular(1024, 0o644);
    assert_eq!(attr_exact.blocks, 2, "1024 / 512 = exactly 2 blocks");

    // Check for zero size
    let attr_zero = FileAttr::regular(0, 0o644);
    assert_eq!(attr_zero.blocks, 0, "0 bytes = 0 blocks");

    // Check for 1 byte
    let attr_one = FileAttr::regular(1, 0o644);
    assert_eq!(attr_one.blocks, 1, "1 byte = 1 block (rounded up)");
  }

  #[test]
  fn test_statfs_fields() {
    // Create StatFs with known values and verify all fields
    let statfs = StatFs {
      blocks: 1_000_000,
      bfree: 500_000,
      bavail: 400_000,
      files: 100_000,
      ffree: 50_000,
      bsize: 4096,
      namelen: 255,
    };

    assert_eq!(statfs.blocks, 1_000_000, "total blocks");
    assert_eq!(statfs.bfree, 500_000, "free blocks");
    assert_eq!(statfs.bavail, 400_000, "available blocks (for unprivileged users)");
    assert_eq!(statfs.files, 100_000, "total inodes");
    assert_eq!(statfs.ffree, 50_000, "free inodes");
    assert_eq!(statfs.bsize, 4096, "block size");
    assert_eq!(statfs.namelen, 255, "max file name length");
  }

  #[test]
  fn test_fs_error_not_empty_display() {
    // cgofuse-to-unifuse-porting.md: NotEmpty → ENOTEMPTY
    let err = FsError::NotEmpty;
    assert_eq!(err.to_errno(), libc::ENOTEMPTY);
    assert_eq!(err.to_string(), "directory not empty");
  }

  #[test]
  fn test_fs_error_already_exists_display() {
    // cgofuse: AlreadyExists → EEXIST
    let err = FsError::AlreadyExists;
    assert_eq!(err.to_errno(), libc::EEXIST);
    assert_eq!(err.to_string(), "already exists");
  }

  #[test]
  fn test_file_attr_symlink() {
    // rclone: symlink attributes
    let now = std::time::SystemTime::now();
    let attr = FileAttr {
      size: 10,
      blocks: 1,
      atime: now,
      mtime: now,
      ctime: now,
      crtime: now,
      kind: FileType::Symlink,
      perm: 0o777,
      nlink: 1,
      uid: 0,
      gid: 0,
      rdev: 0,
      flags: 0,
    };
    assert_eq!(attr.kind, FileType::Symlink, "kind should be Symlink");
    assert_eq!(attr.perm, 0o777, "symlinks typically have 0o777");
    assert_eq!(attr.nlink, 1, "symlink has 1 link");
  }
}
