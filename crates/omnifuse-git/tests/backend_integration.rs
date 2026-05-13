//! Integration tests for GitBackend.

// Integration fixtures keep helper functions ordinary and docs descriptive.
#![allow(clippy::doc_markdown, clippy::expect_used, clippy::missing_const_for_fn)]

use std::path::{Path, PathBuf};

use omnifuse_core::{Backend, PathProtection, RemoteApplyMode, RemoteRefresh, RemoteRefreshResult};
use omnifuse_git::{GitBackend, GitConfig};

/// Timeout for async tests (30s — git operations can be slow).
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Create a bare repo + clone for backend tests.
async fn create_bare_and_clone() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let base = tmp.path().to_path_buf();

  let bare_path = base.join("bare.git");
  let clone_path = base.join("clone");

  // Create bare repo
  tokio::process::Command::new("git")
    .args(["init", "--bare", "-b", "main"])
    .arg(&bare_path)
    .output()
    .await
    .expect("git init --bare");

  // Clone
  tokio::process::Command::new("git")
    .args(["clone"])
    .arg(&bare_path)
    .arg(&clone_path)
    .output()
    .await
    .expect("clone");

  // Configure
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["config", "user.email", "test@test.com"])
    .output()
    .await
    .expect("config");
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["config", "user.name", "Test"])
    .output()
    .await
    .expect("config");

  // Initial commit
  tokio::fs::write(clone_path.join("README.md"), "# Test")
    .await
    .expect("write");
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["add", "."])
    .output()
    .await
    .expect("add");
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["commit", "-m", "initial"])
    .output()
    .await
    .expect("commit");
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["push", "-u", "origin", "main"])
    .output()
    .await
    .expect("push");

  (tmp, bare_path, clone_path)
}

struct StaticPathProtection {
  paths: Vec<PathBuf>
}

impl StaticPathProtection {
  fn new(paths: Vec<PathBuf>) -> Self {
    Self { paths }
  }
}

impl PathProtection for StaticPathProtection {
  fn is_protected(&self, path: &Path) -> bool {
    self
      .paths
      .iter()
      .any(|protected| path == protected || path.starts_with(protected))
  }
}

async fn create_clone(bare_path: &Path, clone_path: &Path) {
  tokio::process::Command::new("git")
    .args(["clone"])
    .arg(bare_path)
    .arg(clone_path)
    .output()
    .await
    .expect("clone");

  tokio::process::Command::new("git")
    .current_dir(clone_path)
    .args(["config", "user.email", "other@test.com"])
    .output()
    .await
    .expect("config email");
  tokio::process::Command::new("git")
    .current_dir(clone_path)
    .args(["config", "user.name", "Other"])
    .output()
    .await
    .expect("config name");
}

async fn create_remote_commit(repo_path: &Path, file: &str, content: &str) {
  tokio::fs::write(repo_path.join(file), content).await.expect("write");
  tokio::process::Command::new("git")
    .current_dir(repo_path)
    .args(["add", file])
    .output()
    .await
    .expect("add");
  tokio::process::Command::new("git")
    .current_dir(repo_path)
    .args(["commit", "-m", "remote change"])
    .output()
    .await
    .expect("commit");
  tokio::process::Command::new("git")
    .current_dir(repo_path)
    .args(["push"])
    .output()
    .await
    .expect("push");
}

#[tokio::test]
async fn test_should_track_hides_git() {
  eprintln!("[TEST] test_should_track_hides_git");
  let config = GitConfig {
    source: "/nonexistent".to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);

  // Before init, filter is not initialized, but .git is still hidden
  assert!(!backend.should_track(Path::new(".git")), ".git should be hidden");
  assert!(
    !backend.should_track(Path::new(".git/config")),
    ".git/config should be hidden"
  );
  assert!(
    !backend.should_track(Path::new("subdir/.git/HEAD")),
    "nested .git should be hidden"
  );
}

#[tokio::test]
async fn test_should_track_normal() {
  eprintln!("[TEST] test_should_track_normal");
  let config = GitConfig {
    source: "/nonexistent".to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);

  // Before init(), filter is not initialized — regular files should be visible
  assert!(
    backend.should_track(Path::new("README.md")),
    "README.md should be visible"
  );
  assert!(
    backend.should_track(Path::new("src/main.rs")),
    "src/main.rs should be visible"
  );
}

