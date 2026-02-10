//! Adapter from WinFsp to async `UniFuseFilesystem`.
//!
//! Converts synchronous WinFsp `FileSystemContext` callbacks
//! into path-based async calls to the `UniFuseFilesystem` trait.
//!
//! Type mapping:
//! - `U16CStr` (NT path) -> `Path` (via `nt_path_to_pathbuf`)
//! - `unifuse::FileAttr` -> `winfsp::FileInfo` (via `fill_file_info`)
//! - `unifuse::FsError` -> `NTSTATUS` (via `FsError::to_ntstatus()`)
//!
//! Key design decision: WinFsp is synchronous, but `UniFuseFilesystem` is async.
//! The bridge uses `tokio::runtime::Handle::block_on()` to call async methods
//! from a synchronous context.

use std::{
  ffi::c_void,
  path::{Path, PathBuf},
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH}
};

use tracing::debug;
use winfsp::{
  U16CStr,
  filesystem::{
    DirInfo, DirMarker, FileSecurity, FileSystemContext, OpenFileInfo,
    WideNameInfo
  }
};
use windows::Win32::{
  Foundation::NTSTATUS,
  Storage::FileSystem::{
    FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_READONLY, FILE_DIRECTORY_FILE
  }
};

use crate::{
  FileHandle, FileType, FsError, OpenFlags, UniFuseFilesystem,
  types::FileAttr
};

// --- Constants ---

/// Offset between Windows FILETIME epoch (1601-01-01) and Unix epoch (1970-01-01)
/// in 100-nanosecond intervals.
const FILETIME_UNIX_DIFF: u64 = 116_444_736_000_000_000;

/// 100-nanosecond intervals per second.
const INTERVALS_PER_SEC: u64 = 10_000_000;

// --- WinFsp file context ---

/// File context stored by WinFsp for each open file/directory.
///
/// WinFsp does not pass the file path to `read()`/`write()`/`get_file_info()`,
/// so we store it at `open()`/`create()` time and retrieve it in subsequent calls.
pub struct WinfspFileContext {
  /// Path to the file (relative to the filesystem root).
  path: PathBuf,
  /// File handle returned by `UniFuseFilesystem::open()`.
  handle: FileHandle,
  /// Whether this context represents a directory.
  is_directory: bool,
  /// Marked for deletion (set by `set_delete`, executed in `cleanup`).
  delete_on_close: bool
}

// --- Adapter ---

/// Adapter: converts synchronous WinFsp `FileSystemContext` calls
/// into async `UniFuseFilesystem` calls via `block_on()`.
pub struct WinfspAdapter<F: UniFuseFilesystem> {
  inner: Arc<F>,
  rt: tokio::runtime::Handle
}

impl<F: UniFuseFilesystem> WinfspAdapter<F> {
  /// Create a new WinFsp adapter.
  pub fn new(fs: Arc<F>, rt: tokio::runtime::Handle) -> Self {
    Self { inner: fs, rt }
  }

  /// Run an async future on the tokio runtime.
  fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
    self.rt.block_on(future)
  }
}

// --- Helper functions ---

/// Convert a WinFsp NT path (`\dir\file`) to a `PathBuf` (`/dir/file`).
///
/// WinFsp uses backslash-separated paths starting with `\`.
/// The root path `\` maps to `/`.
fn nt_path_to_pathbuf(nt_path: &U16CStr) -> PathBuf {
  let os_string = nt_path.to_os_string();
  let s = os_string.to_string_lossy();
  let unix_path = s.replace('\\', "/");
  PathBuf::from(unix_path)
}

/// Convert `SystemTime` to Windows FILETIME (u64, 100ns intervals since 1601-01-01).
fn system_time_to_filetime(time: SystemTime) -> u64 {
  let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
  let intervals = duration.as_secs() * INTERVALS_PER_SEC
    + u64::from(duration.subsec_nanos()) / 100;
  intervals + FILETIME_UNIX_DIFF
}

/// Convert Windows FILETIME (u64) to `SystemTime`.
fn filetime_to_system_time(filetime: u64) -> SystemTime {
  if filetime < FILETIME_UNIX_DIFF {
    return UNIX_EPOCH;
  }
  let intervals = filetime - FILETIME_UNIX_DIFF;
  let secs = intervals / INTERVALS_PER_SEC;
  let nanos = ((intervals % INTERVALS_PER_SEC) * 100) as u32;
  UNIX_EPOCH + Duration::new(secs, nanos)
}

