//! unifuse — cross-platform async FUSE abstraction.
//!
//! A thin adaptation layer on top of existing Rust crates:
//!
//! - Linux/macOS: **rfuse3** — async, tokio-native, works directly with `/dev/fuse`
//! - Windows: **winfsp-rs** — Native API (not a FUSE compatibility layer)
//!
//! # Key idea
//!
//! `UniFuseFilesystem` — an async path-based trait implemented by the business logic (omnifuse-core).
//! Platform adapters (`Rfuse3Adapter`, `WinfspAdapter`) — thin wrappers
//! that convert platform-specific calls into path-based async trait calls.
//!
//! # Example
//!
//! ```ignore
//! use unifuse::{UniFuseFilesystem, UniFuseHost, MountOptions};
//!
//! let fs = MyFilesystem::new();
//! let host = UniFuseHost::new(fs);
//! host.mount("/mnt/myfs", MountOptions::default()).await?;
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod inode;
#[cfg(unix)]
pub mod rfuse3_adapter;
pub mod types;
#[cfg(windows)]
pub mod winfsp_adapter;

use std::{
  ffi::OsStr,
  path::{Path, PathBuf},
  sync::Arc,
  time::SystemTime
};

pub use inode::{InodeMap, NodeKind, ROOT_INODE};
pub use types::*;

