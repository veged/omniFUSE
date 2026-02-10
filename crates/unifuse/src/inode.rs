//! Inode-to-path mapping for FUSE.
//!
//! A bidirectional map between inode numbers (`u64`) and filesystem paths.
//! Uses FNV-1a hash for deterministic inode number generation.
//!
//! `WinFsp` works with paths directly — it does NOT need `InodeMap`.

use std::path::{Path, PathBuf};

use dashmap::DashMap;
use tracing::{debug, error};

/// Root inode number (always 1 for FUSE).
pub const ROOT_INODE: u64 = 1;

/// FNV-1a offset basis.
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;

/// FNV-1a prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// Filesystem node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
  /// Regular file.
  File,
  /// Directory.
  Dir
}

/// Bidirectional inode-to-path map.
///
/// Uses FNV-1a hash for deterministic inode generation:
/// the same path always produces the same inode number.
#[derive(Debug)]
pub struct InodeMap {
  /// inode -> path.
  inode_to_path: DashMap<u64, PathBuf>,
  /// path -> inode.
  path_to_inode: DashMap<PathBuf, u64>,
  /// Root path of the filesystem.
  root: PathBuf
}

impl InodeMap {
  /// Create a new map with the given root path.
  #[must_use]
  pub fn new(root: PathBuf) -> Self {
    let map = Self {
      inode_to_path: DashMap::new(),
      path_to_inode: DashMap::new(),
      root: root.clone()
    };

    // Register the root directory
    map.inode_to_path.insert(ROOT_INODE, root.clone());
    map.path_to_inode.insert(root, ROOT_INODE);

    map
  }

  /// Get the root path.
  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Compute an inode for a path via FNV-1a hash.
  ///
  /// The hash includes the node type to distinguish files and directories
  /// with the same name.
  #[must_use]
  pub fn compute_inode(path: &Path, kind: NodeKind) -> u64 {
    let path_str = path.to_string_lossy();

    // Root directory is always `ROOT_INODE`
    if path_str.is_empty() || path_str == "/" || path_str == "." {
      return ROOT_INODE;
    }

    let mut h: u64 = FNV_OFFSET;

    // Include the node type in the hash
    let kind_prefix = match kind {
      NodeKind::File => b"f:",
      NodeKind::Dir => b"d:"
    };

    for &b in kind_prefix.iter().chain(path_str.as_bytes().iter()) {
      h ^= u64::from(b);
      h = h.wrapping_mul(FNV_PRIME);
    }

    // Avoid collision with the root inode
    if h == ROOT_INODE {
      h = 2;
    }

    // Avoid 0 (invalid inode)
    if h == 0 {
      h = u64::MAX;
    }

    h
  }

  /// Get or create an inode for a path.
  ///
  /// If the path is already registered, returns the existing inode.
  /// Otherwise, computes and registers a new one.
  pub fn get_or_insert(&self, path: &Path, kind: NodeKind) -> u64 {
    // Check if the path is registered
    if let Some(inode) = self.path_to_inode.get(path) {
      return *inode;
    }

    // Compute a new inode
    let inode = Self::compute_inode(path, kind);

    // Check for collision
    if self.inode_to_path.contains_key(&inode) {
      let existing = self.inode_to_path.get(&inode);
      error!(
        inode,
        new_path = %path.display(),
        existing_path = %existing.map_or_else(|| "unknown".to_string(), |p| p.display().to_string()),
        "inode collision detected"
      );
    }

    // Register the mapping
    let path_buf = path.to_path_buf();
    self.inode_to_path.insert(inode, path_buf.clone());
    self.path_to_inode.insert(path_buf, inode);

    debug!(inode, path = %path.display(), ?kind, "inode registered");

    inode
  }

  /// Get a path by inode number.
  #[must_use]
  pub fn get_path(&self, inode: u64) -> Option<PathBuf> {
    self.inode_to_path.get(&inode).map(|r| r.value().clone())
  }

  /// Get an inode by path.
  #[must_use]
  pub fn get_inode(&self, path: &Path) -> Option<u64> {
    self.path_to_inode.get(path).map(|r| *r.value())
  }

  /// Remove a path from the map.
  pub fn remove(&self, path: &Path) {
    if let Some((_, inode)) = self.path_to_inode.remove(path) {
      self.inode_to_path.remove(&inode);
      debug!(inode, path = %path.display(), "inode mapping removed");
    }
  }

