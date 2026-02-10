//! Адаптер rfuse3 → async `UniFuseFilesystem`.
//!
//! Преобразует inode-based вызовы rfuse3 в path-based async вызовы
//! `UniFuseFilesystem` trait'а.
//!
//! Маппинг типов:
//! - `rfuse3::Inode` (u64) → `Path` (через `InodeMap`)
//! - `rfuse3::raw::reply::FileAttr` → `unifuse::FileAttr`
//! - `unifuse::FsError` → `rfuse3::Errno`

use std::{
  ffi::{OsStr, OsString},
  path::PathBuf,
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH}
};

use futures_util::stream;
use rfuse3::{Errno, Timestamp, raw::prelude::*};
use tokio::sync::RwLock;
use tracing::debug;

use crate::{
  FileType, FsError, InodeMap, NodeKind, OpenFlags, ROOT_INODE, UniFuseFilesystem,
  types::{self, FileHandle as UfFileHandle}
};

/// Алиас для rfuse3 `Result`.
type Result<T> = std::result::Result<T, Errno>;

/// TTL для кеширования атрибутов (1 минута).
const TTL: Duration = Duration::from_secs(60);

/// Максимальный размер записи (1 МБ).
const MAX_WRITE: std::num::NonZeroU32 = match std::num::NonZeroU32::new(1024 * 1024) {
  Some(v) => v,
  None => panic!("MAX_WRITE не может быть нулём")
};

/// Адаптер: преобразует inode-based вызовы rfuse3 → path-based async `UniFuseFilesystem`.
///
/// Содержит `InodeMap` для маппинга inode ↔ path.
pub struct Rfuse3Adapter<F: UniFuseFilesystem> {
  inner: Arc<F>,
  inode_map: Arc<RwLock<InodeMap>>
}

impl<F: UniFuseFilesystem> Rfuse3Adapter<F> {
  /// Создать новый адаптер для файловой системы.
  pub fn new(fs: Arc<F>, root: PathBuf) -> Self {
    Self {
      inner: fs,
      inode_map: Arc::new(RwLock::new(InodeMap::new(root)))
    }
  }

  /// Получить путь по inode, или вернуть ENOENT.
  async fn resolve_path(&self, inode: u64) -> Result<PathBuf> {
    self
      .inode_map
      .read()
      .await
      .get_path(inode)
      .ok_or_else(|| Errno::from(libc::ENOENT))
  }
}

// --- Конвертация типов ---

/// Преобразовать `unifuse::FileAttr` → `rfuse3::raw::reply::FileAttr`.
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

/// Преобразовать `unifuse::FileType` → `rfuse3::FileType`.
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

/// Преобразовать `unifuse::FileType` → `inode::NodeKind`.
const fn to_node_kind(kind: FileType) -> NodeKind {
  match kind {
    FileType::Directory => NodeKind::Dir,
    _ => NodeKind::File
  }
}

/// Преобразовать `SystemTime` → rfuse3 `Timestamp`.
fn system_time_to_timestamp(time: SystemTime) -> Timestamp {
  let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
  Timestamp::new(
    i64::try_from(duration.as_secs()).unwrap_or(0),
    duration.subsec_nanos()
  )
}

/// Преобразовать rfuse3 `Timestamp` → `SystemTime`.
fn timestamp_to_system_time(ts: Timestamp) -> SystemTime {
  let secs = u64::try_from(ts.sec).unwrap_or(0);
  UNIX_EPOCH + Duration::new(secs, ts.nsec)
}

/// Преобразовать `FsError` → rfuse3 `Errno`.
fn fs_error_to_errno(err: &FsError) -> Errno {
  Errno::from(err.to_errno())
}

/// Преобразовать rfuse3 open flags → `unifuse::OpenFlags`.
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
impl<F: UniFuseFilesystem> rfuse3::raw::Filesystem for Rfuse3Adapter<F> {
  async fn init(&self, _req: Request) -> Result<ReplyInit> {
    debug!("FUSE init");
    Ok(ReplyInit {
      max_write: MAX_WRITE
    })
  }

