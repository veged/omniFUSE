//! File filtering via `.gitignore`.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tracing::debug;

/// Filter based on `.gitignore`.
#[derive(Debug)]
pub struct GitignoreFilter {
  /// Gitignore parser.
  gitignore: Gitignore
}

impl GitignoreFilter {
  /// Create a filter for a directory.
  ///
  /// Looks for `.gitignore` in the specified directory.
  #[must_use]
  pub fn new(repo_path: &Path) -> Self {
    let gitignore_path = repo_path.join(".gitignore");
    let mut builder = GitignoreBuilder::new(repo_path);

    if gitignore_path.exists()
      && let Some(err) = builder.add(&gitignore_path)
    {
      debug!(error = %err, "error parsing .gitignore");
    }

    let gitignore = builder.build().unwrap_or_else(|_| {
      // Empty gitignore if parsing failed
      GitignoreBuilder::new(repo_path)
        .build()
        .unwrap_or_else(|_| Gitignore::empty())
    });

    Self { gitignore }
  }

  /// Check whether a file is ignored.
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
    // Without .gitignore nothing is ignored
    assert!(!filter.is_ignored(&PathBuf::from("/nonexistent/file.txt")));
  }
}