#[tokio::test]
async fn test_should_track_gitignore() {
  eprintln!("[TEST] test_should_track_gitignore");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  // Create .gitignore
  tokio::fs::write(clone_path.join(".gitignore"), "*.log\ntarget/\n")
    .await
    .expect("write gitignore");

  let config = GitConfig {
    source: clone_path.display().to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);

  // init() initializes the filter
  let local_dir = clone_path.join(".vfs");
  tokio::fs::create_dir_all(&local_dir).await.expect("mkdir");
  let _result = backend.init(&local_dir).await.expect("init");

  // After init, filter should work
  assert!(
    !backend.should_track(&clone_path.join("test.log")),
    "*.log should be ignored after init"
  );
  assert!(
    backend.should_track(&clone_path.join("src/main.rs")),
    "main.rs should not be ignored"
  );
}

#[tokio::test]
async fn test_init_local_repo() {
  eprintln!("[TEST] test_init_local_repo");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  let config = GitConfig {
    source: clone_path.display().to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);

  let local_dir = clone_path.join(".vfs");
  tokio::fs::create_dir_all(&local_dir).await.expect("mkdir");

  let result = backend.init(&local_dir).await.expect("init");
  assert!(
    matches!(
      result,
      omnifuse_core::InitResult::UpToDate | omnifuse_core::InitResult::Updated
    ),
    "init local repo: {result:?}"
  );
}

#[tokio::test]
async fn remote_git_init_uses_configured_local_dir() {
  eprintln!("[TEST] remote_git_init_uses_configured_local_dir");
  let (_tmp, bare_path, _clone_path) = create_bare_and_clone().await;
  let work = tempfile::tempdir().expect("work dir");
  let configured = work.path().join("configured");

  let backend = GitBackend::new(GitConfig {
    source: format!("file://{}", bare_path.display()),
    branch: "main".to_string(),
    max_push_retries: 1,
    poll_interval_secs: 30,
    local_dir: configured.clone()
  });

  backend.init(&configured).await.expect("init");

  assert!(configured.join(".git").exists());
}

#[tokio::test]
async fn test_sync_commits_pushes() {
  eprintln!("[TEST] test_sync_commits_pushes");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

    let config = GitConfig {
      source: clone_path.display().to_string(),
      ..GitConfig::default()
    };
    let backend = GitBackend::new(config);

    let local_dir = clone_path.join(".vfs");
    tokio::fs::create_dir_all(&local_dir).await.expect("mkdir");
    backend.init(&local_dir).await.expect("init");

    // Create a file and sync
    let new_file = clone_path.join("synced.txt");
    tokio::fs::write(&new_file, "sync data").await.expect("write");

    let result = backend.sync(&[new_file]).await.expect("sync");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { .. }),
      "sync should be Success: {result:?}"
    );

    // Verify the commit was created
    let output = tokio::process::Command::new("git")
      .current_dir(&clone_path)
      .args(["log", "-1", "--format=%s"])
      .output()
      .await
      .expect("git log");
    let message = String::from_utf8_lossy(&output.stdout);
    assert!(message.contains("[auto]"), "commit should contain [auto]: {message}");
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Creating a symlink in a repository: git tracks it.
#[cfg(unix)]
#[tokio::test]
async fn test_symlink_in_repo() {
  eprintln!("[TEST] test_symlink_in_repo");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  // Create the target file
  tokio::fs::write(clone_path.join("target.txt"), "symlink target")
    .await
    .expect("write target");

  // Create a symlink
  tokio::fs::symlink(clone_path.join("target.txt"), clone_path.join("link.txt"))
    .await
    .expect("create symlink");

  // git add the symlink
  tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["add", "link.txt", "target.txt"])
    .output()
    .await
    .expect("git add");

  // Verify git tracks the symlink
  let output = tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["status", "--porcelain"])
    .output()
    .await
    .expect("git status");
  let status = String::from_utf8_lossy(&output.stdout);
  assert!(status.contains("link.txt"), "symlink should be in git status: {status}");
}

/// Creating a file in repo — git status --porcelain shows untracked.
#[tokio::test]
async fn test_git_status_after_write() {
  eprintln!("[TEST] test_git_status_after_write");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  // Create a new file (not added to git)
  tokio::fs::write(clone_path.join("untracked.txt"), "new file")
    .await
    .expect("write");

  // Verify git status --porcelain
  let output = tokio::process::Command::new("git")
    .current_dir(&clone_path)
    .args(["status", "--porcelain"])
    .output()
    .await
    .expect("git status");
  let status = String::from_utf8_lossy(&output.stdout);
  assert!(
    status.contains("?? untracked.txt"),
    "untracked file should have '??' in git status: {status}"
  );
}

