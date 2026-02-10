//! Метаданные страниц для конфликт-детекции.
//!
//! Хранит `.vfs/meta/{slug}.json` и `.vfs/base/{slug}.md`
//! для three-way merge.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Метаданные страницы.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMeta {
  /// ID страницы на сервере.
  pub id: u64,
  /// Заголовок.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Время последнего изменения на remote (ISO 8601).
  pub modified_at: String
}

/// Хранилище метаданных и base-контента.
#[derive(Debug)]
pub struct MetaStore {
  /// Путь к `.vfs/meta/`.
  meta_dir: PathBuf,
  /// Путь к `.vfs/base/`.
  base_dir: PathBuf
}

impl MetaStore {
  /// Создать хранилище для заданной локальной директории.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при невозможности создать директории.
  pub fn new(local_dir: &Path) -> anyhow::Result<Self> {
    let vfs_dir = local_dir.join(".vfs");
    let meta_dir = vfs_dir.join("meta");
    let base_dir = vfs_dir.join("base");

    std::fs::create_dir_all(&meta_dir)?;
    std::fs::create_dir_all(&base_dir)?;

    Ok(Self { meta_dir, base_dir })
  }

  /// Сохранить метаданные страницы.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку IO.
  pub fn save_meta(&self, slug: &str, meta: &PageMeta) -> anyhow::Result<()> {
    let path = self.meta_path(slug);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(meta)?;
    std::fs::write(&path, json)?;

    debug!(slug, path = %path.display(), "meta сохранены");
    Ok(())
  }

  /// Загрузить метаданные страницы.
  #[must_use]
  pub fn load_meta(&self, slug: &str) -> Option<PageMeta> {
    let path = self.meta_path(slug);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
  }

  /// Сохранить base-контент для merge.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку IO.
  pub fn save_base(&self, slug: &str, content: &str) -> anyhow::Result<()> {
    let path = self.base_path(slug);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&path, content)?;
    debug!(slug, path = %path.display(), "base сохранён");
    Ok(())
  }

  /// Загрузить base-контент для merge.
  #[must_use]
  pub fn load_base(&self, slug: &str) -> Option<String> {
    let path = self.base_path(slug);
    std::fs::read_to_string(&path).ok()
  }

  /// Удалить метаданные и base для страницы.
  pub fn remove(&self, slug: &str) {
    let _ = std::fs::remove_file(self.meta_path(slug));
    let _ = std::fs::remove_file(self.base_path(slug));
  }

  /// Получить все slug'и из `.vfs/meta/`.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку IO.
  pub fn all_slugs(&self) -> anyhow::Result<Vec<String>> {
    let mut slugs = Vec::new();
    Self::collect_slugs(&self.meta_dir, &self.meta_dir, &mut slugs)?;
    Ok(slugs)
  }

  /// Путь к файлу метаданных.
  fn meta_path(&self, slug: &str) -> PathBuf {
    self.meta_dir.join(slug_to_path(slug, "json"))
  }

  /// Путь к base-контенту.
  fn base_path(&self, slug: &str) -> PathBuf {
    self.base_dir.join(slug_to_path(slug, "md"))
  }

  /// Рекурсивный сбор slug'ов из директории.
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
        let slug = rel
          .with_extension("")
          .to_string_lossy()
          .replace('\\', "/");
        slugs.push(slug);
      }
    }

    Ok(())
  }
}

/// Преобразовать slug в путь файла.
///
/// `"docs/architecture"` → `"docs/architecture.{ext}"`
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

/// Преобразовать путь файла `.md` в slug.
///
/// `/mount/docs/page.md` → `"docs/page"` (относительно `local_dir`).
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
mod tests {
  use super::*;

  #[test]
  fn test_slug_to_path() {
    assert_eq!(slug_to_path("docs/architecture", "json"), PathBuf::from("docs/architecture.json"));
    assert_eq!(slug_to_path("page", "md"), PathBuf::from("page.md"));
  }

  #[test]
  fn test_path_to_slug() {
    let local_dir = Path::new("/mnt/wiki");
    assert_eq!(
      path_to_slug(Path::new("/mnt/wiki/docs/page.md"), local_dir),
      Some("docs/page".to_string())
    );
    assert_eq!(
      path_to_slug(Path::new("/mnt/wiki/.vfs/meta/x.json"), local_dir),
      None
    );
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
    store
      .save_base("docs/test", "# Test content")
      .expect("save base");

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
}
