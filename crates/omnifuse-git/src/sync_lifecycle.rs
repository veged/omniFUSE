//! Git synchronization lifecycle.

use std::path::{Path, PathBuf};

use omnifuse_core::{RemoteApplyMode, RemoteDeferReason, RemoteRefresh, RemoteRefreshResult};
use tracing::{debug, info, warn};

use crate::{
  GitConfig,
  engine::MergeResult,
  error::{classify_git_error, is_nothing_to_commit},
  ops::{GitOps, StartupSyncResult},
  repo_source::RepoSource,
  tracking::GitTrackingRules
};

/// Git startup result.
#[derive(Debug, Clone)]
pub enum GitInit {
  /// Repository is up to date.
  UpToDate,
  /// Repository was updated from remote.
  Updated,
  /// Startup sync found conflicts.
  Conflicts {
    /// Files with conflicts.
    files: Vec<PathBuf>
  },
  /// Remote is unavailable, local state is usable.
  Offline
}

/// Git local sync result.
#[derive(Debug, Clone)]
pub enum GitSync {
  /// Local changes were synced.
  Success {
    /// Number of synced files.
    synced_files: usize
  },
  /// Sync hit conflicts.
  Conflict {
    /// Files with conflicts.
    files: Vec<PathBuf>
  },
  /// Remote is unavailable.
  Offline
}

/// Git remote refresh result.
#[derive(Debug, Clone)]
pub enum GitRefresh {
  /// Remote has no changes.
  NoChange,
  /// Remote changes were applied.
  Applied {
    /// Files changed by remote.
    files: Vec<PathBuf>,
    /// Merge result returned by git.
    merge: MergeResult
  },
  /// Refresh hit conflicts.
  Conflict {
    /// Files with conflicts.
    files: Vec<PathBuf>
  },
  /// Remote is unavailable.
  Offline
}

/// Deep git workflow facade.
#[derive(Debug)]
pub struct GitSyncLifecycle {
  repo_path: PathBuf,
  ops: GitOps,
  tracking: GitTrackingRules,
  max_push_retries: u32
}

impl GitSyncLifecycle {
  /// Open a repository and run startup synchronization.
  ///
  /// # Errors
  ///
  /// Returns an error if the repository cannot be prepared or opened.
  pub async fn open(config: GitConfig, local_dir: &Path) -> anyhow::Result<(Self, GitInit)> {
    let target_dir = if config.local_dir.as_os_str().is_empty() {
      local_dir.to_path_buf()
    } else {
      config.local_dir.clone()
    };
    let source = RepoSource::parse(&config.source);
    let repo_path = prepare_repo(&source, &config.branch, &target_dir).await?;
    let ops = GitOps::new(repo_path.clone(), config.branch)?;
    let init = map_startup_sync(ops.startup_sync().await?);
    let tracking = GitTrackingRules::new(&repo_path);

    Ok((
      Self {
        repo_path,
        ops,
        tracking,
        max_push_retries: config.max_push_retries
      },
      init
    ))
  }

  /// Return whether a path should be tracked.
  #[must_use]
  pub fn should_track(&self, path: &Path) -> bool {
    self.tracking.accepts(path)
  }

  /// Synchronize local dirty files to the Git remote.
  ///
  /// # Errors
  ///
  /// Returns an error if git commit or push fails with a non-domain error.
  pub async fn sync_local(&self, dirty_files: &[PathBuf]) -> anyhow::Result<GitSync> {
    if let Err(error) = self.ops.auto_commit(dirty_files).await {
      if !is_nothing_to_commit(&error) {
        return Err(error);
      }
      debug!("sync_local: no changes to commit");
    }

    match self.ops.push_with_retry(self.max_push_retries).await {
      Ok(()) => Ok(GitSync::Success {
        synced_files: dirty_files.len()
      }),
      Err(error) => match classify_git_error(&error) {
        Some(omnifuse_core::ErrorKind::Conflict) => {
          warn!("sync_local: conflicts during push");
          Ok(GitSync::Conflict {
            files: dirty_files.to_vec()
          })
        }
        Some(omnifuse_core::ErrorKind::Offline) => Ok(GitSync::Offline),
        _ => Err(error)
      }
    }
  }