/// .gitignore with *.log pattern: should_track("debug.log") = false,
/// should_track("readme.md") = true.
#[tokio::test]
async fn test_should_track_gitignore_patterns() {
  eprintln!("[TEST] test_should_track_gitignore_patterns");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  // Create .gitignore with *.log pattern
  tokio::fs::write(clone_path.join(".gitignore"), "*.log\n")
    .await
    .expect("write gitignore");

  let config = GitConfig {
    source: clone_path.display().to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);

  // init() initializes the filter
  let local_dir = clone_path.join(".vfs");
  tokio::fs::create_dir_all(&local_dir).await.expect("mkdir");
  let _result = backend.init(&local_dir).await.expect("init");

  // *.log should be ignored
  assert!(
    !backend.should_track(&clone_path.join("debug.log")),
    "debug.log should be ignored (pattern *.log)"
  );

  // readme.md does not match *.log
  assert!(
    backend.should_track(&clone_path.join("readme.md")),
    "readme.md should not be ignored"
  );
}

/// refresh_remote after a remote commit: applies safe changes.
#[tokio::test]
async fn test_refresh_remote_applies_remote_commit() {
  eprintln!("[TEST] test_refresh_remote_applies_remote_commit");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (_tmp, bare_path, clone_path) = create_bare_and_clone().await;
    let other_clone = bare_path.parent().expect("parent").join("other_clone");
    create_clone(&bare_path, &other_clone).await;

    let config = GitConfig {
      source: clone_path.display().to_string(),
      local_dir: clone_path.clone(),
      ..GitConfig::default()
    };
    let backend = GitBackend::new(config);
    backend.init(&clone_path).await.expect("init");

    create_remote_commit(&other_clone, "remote_change.md", "# Remote change").await;

    let protected = StaticPathProtection::new(Vec::new());
    let result = backend
      .refresh_remote(RemoteRefresh {
        protected_paths: &protected,
        mode: RemoteApplyMode::ApplySafe
      })
      .await
      .expect("refresh");

    assert!(matches!(result, RemoteRefreshResult::Applied { .. }));
    assert_eq!(
      tokio::fs::read_to_string(clone_path.join("remote_change.md"))
        .await
        .expect("read"),
      "# Remote change"
    );
  })
  .await
  .expect("test timed out");
}

#[tokio::test]
async fn git_refresh_defers_when_remote_change_hits_protected_path() {
  eprintln!("[TEST] git_refresh_defers_when_remote_change_hits_protected_path");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (_tmp, bare_path, clone_path) = create_bare_and_clone().await;
    let other_clone = bare_path.parent().expect("parent").join("other_clone");
    create_clone(&bare_path, &other_clone).await;

    let backend = GitBackend::new(GitConfig {
      source: clone_path.display().to_string(),
      local_dir: clone_path.clone(),
      ..GitConfig::default()
    });
    backend.init(&clone_path).await.expect("init");

    create_remote_commit(&other_clone, "shared.txt", "remote").await;

    let protected = StaticPathProtection::new(vec![clone_path.join("shared.txt")]);
    let result = backend
      .refresh_remote(RemoteRefresh {
        protected_paths: &protected,
        mode: RemoteApplyMode::ApplySafe
      })
      .await
      .expect("refresh");

    assert!(matches!(result, RemoteRefreshResult::Deferred { .. }));
    assert!(
      !clone_path.join("shared.txt").exists(),
      "protected remote file should not be applied"
    );
  })
  .await
  .expect("test timed out");
}

/// is_online() returns true for a reachable local repo.
#[tokio::test]
async fn test_is_online_local_repo() {
  eprintln!("[TEST] test_is_online_local_repo");
  let (_tmp, _bare, clone_path) = create_bare_and_clone().await;

  let config = GitConfig {
    source: clone_path.display().to_string(),
    ..GitConfig::default()
  };
  let backend = GitBackend::new(config);
  let local_dir = clone_path.join(".vfs");
  tokio::fs::create_dir_all(&local_dir).await.expect("mkdir");
  backend.init(&local_dir).await.expect("init");

  // Local bare repo is always "online"
  let online = backend.is_online().await;
  assert!(online, "is_online() should return true for local repo",);
}