  async fn destroy(&self, _req: Request) {
    debug!("FUSE destroy");
  }

  async fn lookup(&self, _req: Request, parent: u64, name: &OsStr) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let child_path = parent_path.join(name);

    match self.inner.lookup(&parent_path, name).await {
      Ok(attr) => {
        let kind = to_node_kind(attr.kind);
        let ino = self.inode_map.write().await.get_or_insert(&child_path, kind);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn forget(&self, _req: Request, inode: u64, _nlookup: u64) {
    debug!(inode, "forget (noop)");
  }

  async fn getattr(
    &self,
    _req: Request,
    inode: u64,
    _fh: Option<u64>,
    _flags: u32
  ) -> Result<ReplyAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.getattr(&path).await {
      Ok(attr) => Ok(ReplyAttr {
        ttl: TTL,
        attr: to_rfuse3_attr(&attr, inode)
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn setattr(
    &self,
    _req: Request,
    inode: u64,
    _fh: Option<u64>,
    set_attr: SetAttr
  ) -> Result<ReplyAttr> {
    let path = self.resolve_path(inode).await?;

    let size = set_attr.size;
    let atime = set_attr.atime.map(timestamp_to_system_time);
    let mtime = set_attr.mtime.map(timestamp_to_system_time);
    let mode = set_attr.mode.map(u32::from);

    match self.inner.setattr(&path, size, atime, mtime, mode).await {
      Ok(attr) => Ok(ReplyAttr {
        ttl: TTL,
        attr: to_rfuse3_attr(&attr, inode)
      }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn open(&self, _req: Request, inode: u64, flags: u32) -> Result<ReplyOpen> {
    let path = self.resolve_path(inode).await?;
    let open_flags = decode_open_flags(flags);

    match self.inner.open(&path, open_flags).await {
      Ok(fh) => Ok(ReplyOpen { fh: fh.0, flags: 0 }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn read(
    &self,
    _req: Request,
    inode: u64,
    fh: u64,
    offset: u64,
    size: u32
  ) -> Result<ReplyData> {
    let path = self.resolve_path(inode).await?;

    match self.inner.read(&path, UfFileHandle(fh), offset, size).await {
      Ok(data) => Ok(ReplyData { data: data.into() }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn write(
    &self,
    _req: Request,
    inode: u64,
    fh: u64,
    offset: u64,
    data: &[u8],
    _write_flags: u32,
    _flags: u32
  ) -> Result<ReplyWrite> {
    let path = self.resolve_path(inode).await?;

    match self.inner.write(&path, UfFileHandle(fh), offset, data).await {
      Ok(written) => Ok(ReplyWrite { written }),
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn release(
    &self,
    _req: Request,
    inode: u64,
    fh: u64,
    _flags: u32,
    _lock_owner: u64,
    _flush: bool
  ) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .release(&path, UfFileHandle(fh))
      .await
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn flush(&self, _req: Request, inode: u64, fh: u64, _lock_owner: u64) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .flush(&path, UfFileHandle(fh))
      .await
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn fsync(&self, _req: Request, inode: u64, fh: u64, datasync: bool) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .fsync(&path, UfFileHandle(fh), datasync)
      .await
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

    match self.inner.readdir(&path).await {
      Ok(entries) => {
        let mut dir_entries: Vec<DirectoryEntry> = Vec::new();
        let mut index: i64 = 1;

        // Добавляем . и ..
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

        // Собираем записи из UniFuseFilesystem
        for entry in entries {
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

  async fn mkdir(
    &self,
    _req: Request,
    parent: u64,
    name: &OsStr,
    mode: u32,
    _umask: u32
  ) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let dir_path = parent_path.join(name);

    match self.inner.mkdir(&dir_path, mode).await {
      Ok(attr) => {
        let ino = self
          .inode_map
          .write()
          .await
          .get_or_insert(&dir_path, NodeKind::Dir);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&attr, ino),
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
      .rmdir(&dir_path)
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
      .unlink(&file_path)
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
      .rename(&old_path, &new_path, 0)
      .await
      .map_err(|e| fs_error_to_errno(&e))?;

    self.inode_map.write().await.rename(&old_path, &new_path);
    Ok(())
  }

  async fn create(
    &self,
    _req: Request,
    parent: u64,
    name: &OsStr,
    mode: u32,
    flags: u32
  ) -> Result<ReplyCreated> {
    let parent_path = self.resolve_path(parent).await?;
    let file_path = parent_path.join(name);
    let open_flags = decode_open_flags(flags);

    match self.inner.create(&file_path, open_flags, mode).await {
      Ok((fh, attr)) => {
        let ino = self
          .inode_map
          .write()
          .await
          .get_or_insert(&file_path, NodeKind::File);
        Ok(ReplyCreated {
          ttl: TTL,
          attr: to_rfuse3_attr(&attr, ino),
          generation: 0,
          fh: fh.0,
          flags: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn symlink(
    &self,
    _req: Request,
    parent: u64,
    name: &OsStr,
    link_path: &OsStr
  ) -> Result<ReplyEntry> {
    let parent_path = self.resolve_path(parent).await?;
    let symlink_path = parent_path.join(name);
    let target = PathBuf::from(link_path);

    match self.inner.symlink(&target, &symlink_path).await {
      Ok(attr) => {
        let ino = self
          .inode_map
          .write()
          .await
          .get_or_insert(&symlink_path, NodeKind::File);
        Ok(ReplyEntry {
          ttl: TTL,
          attr: to_rfuse3_attr(&attr, ino),
          generation: 0
        })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn readlink(&self, _req: Request, inode: u64) -> Result<ReplyData> {
    let path = self.resolve_path(inode).await?;

    match self.inner.readlink(&path).await {
      Ok(target) => {
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        Ok(ReplyData { data: bytes.into() })
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn statfs(&self, _req: Request, inode: u64) -> Result<ReplyStatFs> {
    let path = self.resolve_path(inode).await?;

    match self.inner.statfs(&path).await {
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

  async fn access(&self, _req: Request, _inode: u64, _mask: u32) -> Result<()> {
    Ok(())
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
    self
      .inner
      .setxattr(&path, name, value, flags.cast_signed())
      .await
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn getxattr(
    &self,
    _req: Request,
    inode: u64,
    name: &OsStr,
    size: u32
  ) -> Result<ReplyXAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.getxattr(&path, name).await {
      Ok(data) => {
        if size == 0 {
          #[allow(clippy::cast_possible_truncation)]
          Ok(ReplyXAttr::Size(data.len() as u32))
        } else {
          Ok(ReplyXAttr::Data(data.into()))
        }
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn listxattr(&self, _req: Request, inode: u64, size: u32) -> Result<ReplyXAttr> {
    let path = self.resolve_path(inode).await?;

    match self.inner.listxattr(&path).await {
      Ok(attrs) => {
        let mut data = Vec::new();
        for attr in &attrs {
          data.extend_from_slice(attr.as_encoded_bytes());
          data.push(0);
        }

        if size == 0 {
          #[allow(clippy::cast_possible_truncation)]
          Ok(ReplyXAttr::Size(data.len() as u32))
        } else {
          Ok(ReplyXAttr::Data(data.into()))
        }
      }
      Err(e) => Err(fs_error_to_errno(&e))
    }
  }

  async fn removexattr(&self, _req: Request, inode: u64, name: &OsStr) -> Result<()> {
    let path = self.resolve_path(inode).await?;
    self
      .inner
      .removexattr(&path, name)
      .await
      .map_err(|e| fs_error_to_errno(&e))
  }

  async fn opendir(&self, _req: Request, inode: u64, _flags: u32) -> Result<ReplyOpen> {
    let _ = self.resolve_path(inode).await?;
    Ok(ReplyOpen { fh: 0, flags: 0 })
  }

  async fn releasedir(&self, _req: Request, _inode: u64, _fh: u64, _flags: u32) -> Result<()> {
    Ok(())
  }

  async fn interrupt(&self, _req: Request, _unique: u64) -> Result<()> {
    debug!("получен FUSE interrupt");
    Ok(())
  }
}
