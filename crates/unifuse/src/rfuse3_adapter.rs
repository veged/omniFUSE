//! Adapter from rfuse3 to async `SessionPathFs`.
//!
//! Converts inode-based rfuse3 calls into path-based async calls
//! to the `SessionPathFs` trait.
//!
//! Type mapping:
//! - `rfuse3::Inode` (u64) -> `Path` (via `InodeMap`)
//! - `rfuse3::raw::reply::FileAttr` -> `unifuse::FileAttr`
//! - `unifuse::FsError` -> `rfuse3::Errno`

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::{
  ffi::{OsStr, OsString},
  path::PathBuf,
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH}
};

use dashmap::DashMap;
use futures_util::stream;
use rfuse3::{Errno, Timestamp, raw::prelude::*};
use tokio::sync::RwLock;
use tracing::debug;

use crate::{
  CloseReason, DirPageRequest, FileType, FlushMode, FsError, FsMutation, InodeMap, NodeKind, OpenFlags, OpenIntent,
  OpenedNode, ROOT_INODE, SessionPathFs,
  types::{self}
};

/// Alias for rfuse3 `Result`.
type Result<T> = std::result::Result<T, Errno>;

/// TTL for attribute caching (1 minute).
const TTL: Duration = Duration::from_secs(60);

/// Maximum write size (1 MB).
const MAX_WRITE: std::num::NonZeroU32 = match std::num::NonZeroU32::new(1024 * 1024) {
  Some(v) => v,
  None => panic!("MAX_WRITE cannot be zero")
};

/// Adapter: converts inode-based rfuse3 calls into path-based async `SessionPathFs` calls.
///
/// Contains an `InodeMap` for inode-to-path mapping.
pub struct Rfuse3Adapter<F: SessionPathFs> {
  inner: Arc<F>,
  state: Arc<F::MountState>,
  inode_map: Arc<RwLock<InodeMap>>,
  open_nodes: DashMap<u64, OpenedNode>
}

impl<F: SessionPathFs> Rfuse3Adapter<F> {
  /// Create a new adapter for the filesystem.
  pub fn new(fs: Arc<F>, state: Arc<F::MountState>) -> Self {
    Self {
      inner: fs,
      state,
      inode_map: Arc::new(RwLock::new(InodeMap::new(PathBuf::new()))),
      open_nodes: DashMap::new()
    }
  }

  /// Get the path for an inode, or return ENOENT.
  async fn resolve_path(&self, inode: u64) -> Result<PathBuf> {
    self
      .inode_map
      .read()
      .await
      .get_path(inode)
      .ok_or_else(|| Errno::from(libc::ENOENT))
  }

  fn opened_node(&self, fh: u64) -> Result<OpenedNode> {
    self
      .open_nodes
      .get(&fh)
      .map(|entry| entry.value().clone())
      .ok_or_else(|| Errno::from(libc::EBADF))
  }
}

// --- Type conversions ---

/// Convert `unifuse::FileAttr` to `rfuse3::raw::reply::FileAttr`.
fn to_rfuse3_attr(attr: &types::FileAttr, ino: u64) -> FileAttr {
  FileAttr {
    ino,
    size: attr.size,
    blocks: attr.blocks,
    atime: system_time_to_timestamp(attr.atime),
    mtime: system_time_to_timestamp(attr.mtime),
    ctime: system_time_to_timestamp(attr.ctime),
    #[cfg(target_os = "macos")]
    crtime: system_time_to_timestamp(attr.crtime),
    kind: to_rfuse3_filetype(attr.kind),
    perm: attr.perm,
    nlink: attr.nlink,
    uid: attr.uid,
    gid: attr.gid,
    rdev: attr.rdev,
    #[cfg(target_os = "macos")]
    flags: attr.flags,
    blksize: 512
  }
}

