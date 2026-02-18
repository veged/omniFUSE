//! Page metadata for conflict detection.
//!
//! Stores `.vfs/meta/{slug}.json` and `.vfs/base/{slug}.md`
//! for three-way merge.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Page metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMeta {
  /// Page ID on the server.
  pub id: u64,
  /// Title.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Last modification time on remote (ISO 8601).
  pub modified_at: String
}

/// Metadata and base content store.
#[derive(Debug)]
pub struct MetaStore {
  /// Path to `.vfs/meta/`.
  meta_dir: PathBuf,
  /// Path to `.vfs/base/`.
  base_dir: PathBuf
}

impl MetaStore {
  /// Create a store for the given local directory.
  ///
  /// # Errors
  ///
  /// Returns an error if the directories cannot be created.
  pub fn new(local_dir: &Path) -> anyhow::Result<Self> {
    let vfs_dir = local_dir.join(".vfs");
    let meta_dir = vfs_dir.join("meta");
    let base_dir = vfs_dir.join("base");

    std::fs::create_dir_all(&meta_dir)?;
    std::fs::create_dir_all(&base_dir)?;

    Ok(Self { meta_dir, base_dir })
  }

  /// Save page metadata.
  ///
  /// # Errors
  ///
  /// Returns an IO error.
  pub fn save_meta(&self, slug: &str, meta: &PageMeta) -> anyhow::Result<()> {
    let path = self.meta_path(slug);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(meta)?;
    std::fs::write(&path, json)?;

    debug!(slug, path = %path.display(), "meta saved");
    Ok(())
  }

  /// Load page metadata.
  #[must_use]
  pub fn load_meta(&self, slug: &str) -> Option<PageMeta> {
    let path = self.meta_path(slug);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
  }

  /// Save base content for merge.
  ///
  /// # Errors
  ///
  /// Returns an IO error.
  pub fn save_base(&self, slug: &str, content: &str) -> anyhow::Result<()> {
    let path = self.base_path(slug);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&path, content)?;
    debug!(slug, path = %path.display(), "base saved");
    Ok(())
  }

  /// Load base content for merge.
  #[must_use]
  pub fn load_base(&self, slug: &str) -> Option<String> {
    let path = self.base_path(slug);
    std::fs::read_to_string(&path).ok()
  }

  /// Remove metadata and base for a page.
  pub fn remove(&self, slug: &str) {
    let _ = std::fs::remove_file(self.meta_path(slug));
    let _ = std::fs::remove_file(self.base_path(slug));
  }

  /// Get all slugs from `.vfs/meta/`.
  ///
  /// # Errors
  ///
  /// Returns an IO error.
  pub fn all_slugs(&self) -> anyhow::Result<Vec<String>> {
    let mut slugs = Vec::new();
    Self::collect_slugs(&self.meta_dir, &self.meta_dir, &mut slugs)?;
    Ok(slugs)
  }

  /// Path to the metadata file.
  fn meta_path(&self, slug: &str) -> PathBuf {
    self.meta_dir.join(slug_to_path(slug, "json"))
  }

  /// Path to the base content.
  fn base_path(&self, slug: &str) -> PathBuf {
    self.base_dir.join(slug_to_path(slug, "md"))
  }

  /// Recursively collect slugs from a directory.
  fn collect_slugs(base: &Path, dir: &Path, slugs: &mut Vec<String>) -> anyhow::Result<()> {
    if !dir.exists() {
      return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
      let entry = entry?;
      let path = entry.path();

      if path.is_dir() {
        Self::collect_slugs(base, &path, slugs)?;
      } else if path.extension().is_some_and(|e| e == "json")
        && let Ok(rel) = path.strip_prefix(base)
      {
        let slug = rel.with_extension("").to_string_lossy().replace('\\', "/");
        slugs.push(slug);
      }
    }

    Ok(())
  }
}

/// Convert a slug to a file path.
///
/// `"docs/architecture"` -> `"docs/architecture.{ext}"`
fn slug_to_path(slug: &str, ext: &str) -> PathBuf {
  let parts: Vec<&str> = slug.split('/').collect();
  let mut path = PathBuf::new();

  for (i, part) in parts.iter().enumerate() {
    if i == parts.len() - 1 {
      path.push(format!("{part}.{ext}"));
    } else {
      path.push(part);
    }
  }

  path
}

