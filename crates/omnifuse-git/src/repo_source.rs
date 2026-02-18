//! Repository source management (local path / remote URL).
//!
//! Ported from `SimpleGitFS` `core/src/git/repo_source.rs`.

use std::{
  hash::{Hash, Hasher},
  path::{Path, PathBuf}
};

use tracing::{debug, info};

/// Git URL prefixes for remote repositories.
const GIT_URL_PREFIXES: &[&str] = &["https://", "http://", "git://", "ssh://", "git@"];

/// Repository source — local path or remote URL.
#[derive(Debug, Clone)]
pub enum RepoSource {
  /// Local filesystem.
  Local(PathBuf),
  /// Remote git URL.
  Remote {
    /// Original URL.
    url: String,
    /// Local path for the clone cache.
    cache_path: PathBuf
  }
}

impl RepoSource {
  /// Parse a source from a string.
  ///
  /// Automatically determines URL vs local path.
  #[must_use]
  pub fn parse(input: &str) -> Self {
    if Self::is_git_url(input) {
      let url = input.to_string();
      let cache_path = Self::compute_cache_path(&url);
      Self::Remote { url, cache_path }
    } else {
      Self::Local(PathBuf::from(input))
    }
  }

  /// Check whether a string looks like a git URL.
  #[must_use]
  pub fn is_git_url(input: &str) -> bool {
    for prefix in GIT_URL_PREFIXES {
      if input.starts_with(prefix) {
        return true;
      }
    }

    // scp-like: user@host:path
    if input.contains('@')
      && input.contains(':')
      && !input.contains("://")
      && let Some(pos) = input.find(':')
      && pos > 1
    {
      return true;
    }

    false
  }

  /// Compute the cache path for a remote URL.
  #[must_use]
  pub fn compute_cache_path(url: &str) -> PathBuf {
    let hash = Self::hash_url(url);
    let name = Self::extract_repo_name(url);
    let cache_base = dirs_cache_dir().join("omnifuse");
    cache_base.join(format!("{name}-{hash}"))
  }

  /// Extract the repository name from a URL.
  #[must_use]
  fn extract_repo_name(url: &str) -> String {
    let url = url.trim_end_matches(".git");
    let name = url
      .rsplit('/')
      .next()
      .or_else(|| url.rsplit(':').next())
      .unwrap_or("repo");

    name
      .chars()
      .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
      .take(32)
      .collect()
  }

  /// Hash a URL for a unique identifier.
  fn hash_url(url: &str) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
  }

  /// Local working path.
  #[must_use]
  pub fn local_path(&self) -> &Path {
    match self {
      Self::Local(path) => path,
      Self::Remote { cache_path, .. } => cache_path
    }
  }

  /// Is this a remote source?
  #[must_use]
  pub const fn is_remote(&self) -> bool {
    matches!(self, Self::Remote { .. })
  }

  /// Remote URL (if available).
  #[must_use]
  pub fn remote_url(&self) -> Option<&str> {
    match self {
      Self::Remote { url, .. } => Some(url),
      Self::Local(_) => None
    }
  }

  /// Does the local path exist?
  #[must_use]
  pub fn exists(&self) -> bool {
    self.local_path().exists()
  }

  /// Is it a valid git repository?
  #[must_use]
  pub fn is_git_repo(&self) -> bool {
    self.local_path().join(".git").exists()
  }

  /// Ensure the repository is available locally.
  ///
  /// For local — checks existence.
  /// For remote — clones if needed.
  ///
  /// # Errors
  ///
  /// Returns an error if clone fails or the local path is missing.
  pub async fn ensure_available(&self, branch: &str) -> anyhow::Result<PathBuf> {
    match self {
      Self::Local(path) => {
        if !path.exists() {
          anyhow::bail!("path not found: {}", path.display());
        }
        if !path.join(".git").exists() {
          anyhow::bail!("not a git repository: {}", path.display());
        }
        Ok(path.clone())
      }
      Self::Remote { url, cache_path } => {
        if cache_path.join(".git").exists() {
          info!(url, path = %cache_path.display(), "using cached repository");
          Self::fetch_updates(cache_path).await?;
        } else {
          info!(url, path = %cache_path.display(), "cloning");
          Self::clone_repo(url, cache_path, branch).await?;
        }
        Ok(cache_path.clone())
      }
    }
  }

  /// Clone a remote repository.
  async fn clone_repo(url: &str, target: &Path, branch: &str) -> anyhow::Result<()> {
    if let Some(parent) = target.parent() {
      std::fs::create_dir_all(parent)?;
    }

    debug!(url, target = %target.display(), branch, "cloning");

    let output = tokio::process::Command::new("git")
      .args(["clone", "--branch", branch, "--single-branch", "--depth", "1", url])
      .arg(target)
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);

      if stderr.contains("not found") || stderr.contains("Could not find remote branch") {
        debug!("branch {branch} not found, trying default");

        let output = tokio::process::Command::new("git")
          .args(["clone", "--single-branch", "--depth", "1", url])
          .arg(target)
          .output()
          .await?;

        if !output.status.success() {
          let stderr = String::from_utf8_lossy(&output.stderr);
          anyhow::bail!("git clone failed: {stderr}");
        }
      } else {
        anyhow::bail!("git clone failed: {stderr}");
      }
    }

    // Unshallow for full history
    let _ = tokio::process::Command::new("git")
      .args(["fetch", "--unshallow"])
      .current_dir(target)
      .output()
      .await;

    info!(url, "clone completed");
    Ok(())
  }

  /// Fetch updates for an existing repository.
  async fn fetch_updates(repo_path: &Path) -> anyhow::Result<()> {
    debug!(path = %repo_path.display(), "fetching updates");

    let output = tokio::process::Command::new("git")
      .args(["fetch", "--all"])
      .current_dir(repo_path)
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      debug!("fetch failed (continuing): {stderr}");
    }

    Ok(())
  }
}

