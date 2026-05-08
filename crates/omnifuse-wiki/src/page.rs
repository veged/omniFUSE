//! Wiki page identity and local path mapping.

use std::path::{Component, Path, PathBuf};

/// Wiki page slug without the local markdown extension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slug(String);

impl Slug {
  /// Create a normalized slug value.
  #[must_use]
  pub fn new(value: impl Into<String>) -> Self {
    let normalized = value.into().replace('\\', "/");
    let value = normalized.strip_suffix(".md").unwrap_or(&normalized);

    Self(value.to_string())
  }

  /// Borrow the normalized slug.
  #[must_use]
  pub fn as_str(&self) -> &str {
    &self.0
  }

  fn is_valid_page_slug(&self) -> bool {
    !self.0.is_empty()
      && self
        .0
        .split('/')
        .all(|segment| !matches!(segment, "" | "." | ".." | ".vfs"))
  }

  fn segments(&self) -> impl Iterator<Item = &str> {
    self.0.split('/')
  }
}

/// Stable relation between a remote page slug and a local markdown file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageRef {
  /// Remote wiki slug without `.md`.
  pub slug: Slug,
  /// Absolute markdown path under the mounted local directory.
  pub path: PathBuf
}

impl PageRef {
  /// Resolve a page slug to an absolute markdown file under `local_dir`.
  #[must_use]
  pub fn from_slug(local_dir: &Path, slug: Slug) -> Option<Self> {
    if !local_dir.is_absolute() || !slug.is_valid_page_slug() {
      return None;
    }

    let mut path = local_dir.to_path_buf();
    for segment in slug.segments() {
      path.push(segment);
    }
    path.set_extension("md");

    Some(Self { slug, path })
  }

  /// Resolve an absolute markdown path under `local_dir` to a page reference.
  #[must_use]
  pub fn from_path(local_dir: &Path, path: &Path) -> Option<Self> {
    if !local_dir.is_absolute() || !path.is_absolute() || path.extension().is_none_or(|e| e != "md") {
      return None;
    }

    let rel = path.strip_prefix(local_dir).ok()?;
    if rel.components().any(is_vfs_component) {
      return None;
    }

    let slug = Slug::new(rel.with_extension("").to_string_lossy());
    if !slug.is_valid_page_slug() {
      return None;
    }

    Some(Self {
      slug,
      path: path.to_path_buf()
    })
  }
}

/// Remote page state used for change detection and metadata storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSnapshot {
  /// Page ID on the wiki server.
  pub id: u64,
  /// Current remote page title.
  pub title: String,
  /// Current remote modification timestamp.
  pub modified_at: String
}

fn is_vfs_component(component: Component<'_>) -> bool {
  component.as_os_str() == ".vfs"
}

#[cfg(test)]
mod tests {
  use std::path::{Path, PathBuf};

  use super::{PageRef, Slug};

  #[test]
  fn page_ref_maps_slug_to_markdown_path() {
    let local_dir = Path::new("/mnt/wiki");
    let page = PageRef::from_slug(local_dir, Slug::new("root/docs")).expect("page");

    assert_eq!(page.slug.as_str(), "root/docs");
    assert_eq!(page.path, PathBuf::from("/mnt/wiki/root/docs.md"));
  }

  #[test]
  fn page_ref_rejects_vfs_metadata_paths() {
    let local_dir = Path::new("/mnt/wiki");

    assert!(PageRef::from_path(local_dir, Path::new("/mnt/wiki/.vfs/meta/root.json")).is_none());
    assert!(PageRef::from_path(local_dir, Path::new("/mnt/wiki/root/docs.md")).is_some());
  }
}