/// Convert `FileType` + Unix permission to Windows `FILE_ATTRIBUTE_*` flags.
fn file_type_to_win_attrs(kind: FileType, perm: u16) -> u32 {
  let mut attrs = match kind {
    FileType::Directory => FILE_ATTRIBUTE_DIRECTORY.0,
    _ => FILE_ATTRIBUTE_NORMAL.0
  };
  // Read-only if no write permission for owner.
  if perm & 0o200 == 0 {
    attrs |= FILE_ATTRIBUTE_READONLY.0;
  }
  attrs
}

/// Fill a WinFsp `FileInfo` from a unifuse `FileAttr`.
fn fill_file_info(attr: &FileAttr, info: &mut winfsp::filesystem::FileInfo) {
  info.file_attributes = file_type_to_win_attrs(attr.kind, attr.perm);
  info.file_size = attr.size;
  info.allocation_size = attr.size.next_multiple_of(4096);
  info.creation_time = system_time_to_filetime(attr.crtime);
  info.last_access_time = system_time_to_filetime(attr.atime);
  info.last_write_time = system_time_to_filetime(attr.mtime);
  info.change_time = system_time_to_filetime(attr.ctime);
  info.hard_links = 0; // Not tracked.
  info.reparse_tag = 0;
  info.ea_size = 0;
  info.index_number = 0;
}

/// Convert `FsError` to a WinFsp `NTSTATUS` error.
fn fs_error_to_ntstatus(err: &FsError) -> NTSTATUS {
  err.to_ntstatus()
}

// --- FileSystemContext implementation ---

impl<F: UniFuseFilesystem> FileSystemContext for WinfspAdapter<F> {
  type FileContext = WinfspFileContext;

  fn get_security_by_name(
    &self,
    file_name: &U16CStr,
    _security_descriptor: Option<&mut [c_void]>,
    resolve_reparse_points: impl FnOnce(&U16CStr) -> Option<FileSecurity>
  ) -> winfsp::Result<FileSecurity> {
    // Check for reparse points first.
    if let Some(security) = resolve_reparse_points(file_name) {
      return Ok(security);
    }

    let path = nt_path_to_pathbuf(file_name);
    debug!(?path, "get_security_by_name");

    let attr = self
      .block_on(self.inner.getattr(&path))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    let attributes = file_type_to_win_attrs(attr.kind, attr.perm);

    Ok(FileSecurity {
      reparse: false,
      sz_security_descriptor: 0,
      attributes
    })
  }

  fn open(
    &self,
    file_name: &U16CStr,
    _create_options: u32,
    _granted_access: FILE_ACCESS_RIGHTS,
    file_info: &mut OpenFileInfo
  ) -> winfsp::Result<Self::FileContext> {
    let path = nt_path_to_pathbuf(file_name);
    debug!(?path, "open");

    let attr = self
      .block_on(self.inner.getattr(&path))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    let is_directory = attr.kind == FileType::Directory;

    let handle = if is_directory {
      FileHandle(0)
    } else {
      self
        .block_on(self.inner.open(&path, OpenFlags::read_write()))
        .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?
    };

    fill_file_info(&attr, file_info.as_mut());

    Ok(WinfspFileContext {
      path,
      handle,
      is_directory,
      delete_on_close: false
    })
  }

  fn close(&self, context: Self::FileContext) {
    debug!(path = ?context.path, "close");
    if !context.is_directory {
      let _ = self.block_on(self.inner.release(&context.path, context.handle));
    }
  }

  fn create(
    &self,
    file_name: &U16CStr,
    create_options: u32,
    _granted_access: FILE_ACCESS_RIGHTS,
    _file_attributes: u32,
    _security_descriptor: Option<&[c_void]>,
    _allocation_size: u64,
    file_info: &mut OpenFileInfo
  ) -> winfsp::Result<Self::FileContext> {
    let path = nt_path_to_pathbuf(file_name);
    let is_directory = create_options & FILE_DIRECTORY_FILE.0 != 0;

    debug!(?path, is_directory, "create");

    if is_directory {
      let attr = self
        .block_on(self.inner.mkdir(&path, 0o755))
        .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

      fill_file_info(&attr, file_info.as_mut());

      Ok(WinfspFileContext {
        path,
        handle: FileHandle(0),
        is_directory: true,
        delete_on_close: false
      })
    } else {
      let (handle, attr) = self
        .block_on(self.inner.create(&path, OpenFlags::read_write(), 0o644))
        .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

      fill_file_info(&attr, file_info.as_mut());

      Ok(WinfspFileContext {
        path,
        handle,
        is_directory: false,
        delete_on_close: false
      })
    }
  }