  /// Fetch and apply remote changes.
  ///
  /// # Errors
  ///
  /// Returns an error if git fetch or pull fails with a non-domain error.
  pub async fn refresh_remote(&self) -> anyhow::Result<GitRefresh> {
    let engine = self.ops.engine();
    let local_head = engine.get_head_commit().await?;

    if let Err(error) = engine.fetch().await {
      return match classify_git_error(&error) {
        Some(omnifuse_core::ErrorKind::Offline) => Ok(GitRefresh::Offline),
        _ => Err(error)
      };
    }

    let Some(remote_head) = engine.get_remote_head().await? else {
      return Ok(GitRefresh::NoChange);
    };

    if local_head == remote_head {
      return Ok(GitRefresh::NoChange);
    }

    let files = self.diff_files_between(&local_head, &remote_head).await?;
    let merge = engine.pull().await?;

    match merge {
      MergeResult::Conflict { files } => Ok(GitRefresh::Conflict { files }),
      merge => Ok(GitRefresh::Applied { files, merge })
    }
  }

  /// Detect remote changes, respect protected paths, and apply safe refreshes.
  ///
  /// # Errors
  ///
  /// Returns an error if git inspection or pull fails with a non-domain error.
  pub async fn refresh_remote_protected(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    let changed_files = match self.changed_remote_files().await {
      Ok(files) => files,
      Err(error) => {
        return match classify_git_error(&error) {
          Some(omnifuse_core::ErrorKind::Offline) => Ok(RemoteRefreshResult::Offline),
          _ => Err(error)
        };
      }
    };

    if changed_files.is_empty() {
      return Ok(RemoteRefreshResult::Unchanged);
    }

    let protected: Vec<PathBuf> = changed_files
      .iter()
      .filter(|path| request.protected_paths.is_protected(path))
      .cloned()
      .collect();
    if !protected.is_empty() {
      return Ok(RemoteRefreshResult::Deferred {
        affected: protected,
        reason: RemoteDeferReason::ProtectedLocalChange
      });
    }

    if matches!(request.mode, RemoteApplyMode::DetectOnly) {
      return Ok(RemoteRefreshResult::Deferred {
        affected: changed_files,
        reason: RemoteDeferReason::DetectOnly
      });
    }

    match self.refresh_remote().await? {
      GitRefresh::NoChange => Ok(RemoteRefreshResult::Unchanged),
      GitRefresh::Applied { files, .. } => Ok(RemoteRefreshResult::Applied {
        changed: files,
        deleted: Vec::new()
      }),
      GitRefresh::Conflict { files } => Ok(RemoteRefreshResult::Deferred {
        affected: files,
        reason: RemoteDeferReason::Conflict
      }),
      GitRefresh::Offline => Ok(RemoteRefreshResult::Offline)
    }
  }

  /// Classify a git error for core observability.
  #[must_use]
  pub fn classify(&self, error: &anyhow::Error) -> omnifuse_core::ErrorKind {
    classify_git_error(error).unwrap_or(omnifuse_core::ErrorKind::Internal)
  }

  /// Repository path.
  #[must_use]
  pub fn repo_path(&self) -> &Path {
    &self.repo_path
  }

  pub(crate) async fn changed_remote_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    if !self.ops.check_remote().await? {
      return Ok(Vec::new());
    }

    self.diff_remote_files().await
  }

  pub(crate) async fn is_online(&self) -> bool {
    self.ops.engine().fetch().await.is_ok()
  }

  async fn diff_remote_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let engine = self.ops.engine();

    let local_head = engine.get_head_commit().await?;
    let remote_head = engine.get_remote_head().await?;

    let Some(remote_head) = remote_head else {
      return Ok(Vec::new());
    };

    if local_head == remote_head {
      return Ok(Vec::new());
    }

    self.diff_files_between(&local_head, &remote_head).await
  }

  async fn diff_files_between(&self, from: &str, to: &str) -> anyhow::Result<Vec<PathBuf>> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["diff", "--name-only", from, to])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(Vec::new());
    }

    Ok(
      String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| self.repo_path.join(line))
        .collect()
    )
  }
}

