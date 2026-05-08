//! Shared dirty path index for protecting local changes from remote refresh.

use std::{
  collections::HashSet,
  path::{Path, PathBuf},
  sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard}
};

/// Path protection contract used by remote refresh.
pub trait PathProtection: Send + Sync {
  /// Returns true when a path must not be overwritten by remote changes.
  fn is_protected(&self, path: &Path) -> bool;
}

/// Thread-safe set of locally dirty paths.
#[derive(Debug, Default)]
pub struct DirtyIndex {
  paths: RwLock<HashSet<PathBuf>>
}

impl DirtyIndex {
  /// Mark a path as dirty.
  pub fn mark_dirty(&self, path: PathBuf) {
    self.write_paths().insert(path);
  }

  /// Mark a path as clean.
  pub fn mark_clean(&self, path: &Path) {
    self.write_paths().remove(path);
  }

  /// Return a snapshot of currently dirty paths.
  #[must_use]
  pub fn snapshot(&self) -> Vec<PathBuf> {
    self.read_paths().iter().cloned().collect()
  }

  /// Returns true when a path is dirty or is inside a dirty directory.
  #[must_use]
  pub fn is_protected(&self, path: &Path) -> bool {
    self
      .read_paths()
      .iter()
      .any(|dirty_path| path == dirty_path || path.starts_with(dirty_path))
  }

  /// Borrow as a path protection trait object.
  #[must_use]
  pub fn as_path_protection(&self) -> &dyn PathProtection {
    self
  }

  fn read_paths(&self) -> RwLockReadGuard<'_, HashSet<PathBuf>> {
    self.paths.read().unwrap_or_else(PoisonError::into_inner)
  }

  fn write_paths(&self) -> RwLockWriteGuard<'_, HashSet<PathBuf>> {
    self.paths.write().unwrap_or_else(PoisonError::into_inner)
  }
}

impl PathProtection for DirtyIndex {
  fn is_protected(&self, path: &Path) -> bool {
    self.is_protected(path)
  }
}

#[cfg(test)]
mod tests {
  use std::path::{Path, PathBuf};

  use super::DirtyIndex;

  #[test]
  fn dirty_index_tracks_and_untracks_paths() {
    let index = DirtyIndex::default();
    let path = PathBuf::from("docs/page.md");

    index.mark_dirty(path.clone());
    assert!(index.is_protected(&path));

    index.mark_clean(&path);
    assert!(!index.is_protected(&path));
  }

  #[test]
  fn dirty_index_protects_descendants_when_directory_is_dirty() {
    let index = DirtyIndex::default();
    index.mark_dirty(PathBuf::from("docs"));

    assert!(index.is_protected(Path::new("docs/page.md")));
  }
}