impl std::fmt::Display for RepoSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Local(path) => write!(f, "{}", path.display()),
      Self::Remote { url, .. } => write!(f, "{url}")
    }
  }
}

/// Get the cache directory (`XDG_CACHE_HOME` or fallback).
fn dirs_cache_dir() -> PathBuf {
  if let Ok(cache) = std::env::var("XDG_CACHE_HOME") {
    return PathBuf::from(cache);
  }

  #[cfg(unix)]
  {
    if let Ok(home) = std::env::var("HOME") {
      return PathBuf::from(home).join(".cache");
    }
  }

  #[cfg(windows)]
  {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
      return PathBuf::from(local_app_data);
    }
  }

  PathBuf::from("/tmp")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[test]
  fn test_is_git_url() {
    assert!(RepoSource::is_git_url("https://github.com/user/repo.git"));
    assert!(RepoSource::is_git_url("https://github.com/user/repo"));
    assert!(RepoSource::is_git_url("git@github.com:user/repo.git"));
    assert!(RepoSource::is_git_url("ssh://git@github.com/user/repo.git"));
    assert!(RepoSource::is_git_url("git://github.com/user/repo.git"));

    assert!(!RepoSource::is_git_url("/path/to/repo"));
    assert!(!RepoSource::is_git_url("./relative/path"));
    assert!(!RepoSource::is_git_url("C:\\Windows\\path"));
  }

  #[test]
  fn test_parse_local() {
    let source = RepoSource::parse("/path/to/repo");
    assert!(matches!(source, RepoSource::Local(_)));
    assert!(!source.is_remote());
  }

  #[test]
  fn test_parse_remote() {
    let source = RepoSource::parse("https://github.com/user/repo.git");
    assert!(source.is_remote());
    assert_eq!(source.remote_url(), Some("https://github.com/user/repo.git"));
  }

  #[test]
  fn test_extract_repo_name() {
    assert_eq!(
      RepoSource::extract_repo_name("https://github.com/user/myrepo.git"),
      "myrepo"
    );
    assert_eq!(
      RepoSource::extract_repo_name("git@github.com:user/another-repo.git"),
      "another-repo"
    );
  }

  /// parse() preserves the URL unchanged (no normalization applied).
  #[test]
  fn test_parse_preserves_url() {
    let url = "https://github.com/user/repo.git";
    let source = RepoSource::parse(url);
    assert!(source.is_remote(), "URL should be recognized as remote");
    assert_eq!(
      source.remote_url(),
      Some(url),
      "parse() should preserve URL unchanged (with .git suffix)"
    );
  }

  /// parse() for ssh:// URL — original URL is returned unchanged.
  #[test]
  fn test_parse_keeps_original_url() {
    let url = "ssh://git@host/repo";
    let source = RepoSource::parse(url);
    assert!(source.is_remote(), "ssh:// URL should be remote");
    assert_eq!(
      source.remote_url(),
      Some(url),
      "remote_url() should return the original URL without modifications"
    );
    // Additionally: Display also shows the original URL
    assert_eq!(source.to_string(), url, "Display should show the original URL");
  }
}