  fn read(
    &self,
    context: &Self::FileContext,
    buffer: &mut [u8],
    offset: u64
  ) -> winfsp::Result<u32> {
    debug!(path = ?context.path, offset, len = buffer.len(), "read");

    #[allow(clippy::cast_possible_truncation)]
    let size = buffer.len() as u32;

    let data = self
      .block_on(self.inner.read(&context.path, context.handle, offset, size))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    let bytes_read = data.len().min(buffer.len());
    buffer[..bytes_read].copy_from_slice(&data[..bytes_read]);

    #[allow(clippy::cast_possible_truncation)]
    Ok(bytes_read as u32)
  }

  fn write(
    &self,
    context: &Self::FileContext,
    buffer: &[u8],
    offset: u64,
    _write_to_eof: bool,
    _constrained_io: bool,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<u32> {
    debug!(path = ?context.path, offset, len = buffer.len(), "write");

    let written = self
      .block_on(
        self
          .inner
          .write(&context.path, context.handle, offset, buffer)
      )
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    // Update file info after write.
    if let Ok(attr) = self.block_on(self.inner.getattr(&context.path)) {
      fill_file_info(&attr, file_info);
    }

    Ok(written)
  }

  fn get_file_info(
    &self,
    context: &Self::FileContext,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, "get_file_info");

    let attr = self
      .block_on(self.inner.getattr(&context.path))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    fill_file_info(&attr, file_info);
    Ok(())
  }

  fn set_basic_info(
    &self,
    context: &Self::FileContext,
    file_attributes: u32,
    creation_time: u64,
    last_access_time: u64,
    last_write_time: u64,
    _change_time: u64,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, "set_basic_info");

    let atime = if last_access_time != 0 {
      Some(filetime_to_system_time(last_access_time))
    } else {
      None
    };

    let mtime = if last_write_time != 0 {
      Some(filetime_to_system_time(last_write_time))
    } else {
      None
    };

    // Map readonly attribute to permissions.
    let mode = if file_attributes != 0 {
      let readonly = file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0;
      Some(if readonly { 0o444_u32 } else { 0o644 })
    } else {
      None
    };

    let _ = self
      .block_on(self.inner.setattr(&context.path, None, atime, mtime, mode));

    // Re-read attributes.
    if let Ok(attr) = self.block_on(self.inner.getattr(&context.path)) {
      fill_file_info(&attr, file_info);
    }