/// Convert `unifuse::FileType` to `rfuse3::FileType`.
const fn to_rfuse3_filetype(kind: FileType) -> rfuse3::FileType {
  match kind {
    FileType::RegularFile => rfuse3::FileType::RegularFile,
    FileType::Directory => rfuse3::FileType::Directory,
    FileType::Symlink => rfuse3::FileType::Symlink,
    FileType::BlockDevice => rfuse3::FileType::BlockDevice,
    FileType::CharDevice => rfuse3::FileType::CharDevice,
    FileType::NamedPipe => rfuse3::FileType::NamedPipe,
    FileType::Socket => rfuse3::FileType::Socket
  }
}

/// Convert `unifuse::FileType` to `inode::NodeKind`.
const fn to_node_kind(kind: FileType) -> NodeKind {
  match kind {
    FileType::Directory => NodeKind::Dir,
    _ => NodeKind::File
  }
}

/// Convert `SystemTime` to rfuse3 `Timestamp`.
fn system_time_to_timestamp(time: SystemTime) -> Timestamp {
  let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
  Timestamp::new(i64::try_from(duration.as_secs()).unwrap_or(0), duration.subsec_nanos())
}

/// Convert rfuse3 `Timestamp` to `SystemTime`.
fn timestamp_to_system_time(ts: Timestamp) -> SystemTime {
  let secs = u64::try_from(ts.sec).unwrap_or(0);
  UNIX_EPOCH + Duration::new(secs, ts.nsec)
}

/// Convert `FsError` to rfuse3 `Errno`.
fn fs_error_to_errno(err: &FsError) -> Errno {
  Errno::from(err.to_errno())
}

fn xattr_reply(data: Vec<u8>, size: u32) -> Result<ReplyXAttr> {
  if size == 0 {
    #[allow(clippy::cast_possible_truncation)]
    return Ok(ReplyXAttr::Size(data.len() as u32));
  }

  if data.len() > size as usize {
    return Err(Errno::from(libc::ERANGE));
  }

  Ok(ReplyXAttr::Data(data.into()))
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
  value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
  value.to_string_lossy().as_bytes().to_vec()
}

/// Convert rfuse3 open flags to `unifuse::OpenFlags`.
const fn decode_open_flags(flags: u32) -> OpenFlags {
  let access_mode = flags & libc::O_ACCMODE as u32;
  OpenFlags {
    read: access_mode == libc::O_RDONLY as u32 || access_mode == libc::O_RDWR as u32,
    write: access_mode == libc::O_WRONLY as u32 || access_mode == libc::O_RDWR as u32,
    append: flags & libc::O_APPEND as u32 != 0,
    truncate: flags & libc::O_TRUNC as u32 != 0,
    create: flags & libc::O_CREAT as u32 != 0,
    exclusive: flags & libc::O_EXCL as u32 != 0
  }
}

#[allow(clippy::too_many_arguments)]
impl<F: SessionPathFs> rfuse3::raw::Filesystem for Rfuse3Adapter<F> {
  async fn init(&self, _req: Request) -> Result<ReplyInit> {
    debug!("FUSE init");
    Ok(ReplyInit { max_write: MAX_WRITE })
  }

  async fn destroy(&self, _req: Request) {
    debug!("FUSE destroy");
  }

  async fn lookup(&self, _req: Request, parent: u64, name: &OsStr) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let child_path = parent_path.join(name);