  /// Rename a path (update the mapping).
  pub fn rename(&self, old_path: &Path, new_path: &Path) {
    if let Some((_, inode)) = self.path_to_inode.remove(old_path) {
      self.inode_to_path.insert(inode, new_path.to_path_buf());
      self.path_to_inode.insert(new_path.to_path_buf(), inode);
      debug!(
        inode,
        old = %old_path.display(),
        new = %new_path.display(),
        "inode mapping renamed"
      );
    }
  }

  /// Check whether an inode exists.
  #[must_use]
  pub fn contains_inode(&self, inode: u64) -> bool {
    self.inode_to_path.contains_key(&inode)
  }

  /// Check whether a path exists.
  #[must_use]
  pub fn contains_path(&self, path: &Path) -> bool {
    self.path_to_inode.contains_key(path)
  }

  /// Number of registered inodes.
  #[must_use]
  pub fn len(&self) -> usize {
    self.inode_to_path.len()
  }

  /// Whether the map is empty.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.inode_to_path.is_empty()
  }

  /// Clear all mappings except the root.
  pub fn clear(&self) {
    self.inode_to_path.clear();
    self.path_to_inode.clear();

    // Re-register the root
    self.inode_to_path.insert(ROOT_INODE, self.root.clone());
    self.path_to_inode.insert(self.root.clone(), ROOT_INODE);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_root_inode() {
    let map = InodeMap::new(PathBuf::from("/tmp/test"));
    assert_eq!(map.get_inode(Path::new("/tmp/test")), Some(ROOT_INODE));
    assert_eq!(map.get_path(ROOT_INODE), Some(PathBuf::from("/tmp/test")));
  }

  #[test]
  fn test_compute_inode_deterministic() {
    let path = Path::new("/tmp/test/file.txt");
    let inode1 = InodeMap::compute_inode(path, NodeKind::File);
    let inode2 = InodeMap::compute_inode(path, NodeKind::File);
    assert_eq!(inode1, inode2);
  }

  #[test]
  fn test_file_dir_different_inodes() {
    let path = Path::new("/tmp/test/name");
    let file_inode = InodeMap::compute_inode(path, NodeKind::File);
    let dir_inode = InodeMap::compute_inode(path, NodeKind::Dir);
    assert_ne!(file_inode, dir_inode);
  }

  #[test]
  fn test_get_or_insert() {
    let map = InodeMap::new(PathBuf::from("/tmp/test"));
    let path = Path::new("/tmp/test/subdir/file.txt");

    let inode1 = map.get_or_insert(path, NodeKind::File);
    let inode2 = map.get_or_insert(path, NodeKind::File);

    assert_eq!(inode1, inode2);
    assert_eq!(map.get_path(inode1), Some(path.to_path_buf()));
  }

  #[test]
  fn test_remove() {
    let map = InodeMap::new(PathBuf::from("/tmp/test"));
    let path = Path::new("/tmp/test/file.txt");

    let inode = map.get_or_insert(path, NodeKind::File);
    assert!(map.contains_inode(inode));
    assert!(map.contains_path(path));

    map.remove(path);
    assert!(!map.contains_inode(inode));
    assert!(!map.contains_path(path));
  }

  #[test]
  fn test_rename() {
    let map = InodeMap::new(PathBuf::from("/tmp/test"));
    let old_path = Path::new("/tmp/test/old.txt");
    let new_path = Path::new("/tmp/test/new.txt");

    let inode = map.get_or_insert(old_path, NodeKind::File);
    map.rename(old_path, new_path);

    assert!(!map.contains_path(old_path));
    assert!(map.contains_path(new_path));
    assert_eq!(map.get_path(inode), Some(new_path.to_path_buf()));
  }

  #[test]
  fn test_clear_preserves_root() {
    let map = InodeMap::new(PathBuf::from("/tmp/test"));
    let path = Path::new("/tmp/test/file.txt");

    map.get_or_insert(path, NodeKind::File);
    assert_eq!(map.len(), 2); // root + file

    map.clear();
    assert_eq!(map.len(), 1); // only root
    assert!(map.contains_inode(ROOT_INODE));
    assert!(!map.contains_path(path));
  }
}
