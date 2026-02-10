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
#[allow(clippy::expect_used)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_empty_filter() {
    let filter = GitignoreFilter::new(Path::new("/nonexistent"));
    // Without .gitignore nothing is ignored
    assert!(!filter.is_ignored(&PathBuf::from("/nonexistent/file.txt")));
  }

  #[test]
  fn test_gitignore_pattern() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "*.log\n").expect("write gitignore");

    let filter = GitignoreFilter::new(path);
    assert!(
      filter.is_ignored(&path.join("test.log")),
      "*.log should ignore test.log"
    );
  }

  #[test]
  fn test_gitignore_allows_unmatched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "*.log\n").expect("write gitignore");

    let filter = GitignoreFilter::new(path);
    assert!(
      !filter.is_ignored(&path.join("file.txt")),
      "file.txt should not be ignored"
    );
  }

  #[test]
  fn test_gitignore_dir_pattern() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "build/\n").expect("write gitignore");

    // Create build/ directory so is_dir() works
    std::fs::create_dir(path.join("build")).expect("mkdir build");
    std::fs::write(path.join("build/output.js"), "data").expect("write file");

    let filter = GitignoreFilter::new(path);
    assert!(
      filter.is_ignored(&path.join("build")),
      "build/ should be ignored"
    );
  }

  /// macOS system files in gitignore.
  #[test]
  fn test_macos_dotfiles_ignored() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), ".DS_Store\n._*\n").expect("write");

    let filter = GitignoreFilter::new(path);
    assert!(filter.is_ignored(&path.join(".DS_Store")), ".DS_Store should be ignored");
    assert!(filter.is_ignored(&path.join("._test.txt")), "._test.txt should be ignored");
    assert!(!filter.is_ignored(&path.join("normal.txt")), "normal.txt should not be ignored");
  }

  /// Editor temporary files in gitignore.
  #[test]
  fn test_editor_temp_files_ignored() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "*.swp\n*~\n").expect("write");

    let filter = GitignoreFilter::new(path);
    assert!(filter.is_ignored(&path.join(".file.swp")), ".swp should be ignored");
    assert!(filter.is_ignored(&path.join("file.txt~")), "backup~ should be ignored");
    assert!(!filter.is_ignored(&path.join("file.txt")), "file.txt should not be ignored");
  }

  /// Multiple patterns in gitignore.
  #[test]
  fn test_multiple_gitignore_patterns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "*.log\n*.tmp\ntarget/\n").expect("write");

    // Create target/ so is_dir() works
    std::fs::create_dir(path.join("target")).expect("mkdir");

    let filter = GitignoreFilter::new(path);
    assert!(filter.is_ignored(&path.join("debug.log")), "debug.log should be ignored");
    assert!(filter.is_ignored(&path.join("temp.tmp")), "temp.tmp should be ignored");
    assert!(filter.is_ignored(&path.join("target")), "target/ should be ignored");
    assert!(!filter.is_ignored(&path.join("src/main.rs")), "main.rs should not be ignored");
  }

  /// .git (directory) — hidden (filtered), .gitignore — NOT hidden.
  /// Verify that the .git/ pattern filters the directory itself,
  /// but .gitignore (a file not matching the pattern) is not filtered.
  #[test]
  fn test_is_hidden_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), ".git/\n").expect("write gitignore");

    // Create .git/ so is_dir() works
    std::fs::create_dir(path.join(".git")).expect("mkdir .git");

    let filter = GitignoreFilter::new(path);
    assert!(
      filter.is_ignored(&path.join(".git")),
      ".git directory should be hidden (filtered)"
    );
    assert!(
      !filter.is_ignored(&path.join(".gitignore")),
      ".gitignore should NOT be hidden (it is not .git/)"
    );
    assert!(
      !filter.is_ignored(&path.join(".gitkeep")),
      ".gitkeep should NOT be hidden (it is not .git/)"
    );
  }

  /// Combined gitignore: *.log + *.tmp + .DS_Store.
  /// More thorough test with multiple patterns and verification of non-ignored files.
  #[test]
  fn test_gitignore_combined_patterns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path();
    std::fs::write(path.join(".gitignore"), "*.log\n*.tmp\n.DS_Store\n").expect("write");

    let filter = GitignoreFilter::new(path);

    // Should be ignored
    assert!(
      filter.is_ignored(&path.join("test.log")),
      "test.log should be ignored (pattern *.log)"
    );
    assert!(
      filter.is_ignored(&path.join("test.tmp")),
      "test.tmp should be ignored (pattern *.tmp)"
    );
    assert!(
      filter.is_ignored(&path.join(".DS_Store")),
      ".DS_Store should be ignored (exact match)"
    );
    assert!(
      filter.is_ignored(&path.join("subdir/deep.log")),
      "subdir/deep.log should be ignored (*.log recursive)"
    );
    assert!(
      filter.is_ignored(&path.join("cache/session.tmp")),
      "cache/session.tmp should be ignored (*.tmp recursive)"
    );

    // Should NOT be ignored
    assert!(
      !filter.is_ignored(&path.join("README.md")),
      "README.md should NOT be ignored"
    );
    assert!(
      !filter.is_ignored(&path.join("src/main.rs")),
      "src/main.rs should NOT be ignored"
    );
    assert!(
      !filter.is_ignored(&path.join("test.txt")),
      "test.txt should NOT be ignored"
    );
    assert!(
      !filter.is_ignored(&path.join("logger.rs")),
      "logger.rs should NOT be ignored (contains 'log' but not as extension)"
    );
  }
}