/// Convert an `.md` file path to a slug.
///
/// `/mount/docs/page.md` -> `"docs/page"` (relative to `local_dir`).
#[must_use]
pub fn path_to_slug(path: &Path, local_dir: &Path) -> Option<String> {
  let rel = path.strip_prefix(local_dir).ok()?;
  let slug = rel.with_extension("").to_string_lossy().replace('\\', "/");

  if slug.is_empty() || slug.starts_with(".vfs") {
    return None;
  }

  Some(slug)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[test]
  fn test_slug_to_path() {
    assert_eq!(
      slug_to_path("docs/architecture", "json"),
      PathBuf::from("docs/architecture.json")
    );
    assert_eq!(slug_to_path("page", "md"), PathBuf::from("page.md"));
  }

  #[test]
  fn test_path_to_slug() {
    let local_dir = Path::new("/mnt/wiki");
    assert_eq!(
      path_to_slug(Path::new("/mnt/wiki/docs/page.md"), local_dir),
      Some("docs/page".to_string())
    );
    assert_eq!(path_to_slug(Path::new("/mnt/wiki/.vfs/meta/x.json"), local_dir), None);
  }

  #[test]
  fn test_meta_store_roundtrip() {
    let dir = std::env::temp_dir().join("omnifuse-meta-test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = MetaStore::new(&dir).expect("create store");

    let meta = PageMeta {
      id: 42,
      title: "Test".to_string(),
      slug: "docs/test".to_string(),
      modified_at: "2024-01-01T00:00:00Z".to_string()
    };

    store.save_meta("docs/test", &meta).expect("save meta");
    store.save_base("docs/test", "# Test content").expect("save base");

    let loaded = store.load_meta("docs/test");
    assert!(loaded.is_some());
    assert_eq!(loaded.as_ref().map(|m| m.id), Some(42));

    let base = store.load_base("docs/test");
    assert_eq!(base.as_deref(), Some("# Test content"));

    let slugs = store.all_slugs().expect("all slugs");
    assert!(slugs.contains(&"docs/test".to_string()));

    store.remove("docs/test");
    assert!(store.load_meta("docs/test").is_none());
    assert!(store.load_base("docs/test").is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_all_slugs_nested() {
    // Nested slugs are correctly collected from .vfs/meta/
    let dir = std::env::temp_dir().join("omnifuse-meta-nested-test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = MetaStore::new(&dir).expect("create store");

    let meta1 = PageMeta {
      id: 1,
      title: "Root".to_string(),
      slug: "root".to_string(),
      modified_at: "2024-01-01T00:00:00Z".to_string()
    };
    let meta2 = PageMeta {
      id: 2,
      title: "Docs".to_string(),
      slug: "root/docs".to_string(),
      modified_at: "2024-01-01T00:00:01Z".to_string()
    };
    let meta3 = PageMeta {
      id: 3,
      title: "Deep".to_string(),
      slug: "root/docs/deep".to_string(),
      modified_at: "2024-01-01T00:00:02Z".to_string()
    };

    store.save_meta("root", &meta1).expect("save 1");
    store.save_meta("root/docs", &meta2).expect("save 2");
    store.save_meta("root/docs/deep", &meta3).expect("save 3");

    let mut slugs = store.all_slugs().expect("all_slugs");
    slugs.sort();

    assert_eq!(slugs.len(), 3, "should have 3 slugs");
    assert!(slugs.contains(&"root".to_string()));
    assert!(slugs.contains(&"root/docs".to_string()));
    assert!(slugs.contains(&"root/docs/deep".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_remove_nonexistent_silent() {
    // Removing a nonexistent slug does not cause an error
    let dir = std::env::temp_dir().join("omnifuse-meta-remove-test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = MetaStore::new(&dir).expect("create store");

    // Should not panic
    store.remove("nonexistent/page");

    // Verify the store is empty
    let slugs = store.all_slugs().expect("all_slugs");
    assert!(slugs.is_empty(), "store should be empty");

    let _ = std::fs::remove_dir_all(&dir);
  }
}
