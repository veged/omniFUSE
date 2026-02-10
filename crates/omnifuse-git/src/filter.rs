//! Фильтрация файлов через `.gitignore`.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tracing::debug;

/// Фильтр на основе `.gitignore`.
#[derive(Debug)]
pub struct GitignoreFilter {
  /// Парсер gitignore.
  gitignore: Gitignore
}

impl GitignoreFilter {
  /// Создать фильтр для директории.
  ///
  /// Ищет `.gitignore` в указанной директории.
  #[must_use]
  pub fn new(repo_path: &Path) -> Self {
    let gitignore_path = repo_path.join(".gitignore");
    let mut builder = GitignoreBuilder::new(repo_path);

    if gitignore_path.exists()
      && let Some(err) = builder.add(&gitignore_path)
    {
      debug!(error = %err, "ошибка парсинга .gitignore");
    }

    let gitignore = builder.build().unwrap_or_else(|_| {
      // Пустой gitignore если парсинг не удался
      GitignoreBuilder::new(repo_path)
        .build()
        .unwrap_or_else(|_| Gitignore::empty())
    });

    Self { gitignore }
  }

  /// Проверить, игнорируется ли файл.
  #[must_use]
  pub fn is_ignored(&self, path: &Path) -> bool {
    let is_dir = path.is_dir();
    self
      .gitignore
      .matched(path, is_dir)
      .is_ignore()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_empty_filter() {
    let filter = GitignoreFilter::new(Path::new("/nonexistent"));
    // Без .gitignore ничего не игнорируется
    assert!(!filter.is_ignored(&PathBuf::from("/nonexistent/file.txt")));
  }
}
