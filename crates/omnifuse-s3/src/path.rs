//! Local path and S3 object path mapping.

use std::path::{Component, Path, PathBuf};

use crate::S3Error;

/// Relative object path inside the OpenDAL operator root.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObjectPath(String);

impl ObjectPath {
  /// Parse a local relative path into a safe object path.
  ///
  /// # Errors
  ///
  /// Returns an error for absolute paths, parent traversal, empty paths, and `.vfs` paths.
  pub fn from_local(path: &Path) -> Result<Self, S3Error> {
    let parts = validate_components(path)?;
    Ok(Self(parts.join("/")))
  }

  /// Parse a remote object key into a safe object path.
  ///
  /// # Errors
  ///
  /// Returns an error for keys that cannot be represented safely under `local_dir`.
  pub fn from_remote(key: &str) -> Result<Self, S3Error> {
    let trimmed = key.trim_matches('/');
    let path = Path::new(trimmed);
    let parts = validate_components(path).map_err(|_| S3Error::InvalidPath(PathBuf::from(key)))?;
    Ok(Self(parts.join("/")))
  }

  /// Borrow the object path.
  #[must_use]
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// Convert to a local relative path.
  #[must_use]
  pub fn to_local_path(&self) -> PathBuf {
    self.0.split('/').collect()
  }
}

fn validate_components(path: &Path) -> Result<Vec<String>, S3Error> {
  if path.is_absolute() {
    return Err(S3Error::InvalidPath(path.to_path_buf()));
  }

  let mut parts = Vec::new();
  for component in path.components() {
    match component {
      Component::Normal(part) => {
        let part = part.to_string_lossy().to_string();
        if part.is_empty() || part == ".vfs" {
          return Err(S3Error::InvalidPath(path.to_path_buf()));
        }
        parts.push(part);
      }
      Component::CurDir => {}
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
        return Err(S3Error::InvalidPath(path.to_path_buf()));
      }
    }
  }

  if parts.is_empty() {
    return Err(S3Error::InvalidPath(path.to_path_buf()));
  }

  Ok(parts)
}

/// Whether a path is internal OmniFuse metadata.
#[must_use]
pub fn is_internal_path(path: &Path) -> bool {
  path.components().any(|component| component.as_os_str() == ".vfs")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn rejects_internal_and_traversal_paths() {
    assert!(ObjectPath::from_local(Path::new(".vfs/s3/manifest.json")).is_err());
    assert!(ObjectPath::from_local(Path::new("docs/.vfs/file")).is_err());
    assert!(ObjectPath::from_local(Path::new("../secret")).is_err());
    assert!(ObjectPath::from_remote(".vfs/s3/manifest.json").is_err());
    assert!(ObjectPath::from_remote("docs/.vfs/file").is_err());
    assert!(ObjectPath::from_remote("../secret").is_err());
    assert!(is_internal_path(Path::new("dir/.vfs/file")));
  }
}