    Ok(())
  }

  fn set_file_size(
    &self,
    context: &Self::FileContext,
    new_size: u64,
    _set_allocation_size: bool,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, new_size, "set_file_size");

    self
      .block_on(
        self
          .inner
          .setattr(&context.path, Some(new_size), None, None, None)
      )
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    if let Ok(attr) = self.block_on(self.inner.getattr(&context.path)) {
      fill_file_info(&attr, file_info);
    }

    Ok(())
  }

  fn flush(
    &self,
    context: &Self::FileContext,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, "flush");

    if !context.is_directory {
      let _ = self.block_on(self.inner.flush(&context.path, context.handle));
    }

    if let Ok(attr) = self.block_on(self.inner.getattr(&context.path)) {
      fill_file_info(&attr, file_info);
    }

    Ok(())
  }

  fn cleanup(
    &self,
    context: &mut Self::FileContext,
    _flags: Option<winfsp::filesystem::CleanupFlags>,
    _delete: bool
  ) {
    debug!(path = ?context.path, delete = context.delete_on_close, "cleanup");

    if context.delete_on_close {
      if context.is_directory {
        let _ = self.block_on(self.inner.rmdir(&context.path));
      } else {
        let _ = self.block_on(self.inner.unlink(&context.path));
      }
    }
  }

  fn set_delete(
    &self,
    context: &mut Self::FileContext,
    _file_name: &U16CStr,
    delete_file: bool
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, delete_file, "set_delete");
    context.delete_on_close = delete_file;
    Ok(())
  }

  fn rename(
    &self,
    context: &mut Self::FileContext,
    _file_name: &U16CStr,
    new_file_name: &U16CStr,
    _replace_if_exists: bool
  ) -> winfsp::Result<()> {
    let new_path = nt_path_to_pathbuf(new_file_name);
    debug!(from = ?context.path, to = ?new_path, "rename");

    self
      .block_on(self.inner.rename(&context.path, &new_path, 0))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    context.path = new_path;
    Ok(())
  }

  fn read_directory(
    &self,
    context: &Self::FileContext,
    _pattern: Option<&U16CStr>,
    marker: DirMarker<'_>,
    buffer: &mut [u8]
  ) -> winfsp::Result<u32> {
    debug!(path = ?context.path, "read_directory");

    let entries = self
      .block_on(self.inner.readdir(&context.path))
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    let mut cursor = 0_u32;
    let past_marker = marker.is_none();
    let marker_name = marker.map(|m| m.to_os_string());

    // Add "." and ".." entries.
    if past_marker || marker_name.is_none() {
      let mut dot = DirInfo::<{ 255 * 2 + 2 }>::new();
      if dot.set_name(std::ffi::OsStr::new(".")).is_ok() {
        dot.file_info_mut().file_attributes = FILE_ATTRIBUTE_DIRECTORY.0;
        let _ = dot.write_to_buffer(buffer, &mut cursor);
      }

      let mut dotdot = DirInfo::<{ 255 * 2 + 2 }>::new();
      if dotdot.set_name(std::ffi::OsStr::new("..")).is_ok() {
        dotdot.file_info_mut().file_attributes = FILE_ATTRIBUTE_DIRECTORY.0;
        let _ = dotdot.write_to_buffer(buffer, &mut cursor);
      }
    }

    // Add real entries.
    let mut past = past_marker;
    for entry in &entries {
      if !past {
        if let Some(ref mname) = marker_name {
          if entry.name == mname.to_string_lossy().as_ref() {
            past = true;
          }
        }
        continue;
      }

      let mut dir_info = DirInfo::<{ 255 * 2 + 2 }>::new();
      if dir_info
        .set_name(std::ffi::OsStr::new(&entry.name))
        .is_err()
      {
        continue;
      }

      let child_path = context.path.join(&entry.name);
      if let Ok(attr) = self.block_on(self.inner.getattr(&child_path)) {
        fill_file_info(&attr, dir_info.file_info_mut());
      } else {
        dir_info.file_info_mut().file_attributes = match entry.kind {
          FileType::Directory => FILE_ATTRIBUTE_DIRECTORY.0,
          _ => FILE_ATTRIBUTE_NORMAL.0
        };
      }

      if dir_info.write_to_buffer(buffer, &mut cursor).is_err() {
        break; // Buffer full.
      }
    }

    Ok(cursor)
  }

  fn get_volume_info(
    &self,
    out_volume_info: &mut winfsp::filesystem::VolumeInfo
  ) -> winfsp::Result<()> {
    debug!("get_volume_info");

    let root = PathBuf::from("/");
    if let Ok(stat) = self.block_on(self.inner.statfs(&root)) {
      let block_size = u64::from(stat.bsize);
      out_volume_info.total_size = stat.blocks * block_size;
      out_volume_info.free_size = stat.bfree * block_size;
    } else {
      // Fallback: report 1 GB total, 512 MB free.
      out_volume_info.total_size = 1024 * 1024 * 1024;
      out_volume_info.free_size = 512 * 1024 * 1024;
    }

    Ok(())
  }

  fn overwrite(
    &self,
    context: &Self::FileContext,
    _file_attributes: u32,
    _replace_file_attributes: bool,
    _allocation_size: u64,
    file_info: &mut winfsp::filesystem::FileInfo
  ) -> winfsp::Result<()> {
    debug!(path = ?context.path, "overwrite");

    // Truncate the file to zero.
    self
      .block_on(
        self
          .inner
          .setattr(&context.path, Some(0), None, None, None)
      )
      .map_err(|e| winfsp::FspError::from(fs_error_to_ntstatus(&e)))?;

    if let Ok(attr) = self.block_on(self.inner.getattr(&context.path)) {
      fill_file_info(&attr, file_info);
    }

    Ok(())
  }
}
