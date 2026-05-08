//! Compatibility adapter from `UniFuseFilesystem` to `SessionPathFs`.

use std::path::Path;

use crate::{
  CloseReason, DirPage, DirPageRequest, FileAttr, FlushMode, FsError, FsMutation, MountContext, NodeMeta, OpenIntent,
  OpenedNode, SessionPathFs, StatFs, UniFuseFilesystem, UnmountReason
};

/// Session adapter for the legacy `UniFuseFilesystem` trait.
pub struct CompatSessionFs<F> {
  inner: F
}

impl<F> CompatSessionFs<F> {
  /// Wrap a legacy filesystem.
  #[must_use]
  pub const fn new(inner: F) -> Self {
    Self { inner }
  }

  /// Return the wrapped filesystem.
  #[must_use]
  pub fn into_inner(self) -> F {
    self.inner
  }
}

impl<F: UniFuseFilesystem> SessionPathFs for CompatSessionFs<F> {
  type MountState = ();

  async fn start(&self, _context: MountContext) -> Result<Self::MountState, FsError> {
    Ok(())
  }

  async fn stop(&self, _state: Self::MountState, _reason: UnmountReason) -> Result<(), FsError> {
    Ok(())
  }

  async fn lookup(&self, _state: &Self::MountState, path: &Path) -> Result<NodeMeta, FsError> {
    let attr = self.inner.getattr(path).await?;
    Ok(NodeMeta::new(path.to_path_buf(), attr))
  }

  async fn open(&self, _state: &Self::MountState, path: &Path, intent: OpenIntent) -> Result<OpenedNode, FsError> {
    let handle = self.inner.open(path, intent.flags).await?;
    let attr = self.inner.getattr(path).await?;
    Ok(OpenedNode::new(path.to_path_buf(), handle, attr.kind, attr))
  }

  async fn read_into(&self, file: &OpenedNode, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
    #[allow(clippy::cast_possible_truncation)]
    let size = out.len() as u32;
    let data = self.inner.read(&file.path, file.handle, offset, size).await?;
    let copied = data.len().min(out.len());
    out[..copied].copy_from_slice(&data[..copied]);
    Ok(copied)
  }

  async fn write_from(&self, file: &OpenedNode, offset: u64, data: &[u8]) -> Result<usize, FsError> {
    let written = self.inner.write(&file.path, file.handle, offset, data).await?;
    Ok(written as usize)
  }

  async fn flush(&self, file: &OpenedNode, mode: FlushMode) -> Result<NodeMeta, FsError> {
    match mode {
      FlushMode::Flush => self.inner.flush(&file.path, file.handle).await?,
      FlushMode::Fsync { datasync } => self.inner.fsync(&file.path, file.handle, datasync).await?
    }

    let attr = self.inner.getattr(&file.path).await?;
    Ok(NodeMeta::new(file.path.clone(), attr))
  }

  async fn close(&self, file: OpenedNode, _reason: CloseReason) -> Result<(), FsError> {
    self.inner.release(&file.path, file.handle).await
  }

  async fn read_dir(&self, _state: &Self::MountState, path: &Path, page: DirPageRequest) -> Result<DirPage, FsError> {
    let entries = self.inner.readdir(path).await?;
    let mut entries: Vec<_> = entries.into_iter().map(Into::into).collect();
    if page.offset > 0 {
      entries = entries.into_iter().skip(page.offset).collect();
    }
    if let Some(limit) = page.limit {
      let next_offset = (entries.len() > limit).then_some(page.offset + limit);
      entries.truncate(limit);
      return Ok(DirPage { entries, next_offset });
    }

    Ok(DirPage::all(entries))
  }