    match self.inner.lookup(&self.state, &child_path).await {
      Ok(meta) => {
        let kind = to_node_kind(meta.attr.kind);
        let ino = self.inode_map.write().await.get_or_insert(&child_path, kind);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&meta.attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn forget(&self, _req: Request, inode: u64, _nlookup: u64) {
    debug!(inode, "forget (noop)");
  }

  async fn getattr(&self, _req: Request, inode: u64, _fh: Option<u64>, _flags: u32) -> Result<ReplyAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.lookup(&self.state, &path).await {
      Ok(meta) => Ok(ReplyAttr {
        ttl: TTL,
        attr: to_rfuse3_attr(&meta.attr, inode)
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn setattr(&self, _req: Request, inode: u64, _fh: Option<u64>, set_attr: SetAttr) -> Result<ReplyAttr> {
    let path = self.resolve_path(inode).await?;

    let size = set_attr.size;
    let atime = set_attr.atime.map(timestamp_to_system_time);
    let mtime = set_attr.mtime.map(timestamp_to_system_time);
    #[allow(clippy::useless_conversion)] // mode_t is u16 on macOS, u32 on Linux
    let mode = set_attr.mode.map(u32::from);

    match self
      .inner
      .mutate(
        &self.state,
        FsMutation::SetAttr {
          path: &path,
          size,
          atime,
          mtime,
          mode
        }
      )
      .await
    {
      Ok(meta) => Ok(ReplyAttr {
        ttl: TTL,
        attr: to_rfuse3_attr(&meta.attr, inode)
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn open(&self, _req: Request, inode: u64, flags: u32) -> Result<ReplyOpen> {
    let path = self.resolve_path(inode).await?;
    let intent = OpenIntent {
      flags: decode_open_flags(flags)
    };

    match self.inner.open(&self.state, &path, intent).await {
      Ok(opened) => {
        let fh = opened.handle.0;
        self.open_nodes.insert(fh, opened);
        Ok(ReplyOpen { fh, flags: 0 })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn read(&self, _req: Request, _inode: u64, fh: u64, offset: u64, size: u32) -> Result<ReplyData> {
    let opened = self.opened_node(fh)?;
    let mut out = vec![0; size as usize];

    match self.inner.read_into(&opened, offset, &mut out).await {
      Ok(read) => {
        out.truncate(read);
        Ok(ReplyData { data: out.into() })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn write(
    &self,
    _req: Request,
    _inode: u64,
    fh: u64,
    offset: u64,
    data: &[u8],
    _write_flags: u32,
    _flags: u32
  ) -> Result<ReplyWrite> {
    let opened = self.opened_node(fh)?;

    match self.inner.write_from(&opened, offset, data).await {
      Ok(written) => {
        #[allow(clippy::cast_possible_truncation)]
        let written = written as u32;
        Ok(ReplyWrite { written })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn release(
    &self,
    _req: Request,
    _inode: u64,
    fh: u64,
    _flags: u32,
    _lock_owner: u64,
    _flush: bool
  ) -> Result<()> {
    let Some((_, opened)) = self.open_nodes.remove(&fh) else {
      return Err(Errno::from(libc::EBADF));
    };
    self
      .inner
      .close(opened, CloseReason::Released)
      .await
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn flush(&self, _req: Request, _inode: u64, fh: u64, _lock_owner: u64) -> Result<()> {
    let opened = self.opened_node(fh)?;
    self
      .inner
      .flush(&opened, FlushMode::Flush)
      .await
      .map(|_| ())
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn fsync(&self, _req: Request, _inode: u64, fh: u64, datasync: bool) -> Result<()> {
    let opened = self.opened_node(fh)?;
    self
      .inner
      .flush(&opened, FlushMode::Fsync { datasync })
      .await
      .map(|_| ())
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn readdir(
    &self,
    _req: Request,
    inode: u64,
    _fh: u64,
    offset: i64
  ) -> Result<ReplyDirectory<impl futures_util::Stream<Item = Result<DirectoryEntry>> + Send>> {
    let path = self.resolve_path(inode).await?;

    match self.inner.read_dir(&self.state, &path, DirPageRequest::all()).await {
      Ok(page) => {
        let mut dir_entries: Vec<DirectoryEntry> = Vec::new();
        let mut index: i64 = 1;

        // Add . and ..
        if offset < index {
          dir_entries.push(DirectoryEntry {
            inode,
            kind: rfuse3::FileType::Directory,
            name: OsString::from("."),
            offset: index
          });
        }
        index += 1;

        if offset < index {
          dir_entries.push(DirectoryEntry {
            inode: ROOT_INODE,
            kind: rfuse3::FileType::Directory,
            name: OsString::from(".."),
            offset: index
          });
        }
        index += 1;

        // Collect entries from UniFuseFilesystem
        for entry in page.entries {
          if offset < index {
            let child_path = path.join(&entry.name);
            let kind = to_node_kind(entry.kind);
            let child_ino = self.inode_map.write().await.get_or_insert(&child_path, kind);

            dir_entries.push(DirectoryEntry {
              inode: child_ino,
              kind: to_rfuse3_filetype(entry.kind),
              name: OsString::from(&entry.name),
              offset: index
            });
          }
          index += 1;
        }

        Ok(ReplyDirectory {
          entries: stream::iter(dir_entries.into_iter().map(Ok))
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn mkdir(&self, _req: Request, parent: u64, name: &OsStr, mode: u32, _umask: u32) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let dir_path = parent_path.join(name);

    match self
      .inner
      .mutate(&self.state, FsMutation::Mkdir { path: &dir_path, mode })
      .await
    {
      Ok(meta) => {
        let ino = self.inode_map.write().await.get_or_insert(&dir_path, NodeKind::Dir);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&meta.attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn mknod(&self, _req: Request, parent: u64, name: &OsStr, mode: u32, _rdev: u32) -> Result<ReplyEntry> {
    let file_type = mode & u32::from(libc::S_IFMT);
    if file_type != u32::from(libc::S_IFREG) {
      return Err(Errno::from(libc::ENOTSUP));
    }

    let parent_path = self.resolve_path(parent).await?;
    let file_path = parent_path.join(name);
    let perm = mode & 0o777;

    match self
      .inner
      .mutate(
        &self.state,
        FsMutation::Create {
          path: &file_path,
          flags: OpenFlags::write_only(),
          mode: perm
        }
      )
      .await
    {
      Ok(meta) => {
        let ino = self.inode_map.write().await.get_or_insert(&file_path, NodeKind::File);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&meta.attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn rmdir(&self, _req: Request, parent: u64, name: &OsStr) -> Result<()> {
    let parent_path = self.resolve_path(parent).await?;
    let dir_path = parent_path.join(name);

    self
      .inner
      .mutate(&self.state, FsMutation::Rmdir { path: &dir_path })
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    self.inode_map.write().await.remove(&dir_path);
    Ok(())
  }

  async fn unlink(&self, _req: Request, parent: u64, name: &OsStr) -> Result<()> {
    let parent_path = self.resolve_path(parent).await?;
    let file_path = parent_path.join(name);

    self
      .inner
      .mutate(&self.state, FsMutation::Unlink { path: &file_path })
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    self.inode_map.write().await.remove(&file_path);
    Ok(())
  }

  async fn rename(
    &self,
    _req: Request,
    origin_parent: u64,
    origin_name: &OsStr,
    parent: u64,
    name: &OsStr
  ) -> Result<()> {
    let origin_parent_path = self.resolve_path(origin_parent).await?;
    let new_parent_path = self.resolve_path(parent).await?;

    let old_path = origin_parent_path.join(origin_name);
    let new_path = new_parent_path.join(name);

    self
      .inner
      .mutate(
        &self.state,
        FsMutation::Rename {
          from: &old_path,
          to: &new_path,
          flags: 0
        }
      )
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    self.inode_map.write().await.rename_subtree(&old_path, &new_path);
    Ok(())
  }

  async fn rename2(
    &self,
    _req: Request,
    origin_parent: u64,
    origin_name: &OsStr,
    parent: u64,
    name: &OsStr,
    flags: u32
  ) -> Result<()> {
    if flags != 0 {
      return Err(Errno::from(libc::ENOTSUP));
    }

    let origin_parent_path = self.resolve_path(origin_parent).await?;
    let new_parent_path = self.resolve_path(parent).await?;

    let old_path = origin_parent_path.join(origin_name);
    let new_path = new_parent_path.join(name);

    self
      .inner
      .mutate(
        &self.state,
        FsMutation::Rename {
          from: &old_path,
          to: &new_path,
          flags
        }
      )
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    self.inode_map.write().await.rename_subtree(&old_path, &new_path);
    Ok(())
  }

  async fn link(&self, _req: Request, _inode: u64, _new_parent: u64, _new_name: &OsStr) -> Result<ReplyEntry> {
    Err(Errno::from(libc::ENOTSUP))
  }

  async fn create(&self, _req: Request, parent: u64, name: &OsStr, mode: u32, flags: u32) -> Result<ReplyCreated> {
    let parent_path = self.resolve_path(parent).await?;
    let file_path = parent_path.join(name);
    let open_flags = decode_open_flags(flags);

    match self
      .inner
      .mutate(
        &self.state,
        FsMutation::Create {
          path: &file_path,
          flags: open_flags,
          mode
        }
      )
      .await
    {
      Ok(meta) => {
        let opened = self
          .inner
          .open(&self.state, &file_path, OpenIntent { flags: open_flags })
          .await
          .map_err(|e| fs_error_to_errno(&e))?;
        let fh = opened.handle.0;
        let ino = self.inode_map.write().await.get_or_insert(&file_path, NodeKind::File);
        self.open_nodes.insert(fh, opened);
        Ok(ReplyCreated {
          ttl: TTL,
          attr: to_rfuse3_attr(&meta.attr, ino),
          generation: 0,
          fh,
          flags: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn symlink(&self, _req: Request, parent: u64, name: &OsStr, link_path: &OsStr) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let symlink_path = parent_path.join(name);
    let target = PathBuf::from(link_path);

    match self
      .inner
      .mutate(
        &self.state,
        FsMutation::Symlink {
          target: &target,
          link: &symlink_path
        }
      )
      .await
    {
      Ok(meta) => {
        let ino = self
          .inode_map
          .write()
          .await
          .get_or_insert(&symlink_path, NodeKind::File);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&meta.attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn readlink(&self, _req: Request, inode: u64) -> Result<ReplyData> {
    let path = self.resolve_path(inode).await?;

    match self.inner.read_link(&self.state, &path).await {
      Ok(target) => Ok(ReplyData {
        data: os_str_bytes(target.as_os_str()).into()
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn statfs(&self, _req: Request, inode: u64) -> Result<ReplyStatFs> {
    let _path = self.resolve_path(inode).await?;

    match self.inner.statfs(&self.state).await {
      Ok(stat) => Ok(ReplyStatFs {
        blocks: stat.blocks,
        bfree: stat.bfree,
        bavail: stat.bavail,
        files: stat.files,
        ffree: stat.ffree,
        bsize: stat.bsize,
        namelen: stat.namelen,
        frsize: stat.bsize
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn access(&self, _req: Request, inode: u64, _mask: u32) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .lookup(&self.state, &path)
      .await
      .map(|_| ())
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn setxattr(
    &self,
    _req: Request,
    inode: u64,
    name: &OsStr,
    value: &[u8],
    flags: u32,
    _position: u32
  ) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    #[allow(clippy::cast_possible_wrap)] // xattr flags are small POSIX constants
    let flags = flags as i32;
    self
      .inner
      .mutate(
        &self.state,
        FsMutation::SetXattr {
          path: &path,
          name,
          value,
          flags
        }
      )
      .await
      .map(|_| ())
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn getxattr(&self, _req: Request, inode: u64, name: &OsStr, size: u32) -> Result<ReplyXAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.get_xattr(&self.state, &path, name).await {
      Ok(value) => xattr_reply(value, size),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn listxattr(&self, _req: Request, inode: u64, size: u32) -> Result<ReplyXAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.list_xattr(&self.state, &path).await {
      Ok(attrs) => {
        let mut data = Vec::new();
        for attr in attrs {
          data.extend_from_slice(&os_str_bytes(&attr));
          data.push(0);
        }
        xattr_reply(data, size)
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn removexattr(&self, _req: Request, inode: u64, name: &OsStr) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .mutate(&self.state, FsMutation::RemoveXattr { path: &path, name })
      .await
      .map(|_| ())
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn opendir(&self, _req: Request, inode: u64, _flags: u32) -> Result<ReplyOpen> {
    let _ = self.resolve_path(inode).await?;
    Ok(ReplyOpen { fh: 0, flags: 0 })
  }

  async fn releasedir(&self, _req: Request, _inode: u64, _fh: u64, _flags: u32) -> Result<()> {
    Ok(())
  }

  async fn fsyncdir(&self, _req: Request, _inode: u64, _fh: u64, _datasync: bool) -> Result<()> {
    Ok(())
  }

  async fn bmap(&self, _req: Request, _inode: u64, _blocksize: u32, _idx: u64) -> Result<ReplyBmap> {
    Err(Errno::from(libc::ENOTSUP))
  }

  #[allow(clippy::too_many_arguments)]
  async fn poll(
    &self,
    _req: Request,
    _inode: u64,
    _fh: u64,
    _kh: Option<u64>,
    _flags: u32,
    events: u32,
    _notify: &Notify
  ) -> Result<ReplyPoll> {
    Ok(ReplyPoll { revents: events })
  }

  async fn fallocate(
    &self,
    _req: Request,
    _inode: u64,
    _fh: u64,
    _offset: u64,
    _length: u64,
    _mode: u32
  ) -> Result<()> {
    Err(Errno::from(libc::ENOTSUP))
  }

  async fn lseek(&self, _req: Request, inode: u64, _fh: u64, offset: u64, whence: u32) -> Result<ReplyLSeek> {
    let path = self.resolve_path(inode).await?;
    let meta = self
      .inner
      .lookup(&self.state, &path)
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    #[allow(clippy::cast_sign_loss)]
    let seek_set = libc::SEEK_SET as u32;
    #[allow(clippy::cast_sign_loss)]
    let seek_end = libc::SEEK_END as u32;

    #[cfg(target_os = "linux")]
    {
      #[allow(clippy::cast_sign_loss)]
      let seek_data = libc::SEEK_DATA as u32;
      #[allow(clippy::cast_sign_loss)]
      let seek_hole = libc::SEEK_HOLE as u32;

      if whence == seek_data {
        return if offset <= meta.attr.size {
          Ok(ReplyLSeek { offset })
        } else {
          Err(Errno::from(libc::ENXIO))
        };
      }

      if whence == seek_hole {
        return if offset <= meta.attr.size {
          Ok(ReplyLSeek { offset: meta.attr.size })
        } else {
          Err(Errno::from(libc::ENXIO))
        };
      }
    }

    if whence == seek_set {
      return Ok(ReplyLSeek { offset });
    }

    if whence == seek_end {
      return Ok(ReplyLSeek {
        offset: meta.attr.size.saturating_add(offset)
      });
    }

    Err(Errno::from(libc::ENOTSUP))
  }

  #[allow(clippy::too_many_arguments)]
  async fn copy_file_range(
    &self,
    _req: Request,
    _inode: u64,
    fh_in: u64,
    off_in: u64,
    _inode_out: u64,
    fh_out: u64,
    off_out: u64,
    length: u64,
    _flags: u64
  ) -> Result<ReplyCopyFileRange> {
    let input = self.opened_node(fh_in)?;
    let output = self.opened_node(fh_out)?;
    let mut copied = 0;
    let mut buffer = vec![0; MAX_WRITE.get() as usize];

    while copied < length {
      let remaining = (length - copied).min(u64::from(MAX_WRITE.get()));
      #[allow(clippy::cast_possible_truncation)]
      let remaining = remaining as usize;
      let read = self
        .inner
        .read_into(&input, off_in + copied, &mut buffer[..remaining])
        .await
        .map_err(|e| fs_error_to_errno(&e))?;

      if read == 0 {
        break;
      }

      let written = self
        .inner
        .write_from(&output, off_out + copied, &buffer[..read])
        .await
        .map_err(|e| fs_error_to_errno(&e))?;

      #[allow(clippy::cast_possible_truncation)]
      {
        copied += written as u64;
      }

      if written < read {
        break;
      }
    }

    Ok(ReplyCopyFileRange { copied })
  }

  async fn interrupt(&self, _req: Request, _unique: u64) -> Result<()> {
    debug!("received FUSE interrupt");
    Ok(())
  }
}
