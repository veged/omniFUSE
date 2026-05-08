//! Mount path layout.

use std::{
  collections::hash_map::DefaultHasher,
  hash::{Hash, Hasher},
  path::{Path, PathBuf}
};

use crate::MountEnvironment;

/// Cache identity for a backend source.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CacheKey {
  backend: &'static str,
  identity: String
}

impl CacheKey {
  /// Create a cache key.
  #[must_use]
  pub fn new(backend: &'static str, identity: impl Into<String>) -> Self {
    Self {
      backend,
      identity: identity.into()
    }
  }

  const fn backend(&self) -> &'static str {
    self.backend
  }

  fn hash_for(&self, mount_point: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    self.backend.hash(&mut hasher);
    self.identity.hash(&mut hasher);
    mount_point.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
  }
}

/// Resolved mount paths.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MountLayout {
  /// Canonical mount point visible to users.
  pub mount_point: PathBuf,
  /// Backend working directory under the cache root.
  pub work_dir: PathBuf
}

impl MountLayout {
  /// Resolve mount and work directories.
  ///
  /// # Errors
  ///
  /// Returns an error if environment path resolution fails.
  pub fn resolve(env: &impl MountEnvironment, mount_point: &Path, cache_key: &CacheKey) -> anyhow::Result<Self> {
    let mount_point = env.canonicalize_mount_point(mount_point)?;
    let hash = cache_key.hash_for(&mount_point);
    let work_dir = env
      .cache_base_dir()?
      .join("omnifuse")
      .join(cache_key.backend())
      .join(hash);

    Ok(Self { mount_point, work_dir })
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::path::{Path, PathBuf};

  use crate::{CacheKey, MountLayout, environment::FakeMountEnvironment};

  #[test]
  fn mount_layout_keeps_mount_point_and_work_dir_separate() {
    let env = FakeMountEnvironment::new()
      .home("/home/user")
      .canonical("/mnt/wiki", "/abs/mnt/wiki");

    let layout = MountLayout::resolve(
      &env,
      Path::new("/mnt/wiki"),
      &CacheKey::new("wiki", "https://api.example.test/root")
    )
    .expect("layout");

    assert_eq!(layout.mount_point, PathBuf::from("/abs/mnt/wiki"));
    assert_ne!(layout.mount_point, layout.work_dir);
    assert!(
      layout
        .work_dir
        .parent()
        .is_some_and(|parent| parent.ends_with("omnifuse/wiki"))
    );
  }

  #[test]
  fn same_source_and_mount_produce_stable_work_dir() {
    let env = FakeMountEnvironment::new()
      .home("/home/user")
      .canonical("/mnt/repo", "/abs/mnt/repo");

    let first = MountLayout::resolve(&env, Path::new("/mnt/repo"), &CacheKey::new("git", "source-a")).expect("first");
    let second = MountLayout::resolve(&env, Path::new("/mnt/repo"), &CacheKey::new("git", "source-a")).expect("second");

    assert_eq!(first.work_dir, second.work_dir);
  }

  #[test]
  fn different_sources_produce_different_work_dirs() {
    let env = FakeMountEnvironment::new()
      .home("/home/user")
      .canonical("/mnt/repo", "/abs/mnt/repo");

    let first = MountLayout::resolve(&env, Path::new("/mnt/repo"), &CacheKey::new("git", "source-a")).expect("first");
    let second = MountLayout::resolve(&env, Path::new("/mnt/repo"), &CacheKey::new("git", "source-b")).expect("second");

    assert_ne!(first.work_dir, second.work_dir);
  }
}
