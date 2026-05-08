//! Git synchronization lifecycle.

use std::path::{Path, PathBuf};

use tracing::info;

use crate::{
  GitConfig,
  engine::MergeResult,
  error::classify_git_error,
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
  #[allow(dead_code)]
  ops: GitOps,
  tracking: GitTrackingRules,
  #[allow(dead_code)]
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
    engine::tests::create_bare_and_two_clones,
    sync_lifecycle::{GitInit, GitSyncLifecycle}
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
}
