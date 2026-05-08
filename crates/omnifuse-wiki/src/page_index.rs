//! Bidirectional wiki page index.

use std::{collections::HashMap, path::Path};

use crate::page::{PageRef, PageSnapshot};

/// Indexed wiki page entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
  /// Page identity and local path.
  pub ref_: PageRef,
  /// Last known remote snapshot.
  pub snapshot: PageSnapshot
}

/// Bidirectional index by slug and local path.
#[derive(Debug, Default, Clone)]
pub struct PageIndex {
  by_slug: HashMap<String, PageEntry>,
  slug_by_path: HashMap<std::path::PathBuf, String>
}

impl PageIndex {
  /// Insert or replace a page entry.
  pub fn insert(&mut self, ref_: PageRef, snapshot: PageSnapshot) {
    if let Some(previous) = self.by_slug.get(ref_.slug.as_str()) {
      self.slug_by_path.remove(&previous.ref_.path);
    }

    self
      .slug_by_path
      .insert(ref_.path.clone(), ref_.slug.as_str().to_string());
    self
      .by_slug
      .insert(ref_.slug.as_str().to_string(), PageEntry { ref_, snapshot });
  }

  /// Remove an entry by absolute local path.
  pub fn remove_path(&mut self, path: &Path) {
    let Some(slug) = self.slug_by_path.remove(path) else {
      return;
    };

    self.by_slug.remove(&slug);
  }

  /// Resolve an entry by remote slug.
  #[must_use]
  pub fn by_slug(&self, slug: &str) -> Option<&PageEntry> {
    self.by_slug.get(slug)
  }

  /// Resolve an entry by absolute local path.
  #[must_use]
  pub fn by_path(&self, path: &Path) -> Option<&PageEntry> {
    let slug = self.slug_by_path.get(path)?;
    self.by_slug.get(slug)
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use crate::{
    page::{PageRef, PageSnapshot, Slug},
    page_index::PageIndex
  };

  #[test]
  fn page_index_resolves_by_slug_and_path() {
    let local_dir = Path::new("/mnt/wiki");
    let page = PageRef::from_slug(local_dir, Slug::new("root/docs")).expect("page");
    let mut index = PageIndex::default();

    index.insert(
      page.clone(),
      PageSnapshot {
        id: 7,
        title: "Docs".to_string(),
        modified_at: "2024-01-01T00:00:00Z".to_string()
      }
    );

    assert_eq!(
      index.by_slug(page.slug.as_str()).map(|p| p.ref_.path.clone()),
      Some(page.path.clone())
    );
    assert_eq!(
      index.by_path(&page.path).map(|p| p.ref_.slug.as_str()),
      Some("root/docs")
    );
  }
}
