//! Git tracking rules.

use std::path::{Path, PathBuf};

use crate::filter::GitignoreFilter;

/// Rules deciding whether a path should be tracked by the Git backend.
#[derive(Debug)]
pub struct GitTrackingRules {
  repo_path: PathBuf,
  filter: GitignoreFilter
}

impl GitTrackingRules {
  /// Create tracking rules for a repository.
  #[must_use]
  pub fn new(repo_path: &Path) -> Self {
    Self {
      repo_path: repo_path.to_path_buf(),
      filter: GitignoreFilter::new(repo_path)
    }
  }

  /// Return whether a path should be tracked.
  #[must_use]
  pub fn accepts(&self, path: &Path) -> bool {
    if Self::contains_git_directory(path) {
      return false;
    }

    let path = if path.is_absolute() {
      path.to_path_buf()
    } else {
      self.repo_path.join(path)
    };

    !self.filter.is_ignored(&path)
  }

  pub(crate) fn contains_git_directory(path: &Path) -> bool {
    path.components().any(|component| component.as_os_str() == ".git")
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::path::Path;

  use super::GitTrackingRules;

  #[test]
  fn tracking_rejects_git_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rules = GitTrackingRules::new(tmp.path());

    assert!(!rules.accepts(Path::new(".git/config")));
    assert!(!rules.accepts(&tmp.path().join(".git/config")));
  }

  #[test]
  fn tracking_uses_gitignore_after_init() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(".gitignore"), "*.log\nbuild/\n").expect("gitignore");
    let rules = GitTrackingRules::new(tmp.path());

    assert!(!rules.accepts(&tmp.path().join("debug.log")));
    assert!(!rules.accepts(&tmp.path().join("build/output.bin")));
    assert!(rules.accepts(&tmp.path().join("src/lib.rs")));
  }
}
