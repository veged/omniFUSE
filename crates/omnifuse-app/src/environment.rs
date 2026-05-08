//! Mount environment abstraction.

use std::{
  ffi::OsString,
  path::{Path, PathBuf}
};

/// Environment operations needed to prepare mount paths.
pub trait MountEnvironment {
  /// Resolve the current user's home directory.
  ///
  /// # Errors
  ///
  /// Returns an error if `HOME` is not set.
  fn home_dir(&self) -> anyhow::Result<PathBuf>;

  /// Resolve the base cache directory.
  ///
  /// # Errors
  ///
  /// Returns an error if the home directory cannot be resolved.
  fn cache_base_dir(&self) -> anyhow::Result<PathBuf>;

  /// Canonicalize a mount point, allowing the final path component to be absent.
  ///
  /// # Errors
  ///
  /// Returns an error if neither the mount point nor its parent can be canonicalized.
  fn canonicalize_mount_point(&self, mount_point: &Path) -> anyhow::Result<PathBuf>;
}

/// Standard process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdMountEnvironment;

impl MountEnvironment for StdMountEnvironment {
  fn home_dir(&self) -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
      .filter(|home| !home.is_empty())
      .map(PathBuf::from)
      .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
  }

  fn cache_base_dir(&self) -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
      Ok(self.home_dir()?.join("Library/Caches"))
    }

    #[cfg(not(target_os = "macos"))]
    {
      Ok(std::env::var_os("XDG_CACHE_HOME").map_or_else(
        || self.home_dir().map(|home| home.join(".cache")),
        |path| Ok(PathBuf::from(path))
      )?)
    }
  }

  fn canonicalize_mount_point(&self, mount_point: &Path) -> anyhow::Result<PathBuf> {
    canonicalize_mount_point(mount_point)
  }
}

fn canonicalize_mount_point(mount_point: &Path) -> anyhow::Result<PathBuf> {
  match mount_point.canonicalize() {
    Ok(path) => Ok(path),
    Err(error) => {
      let parent = mount_point.parent().unwrap_or_else(|| Path::new("."));
      let file_name = mount_point.file_name().map_or_else(OsString::new, OsString::from);
      let canonical_parent = parent.canonicalize().map_err(|parent_error| {
        anyhow::anyhow!(
          "canonicalizing {}: {error}; parent: {parent_error}",
          mount_point.display()
        )
      })?;

      Ok(canonical_parent.join(file_name))
    }
  }
}

#[cfg(test)]
pub(crate) mod tests_support {
  use std::{
    collections::HashMap,
    path::{Path, PathBuf}
  };

  use super::MountEnvironment;

  #[derive(Debug, Clone)]
  pub(crate) struct FakeMountEnvironment {
    home: PathBuf,
    canonical_paths: HashMap<PathBuf, PathBuf>
  }

  impl FakeMountEnvironment {
    pub(crate) fn new() -> Self {
      Self {
        home: PathBuf::from("/home/user"),
        canonical_paths: HashMap::new()
      }
    }

    pub(crate) fn home(mut self, home: impl Into<PathBuf>) -> Self {
      self.home = home.into();
      self
    }

    pub(crate) fn canonical(mut self, input: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
      self.canonical_paths.insert(input.into(), output.into());
      self
    }
  }

  impl MountEnvironment for FakeMountEnvironment {
    fn home_dir(&self) -> anyhow::Result<PathBuf> {
      Ok(self.home.clone())
    }

    fn cache_base_dir(&self) -> anyhow::Result<PathBuf> {
      Ok(self.home.join(".cache"))
    }

    fn canonicalize_mount_point(&self, mount_point: &Path) -> anyhow::Result<PathBuf> {
      Ok(
        self
          .canonical_paths
          .get(mount_point)
          .cloned()
          .unwrap_or_else(|| mount_point.to_path_buf())
      )
    }
  }
}

#[cfg(test)]
pub(crate) use tests_support::FakeMountEnvironment;
