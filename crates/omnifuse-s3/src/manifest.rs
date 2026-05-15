//! Local manifest and base content store for S3 objects.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf}
};

use serde::{Deserialize, Serialize};

use crate::{S3Error, path::ObjectPath};

/// Remote object metadata recorded for a local file.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectState {
  /// OpenDAL object path under the operator root.
  pub object_path: String,
  /// Remote ETag when provided by the service.
  pub etag: Option<String>,
  /// Remote version when provided by the service.
  pub version: Option<String>,
  /// Last modified timestamp string when provided by the service.
  pub last_modified: Option<String>,
  /// Content length in bytes.
  pub content_length: u64
}

impl ObjectState {
  /// ETag required for conditional updates.
  ///
  /// # Errors
  ///
  /// Returns an error when the provider did not return a usable ETag.
  pub fn required_etag(&self) -> Result<&str, S3Error> {
    self
      .etag
      .as_deref()
      .filter(|etag| !etag.trim().is_empty())
      .ok_or(S3Error::MissingCapability("remote object ETag"))
  }

  /// Whether two states represent the same remote generation.
  #[must_use]
  pub fn same_generation(&self, other: &Self) -> bool {
    if let (Some(left), Some(right)) = (self.version.as_deref(), other.version.as_deref()) {
      if left != right {
        return false;
      }
    }

    match (self.etag.as_deref(), other.etag.as_deref()) {
      (Some(left), Some(right)) => left == right,
      _ => false
    }
  }
}

/// S3 manifest keyed by object path.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct S3Manifest {
  /// Manifest schema version.
  pub version: u32,
  /// Entries keyed by object path.
  pub entries: BTreeMap<String, ObjectState>
}

impl S3Manifest {
  /// Create an empty manifest.
  #[must_use]
  pub const fn empty() -> Self {
    Self {
      version: 1,
      entries: BTreeMap::new()
    }
  }
}

/// Manifest and base content persistence under `.vfs/s3`.
#[derive(Debug, Clone)]
pub struct ManifestStore {
  manifest_path: PathBuf,
  base_dir: PathBuf
}

impl ManifestStore {
  /// Create store directories.
  ///
  /// # Errors
  ///
  /// Returns an IO error if metadata directories cannot be created.
  pub fn new(local_dir: &Path) -> anyhow::Result<Self> {
    let dir = local_dir.join(".vfs").join("s3");
    let base_dir = dir.join("base");
    std::fs::create_dir_all(&base_dir)?;
    Ok(Self {
      manifest_path: dir.join("manifest.json"),
      base_dir
    })
  }

  /// Load manifest or return an empty one.
  ///
  /// # Errors
  ///
  /// Returns an error if the manifest file is present but cannot be read or parsed.
  pub fn load(&self) -> anyhow::Result<S3Manifest> {
    if !self.manifest_path.exists() {
      return Ok(S3Manifest::empty());
    }
    let data = std::fs::read_to_string(&self.manifest_path)?;
    Ok(serde_json::from_str(&data)?)
  }

  /// Save manifest atomically via a temp file rename.
  ///
  /// # Errors
  ///
  /// Returns an error if the manifest cannot be serialized or written.
  pub fn save(&self, manifest: &S3Manifest) -> anyhow::Result<()> {
    let tmp = self.manifest_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(manifest)?)?;
    std::fs::rename(tmp, &self.manifest_path)?;
    Ok(())
  }

  /// Save base bytes for an object path.
  ///
  /// # Errors
  ///
  /// Returns an error if the object path is unsafe or the file cannot be written.
  pub fn save_base(&self, object_path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = self.base_path(object_path)?;
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
  }

  /// Load base bytes for an object path.
  #[must_use]
  pub fn load_base(&self, object_path: &str) -> Option<Vec<u8>> {
    self
      .base_path(object_path)
      .ok()
      .and_then(|path| std::fs::read(path).ok())
  }

  /// Remove base bytes for an object path.
  pub fn remove_base(&self, object_path: &str) {
    if let Ok(path) = self.base_path(object_path) {
      let _ = std::fs::remove_file(path);
    }
  }

  fn base_path(&self, object_path: &str) -> Result<PathBuf, S3Error> {
    Ok(
      self
        .base_dir
        .join(ObjectPath::from_remote(object_path)?.to_local_path())
    )
  }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn missing_manifest_loads_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ManifestStore::new(dir.path()).expect("store");

    let manifest = store.load().expect("load");

    assert_eq!(manifest, S3Manifest::empty());
  }

  #[test]
  fn manifest_roundtrips_object_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ManifestStore::new(dir.path()).expect("store");
    let mut manifest = S3Manifest::empty();
    manifest.entries.insert(
      "docs/readme.md".to_string(),
      ObjectState {
        object_path: "docs/readme.md".to_string(),
        etag: Some("etag-1".to_string()),
        version: Some("v1".to_string()),
        last_modified: Some("2026-05-15T00:00:00Z".to_string()),
        content_length: 5
      }
    );

    store.save(&manifest).expect("save");

    assert_eq!(store.load().expect("load"), manifest);
  }

  #[test]
  fn base_bytes_roundtrip_and_remove() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ManifestStore::new(dir.path()).expect("store");

    store.save_base("docs/readme.md", b"hello").expect("save base");
    assert_eq!(store.load_base("docs/readme.md").as_deref(), Some(&b"hello"[..]));

    store.remove_base("docs/readme.md");
    assert!(store.load_base("docs/readme.md").is_none());
  }

  #[test]
  fn base_store_rejects_internal_remote_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ManifestStore::new(dir.path()).expect("store");

    assert!(store.save_base(".vfs/s3/manifest.json", b"bad").is_err());
  }

  #[test]
  fn same_generation_requires_matching_etag() {
    let a = ObjectState {
      object_path: "x".into(),
      etag: Some("e1".into()),
      version: None,
      last_modified: None,
      content_length: 0
    };
    let mut b = a.clone();
    assert!(a.same_generation(&b));
    b.etag = Some("e2".into());
    assert!(!a.same_generation(&b));
    b.etag = None;
    assert!(!a.same_generation(&b));
  }
}