async fn prepare_repo(source: &RepoSource, branch: &str, target_dir: &Path) -> anyhow::Result<PathBuf> {
  match source {
    RepoSource::Local(path) => {
      let target_is_inside_repo = target_dir.starts_with(path) || target_dir == path;
      if target_is_inside_repo {
        return source.ensure_available(branch).await;
      }

      std::fs::create_dir_all(target_dir)?;
      if !target_dir.join(".git").exists() {
        info!(source = %path.display(), target = %target_dir.display(), "cloning local repo into cache");
        let output = tokio::process::Command::new("git")
          .args(["clone", "--branch", branch])
          .arg(path)
          .arg(target_dir)
          .output()
          .await?;

        if !output.status.success() {
          let stderr = String::from_utf8_lossy(&output.stderr);
          anyhow::bail!("git clone failed: {stderr}");
        }
      }

      Ok(target_dir.to_path_buf())
    }
    RepoSource::Remote { .. } => source.ensure_available_at(branch, target_dir).await
  }
}

fn map_startup_sync(result: StartupSyncResult) -> GitInit {
  match result {
    StartupSyncResult::UpToDate => GitInit::UpToDate,
    StartupSyncResult::Updated | StartupSyncResult::Merged => GitInit::Updated,
    StartupSyncResult::Conflicts { files } => GitInit::Conflicts { files },
    StartupSyncResult::Offline => GitInit::Offline
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::path::Path;

  use crate::{
    GitConfig,
    engine::{GitEngine, tests::create_bare_and_two_clones},
    sync_lifecycle::{GitInit, GitSync, GitSyncLifecycle}
  };

  #[tokio::test]
  async fn open_local_repo_runs_startup_sync_and_tracking() {
    let (_tmp, _bare, repo_path, _other) = create_bare_and_two_clones().await;
    let config = GitConfig {
      source: repo_path.to_string_lossy().into_owned(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30,
      local_dir: repo_path.clone()
    };

    let (git, init) = GitSyncLifecycle::open(config, &repo_path).await.expect("open");

    assert!(matches!(init, GitInit::UpToDate | GitInit::Updated));
    assert!(git.should_track(Path::new("README.md")));
    assert!(!git.should_track(Path::new(".git/config")));
  }

  #[tokio::test]
  async fn sync_local_commits_and_pushes_dirty_files() {
    let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
    let config = GitConfig {
      source: clone1.to_string_lossy().into_owned(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30,
      local_dir: clone1.clone()
    };
    let (git, _) = GitSyncLifecycle::open(config, &clone1).await.expect("open");
    let file = clone1.join("new.txt");
    tokio::fs::write(&file, "new").await.expect("write");

    let result = git.sync_local(&[file]).await.expect("sync");

    assert!(matches!(result, GitSync::Success { synced_files: 1 }));
  }

  #[tokio::test]
  async fn sync_local_reports_noop_as_success() {
    let (_tmp, _bare, repo_path, _other) = create_bare_and_two_clones().await;
    let config = GitConfig {
      source: repo_path.to_string_lossy().into_owned(),
      branch: "main".to_string(),
      max_push_retries: 1,
      poll_interval_secs: 30,
      local_dir: repo_path.clone()
    };
    let (git, _) = GitSyncLifecycle::open(config, &repo_path).await.expect("open");

    let result = git.sync_local(&[repo_path.join("README.md")]).await.expect("sync");

    assert!(matches!(result, GitSync::Success { synced_files: 1 }));
  }

  #[tokio::test]
  async fn refresh_remote_applies_new_remote_commit() {
    let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
    let config = GitConfig {
      source: clone1.to_string_lossy().into_owned(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30,
      local_dir: clone1.clone()
    };
    let (git, _) = GitSyncLifecycle::open(config, &clone1).await.expect("open");

    tokio::fs::write(clone2.join("remote.txt"), "remote")
      .await
      .expect("write");
    let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");
    engine2.stage(&[clone2.join("remote.txt")]).await.expect("stage");
    engine2.commit("remote change").await.expect("commit");
    engine2.push().await.expect("push");

    let result = git.refresh_remote().await.expect("refresh");

    assert!(matches!(result, super::GitRefresh::Applied { .. }));
    assert!(clone1.join("remote.txt").exists());
  }
}