  async fn mutate(&self, _state: &Self::MountState, mutation: FsMutation<'_>) -> Result<NodeMeta, FsError> {
    match mutation {
      FsMutation::Create { path, flags, mode } => {
        let (_handle, attr) = self.inner.create(path, flags, mode).await?;
        Ok(NodeMeta::new(path.to_path_buf(), attr))
      }
      FsMutation::Mkdir { path, mode } => {
        let attr = self.inner.mkdir(path, mode).await?;
        Ok(NodeMeta::new(path.to_path_buf(), attr))
      }
      FsMutation::Unlink { path } => {
        self.inner.unlink(path).await?;
        Ok(NodeMeta::new(path.to_path_buf(), FileAttr::regular(0, 0o644)))
      }
      FsMutation::Rmdir { path } => {
        self.inner.rmdir(path).await?;
        Ok(NodeMeta::new(path.to_path_buf(), FileAttr::directory(0o755)))
      }
      FsMutation::Rename { from, to, flags } => {
        self.inner.rename(from, to, flags).await?;
        let attr = self.inner.getattr(to).await?;
        Ok(NodeMeta::new(to.to_path_buf(), attr))
      }
      FsMutation::Symlink { target, link } => {
        let attr = self.inner.symlink(target, link).await?;
        Ok(NodeMeta::new(link.to_path_buf(), attr))
      }
      FsMutation::Readlink { .. }
      | FsMutation::SetAttr { .. }
      | FsMutation::SetXattr { .. }
      | FsMutation::RemoveXattr { .. } => Err(FsError::NotSupported)
    }
  }

  async fn statfs(&self, _state: &Self::MountState) -> Result<StatFs, FsError> {
    self.inner.statfs(Path::new("/")).await
  }
}

#[cfg(test)]
mod tests {
  use std::{
    ffi::OsStr,
    path::{Path, PathBuf}
  };

  use super::*;
  use crate::{DirEntry, FileHandle, OpenFlags};

  struct RecordingOldFs {
    path: PathBuf,
    content: Vec<u8>
  }

  impl RecordingOldFs {
    fn with_file(path: &str, content: &[u8]) -> Self {
      Self {
        path: PathBuf::from(path),
        content: content.to_vec()
      }
    }
  }

  impl UniFuseFilesystem for RecordingOldFs {
    async fn getattr(&self, path: &Path) -> Result<FileAttr, FsError> {
      if path == self.path {
        Ok(FileAttr::regular(self.content.len() as u64, 0o644))
      } else {
        Err(FsError::NotFound)
      }
    }

    async fn lookup(&self, parent: &Path, name: &OsStr) -> Result<FileAttr, FsError> {
      self.getattr(&parent.join(name)).await
    }

    async fn open(&self, path: &Path, _flags: OpenFlags) -> Result<FileHandle, FsError> {
      if path == self.path {
        Ok(FileHandle(7))
      } else {
        Err(FsError::NotFound)
      }
    }

    async fn read(&self, path: &Path, _fh: FileHandle, offset: u64, size: u32) -> Result<Vec<u8>, FsError> {
      if path != self.path {
        return Err(FsError::NotFound);
      }

      let start = offset as usize;
      let end = (start + size as usize).min(self.content.len());
      Ok(self.content[start..end].to_vec())
    }

    async fn release(&self, _path: &Path, _fh: FileHandle) -> Result<(), FsError> {
      Ok(())
    }

    async fn readdir(&self, _path: &Path) -> Result<Vec<DirEntry>, FsError> {
      Ok(Vec::new())
    }

    async fn statfs(&self, _path: &Path) -> Result<StatFs, FsError> {
      Ok(StatFs {
        blocks: 1,
        bfree: 1,
        bavail: 1,
        files: 1,
        ffree: 1,
        bsize: 512,
        namelen: 255
      })
    }
  }

  #[tokio::test]
  async fn compat_session_delegates_open_read_close_to_old_trait() {
    let old = RecordingOldFs::with_file("doc.md", b"hello");
    let compat = CompatSessionFs::new(old);
    let state = compat.start(MountContext::test("/mnt/test")).await.expect("start");

    let opened = compat
      .open(&state, Path::new("doc.md"), OpenIntent::read_only())
      .await
      .expect("open");
    let mut out = vec![0; 5];
    let read = compat.read_into(&opened, 0, &mut out).await.expect("read");
    compat.close(opened, CloseReason::Released).await.expect("close");

    assert_eq!(read, 5);
    assert_eq!(&out, b"hello");
  }
}