/// Cross-platform async path-based filesystem trait.
///
/// Analogous to cgofuse `FileSystemInterface` (42 methods),
/// but with a Rust-idiomatic async API: `Result<T>`, `&Path`, `Vec<u8>`.
///
/// Async-first: rfuse3 calls these methods from a tokio runtime.
/// The `WinFsp` adapter uses `tokio::runtime::Handle::block_on()`
/// to call async methods from a synchronous context.
///
/// The trait implementor is `omnifuse-core` (`OmniFuseVfs`).
pub trait UniFuseFilesystem: Send + Sync + 'static {
  // --- Lifecycle ---

  /// Initialize the filesystem. Called before the first operation.
  fn init(&mut self) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  /// Cleanup on unmount.
  fn destroy(&mut self) {}

  // --- Metadata ---

  /// Get file or directory attributes.
  fn getattr(&self, path: &Path) -> impl Future<Output = Result<FileAttr, FsError>> + Send;

  /// Set attributes (size, timestamps, permissions).
  fn setattr(
    &self,
    _path: &Path,
    _size: Option<u64>,
    _atime: Option<SystemTime>,
    _mtime: Option<SystemTime>,
    _mode: Option<u32>
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Look up a file in a directory by name.
  fn lookup(
    &self,
    parent: &Path,
    name: &OsStr
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send;

  // --- File operations ---

  /// Open a file.
  fn open(
    &self,
    path: &Path,
    flags: OpenFlags
  ) -> impl Future<Output = Result<FileHandle, FsError>> + Send;

  /// Create and open a file.
  fn create(
    &self,
    _path: &Path,
    _flags: OpenFlags,
    _mode: u32
  ) -> impl Future<Output = Result<(FileHandle, FileAttr), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Read data from a file.
  fn read(
    &self,
    path: &Path,
    fh: FileHandle,
    offset: u64,
    size: u32
  ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send;

  /// Write data to a file.
  fn write(
    &self,
    _path: &Path,
    _fh: FileHandle,
    _offset: u64,
    _data: &[u8]
  ) -> impl Future<Output = Result<u32, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Flush file buffers.
  fn flush(
    &self,
    _path: &Path,
    _fh: FileHandle
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  /// Close a file.
  fn release(
    &self,
    path: &Path,
    fh: FileHandle
  ) -> impl Future<Output = Result<(), FsError>> + Send;

  /// Sync a file to disk.
  fn fsync(
    &self,
    _path: &Path,
    _fh: FileHandle,
    _datasync: bool
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Ok(()) }
  }

  // --- Directories ---

  /// Read directory contents.
  fn readdir(&self, path: &Path) -> impl Future<Output = Result<Vec<DirEntry>, FsError>> + Send;

  /// Create a directory.
  fn mkdir(
    &self,
    _path: &Path,
    _mode: u32
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Remove a directory.
  fn rmdir(&self, _path: &Path) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Tree operations ---

  /// Remove a file.
  fn unlink(&self, _path: &Path) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Rename a file or directory.
  fn rename(
    &self,
    _from: &Path,
    _to: &Path,
    _flags: u32
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Symbolic links ---

  /// Create a symbolic link.
  fn symlink(
    &self,
    _target: &Path,
    _link: &Path
  ) -> impl Future<Output = Result<FileAttr, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Read the contents of a symbolic link.
  fn readlink(&self, _path: &Path) -> impl Future<Output = Result<PathBuf, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Extended attributes ---

  /// Set an extended attribute.
  fn setxattr(
    &self,
    _path: &Path,
    _name: &OsStr,
    _value: &[u8],
    _flags: i32
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Get an extended attribute.
  fn getxattr(
    &self,
    _path: &Path,
    _name: &OsStr
  ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// List extended attributes.
  fn listxattr(
    &self,
    _path: &Path
  ) -> impl Future<Output = Result<Vec<std::ffi::OsString>, FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  /// Remove an extended attribute.
  fn removexattr(
    &self,
    _path: &Path,
    _name: &OsStr
  ) -> impl Future<Output = Result<(), FsError>> + Send {
    async { Err(FsError::NotSupported) }
  }

  // --- Filesystem information ---

  /// Get filesystem statistics.
  fn statfs(&self, path: &Path) -> impl Future<Output = Result<StatFs, FsError>> + Send;
}

/// Mount options.
#[derive(Debug, Clone)]
pub struct MountOptions {
  /// Filesystem name (displayed in mount).
  pub fs_name: String,
  /// Allow access by other users.
  pub allow_other: bool,
  /// Mount as read-only.
  pub read_only: bool
}

impl Default for MountOptions {
  fn default() -> Self {
    Self {
      fs_name: "omnifuse".to_string(),
      allow_other: false,
      read_only: false
    }
  }
}

/// Host for mounting the filesystem.
///
/// A wrapper over the platform-specific mount API:
/// - Unix: rfuse3 `Session` + `MountHandle`
/// - Windows: winfsp `FileSystemHost`
pub struct UniFuseHost<F: UniFuseFilesystem> {
  fs: Arc<F>
}

impl<F: UniFuseFilesystem> UniFuseHost<F> {
  /// Create a new host for the given filesystem.
  pub fn new(fs: F) -> Self {
    Self { fs: Arc::new(fs) }
  }

  /// Mount the filesystem and block until unmount.
  ///
  /// # Errors
  ///
  /// Returns an error if mounting fails.
  #[cfg(unix)]
  pub async fn mount(&self, mountpoint: &Path, options: &MountOptions) -> Result<(), FsError> {
    use rfuse3_adapter::Rfuse3Adapter;

    let adapter = Rfuse3Adapter::new(Arc::clone(&self.fs), mountpoint.to_path_buf());

    let mut mount_options = rfuse3::MountOptions::default();
    mount_options
      .fs_name(&options.fs_name)
      .allow_other(options.allow_other)
      .read_only(options.read_only);

    let mount_handle = rfuse3::raw::Session::new(mount_options)
      .mount_with_unprivileged(adapter, mountpoint)
      .await
      .map_err(|e| FsError::Other(format!("mount error: {e}")))?;

    mount_handle
      .await
      .map_err(|e| FsError::Other(format!("FUSE session error: {e}")))?;

    Ok(())
  }

  /// Mount the filesystem on Windows via WinFsp and block until unmount.
  ///
  /// # Errors
  ///
  /// Returns an error if mounting fails.
  #[cfg(windows)]
  pub async fn mount(&self, mountpoint: &Path, options: &MountOptions) -> Result<(), FsError> {
    use winfsp_adapter::WinfspAdapter;
    use winfsp::host::{FileSystemHost, VolumeParams};

    let rt = tokio::runtime::Handle::current();
    let adapter = WinfspAdapter::new(Arc::clone(&self.fs), rt);

    let mut volume_params = VolumeParams::new();
    volume_params
      .filesystem_name(&options.fs_name)
      .sector_size(512)
      .sectors_per_allocation_unit(8)
      .max_component_length(255)
      .case_sensitive_search(false)
      .case_preserved_names(true)
      .unicode_on_disk(true)
      .read_only_volume(options.read_only);

    let mut host = FileSystemHost::new(volume_params, adapter)
      .map_err(|e| FsError::Other(format!("WinFsp host creation error: {e}")))?;

    let mount_str = mountpoint.to_string_lossy();
    host
      .mount(&mount_str)
      .map_err(|e| FsError::Other(format!("WinFsp mount error: {e}")))?;

    host
      .start()
      .map_err(|e| FsError::Other(format!("WinFsp start error: {e}")))?;

    // Block until Ctrl+C (same pattern as rfuse3 mount_handle.await on Unix).
    tokio::signal::ctrl_c()
      .await
      .map_err(|e| FsError::Other(format!("signal error: {e}")))?;

    host.stop();
    host.unmount();
    Ok(())
  }

  /// Check whether the FUSE/WinFsp platform is available.
  #[must_use]
  pub fn is_available() -> bool {
    #[cfg(target_os = "linux")]
    {
      Path::new("/dev/fuse").exists()
    }
    #[cfg(target_os = "macos")]
    {
      Path::new("/Library/Filesystems/macfuse.fs").exists()
        || Path::new("/usr/local/lib/libfuse.dylib").exists()
    }
    #[cfg(windows)]
    {
      // Check if WinFsp is installed by looking for the DLL.
      Path::new(r"C:\Program Files (x86)\WinFsp\bin\winfsp-x64.dll").exists()
        || Path::new(r"C:\Program Files\WinFsp\bin\winfsp-x64.dll").exists()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
      false
    }
  }
}
