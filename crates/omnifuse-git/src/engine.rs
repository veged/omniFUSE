//! Git engine — обёртка над git CLI.
//!
//! Портировано из `SimpleGitFS` `core/src/git/engine.rs`.

use std::{
  path::{Path, PathBuf},
  sync::Arc
};

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Результат fetch.
#[derive(Debug, Clone)]
pub enum FetchResult {
  /// Нет изменений.
  UpToDate,
  /// Получены новые коммиты.
  Updated {
    /// Количество коммитов.
    commits: usize
  }
}

/// Результат push.
#[derive(Debug, Clone)]
pub enum PushResult {
  /// Push успешен.
  Success,
  /// Push отклонён (нужен pull).
  Rejected,
  /// Remote не настроен.
  NoRemote
}

/// Результат merge.
#[derive(Debug, Clone)]
pub enum MergeResult {
  /// Уже актуально.
  UpToDate,
  /// Fast-forward merge.
  FastForward,
  /// Создан merge commit.
  Merged {
    /// Хеш merge коммита.
    commit: String
  },
  /// Обнаружены конфликты.
  Conflict {
    /// Файлы с конфликтами.
    files: Vec<PathBuf>
  }
}

/// Git engine для операций с репозиторием.
#[derive(Debug)]
pub struct GitEngine {
  /// Путь к репозиторию.
  repo_path: PathBuf,
  /// Отслеживаемая ветка.
  branch: String,
  /// Имя remote.
  remote: String,
  /// Lock для сериализации git операций.
  op_lock: Arc<RwLock<()>>
}

impl GitEngine {
  /// Создать новый git engine.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку если путь — не git-репозиторий.
  pub fn new(repo_path: PathBuf, branch: String) -> anyhow::Result<Self> {
    let git_dir = repo_path.join(".git");
    if !git_dir.exists() {
      anyhow::bail!("не git-репозиторий: {}", repo_path.display());
    }

    Ok(Self {
      repo_path,
      branch,
      remote: "origin".to_string(),
      op_lock: Arc::new(RwLock::new(()))
    })
  }

  /// Путь к репозиторию.
  #[must_use]
  pub fn repo_path(&self) -> &Path {
    &self.repo_path
  }

  /// Текущая ветка.
  #[must_use]
  pub fn branch(&self) -> &str {
    &self.branch
  }

  /// Добавить файлы в staging area.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git add`.
  pub async fn stage(&self, files: &[PathBuf]) -> anyhow::Result<()> {
    let _lock = self.op_lock.write().await;

    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(&self.repo_path).arg("add");

    for file in files {
      if let Ok(rel_path) = file.strip_prefix(&self.repo_path) {
        cmd.arg(rel_path);
      } else {
        cmd.arg(file);
      }
    }

    let output = cmd.output().await?;
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git add failed: {stderr}");
    }

    debug!(files = files.len(), "staged files");
    Ok(())
  }

  /// Создать коммит.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git commit`.
  pub async fn commit(&self, message: &str) -> anyhow::Result<String> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["commit", "-m", message])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      if stderr.contains("nothing to commit") {
        anyhow::bail!("nothing to commit");
      }
      anyhow::bail!("git commit failed: {stderr}");
    }

    let hash = self.get_head_commit().await?;
    info!(hash = %hash, "создан коммит");
    Ok(hash)
  }

  /// Получить хеш HEAD коммита.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git rev-parse`.
  pub async fn get_head_commit(&self) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["rev-parse", "HEAD"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git rev-parse failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Получить хеш remote HEAD коммита.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git rev-parse`.
  pub async fn get_remote_head(&self) -> anyhow::Result<Option<String>> {
    let ref_name = format!("{}/{}", self.remote, self.branch);

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["rev-parse", &ref_name])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(None);
    }

    Ok(Some(
      String::from_utf8_lossy(&output.stdout).trim().to_string()
    ))
  }

  /// Fetch с remote.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче fetch.
  pub async fn fetch(&self) -> anyhow::Result<FetchResult> {
    let _lock = self.op_lock.write().await;

    let before = self.get_remote_head().await?;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["fetch", &self.remote, &self.branch])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      if stderr.contains("Could not resolve host") || stderr.contains("Connection refused") {
        warn!("fetch failed (сеть): {}", stderr.trim());
        anyhow::bail!("сеть недоступна: {}", stderr.trim());
      }
      anyhow::bail!("git fetch failed: {stderr}");
    }

    let after = self.get_remote_head().await?;

    if before == after {
      debug!("fetch: up to date");
      Ok(FetchResult::UpToDate)
    } else {
      info!("fetch: новые коммиты");
      Ok(FetchResult::Updated { commits: 1 })
    }
  }

  /// Pull с remote (fetch + rebase).
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче pull.
  pub async fn pull(&self) -> anyhow::Result<MergeResult> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["pull", "--rebase", &self.remote, &self.branch])
      .output()
      .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
      if stderr.contains("CONFLICT")
        || stderr.contains("Automatic merge failed")
        || stdout.contains("CONFLICT")
        || stderr.contains("could not apply")
      {
        // Отменить rebase при конфликтах
        let _ = tokio::process::Command::new("git")
          .current_dir(&self.repo_path)
          .args(["rebase", "--abort"])
          .output()
          .await;

        let conflict_files = self.get_conflict_files().await.unwrap_or_default();
        warn!(files = ?conflict_files, "pull/rebase: конфликты");
        return Ok(MergeResult::Conflict {
          files: conflict_files
        });
      }
      anyhow::bail!("git pull failed: {stderr}");
    }

    if stdout.contains("Already up to date")
      || (stdout.contains("Current branch") && stdout.contains("is up to date"))
    {
      debug!("pull: already up to date");
      Ok(MergeResult::UpToDate)
    } else if stdout.contains("Fast-forward") {
      info!("pull: fast-forward");
      Ok(MergeResult::FastForward)
    } else {
      let hash = self.get_head_commit().await?;
      info!(hash = %hash, "pull: rebased/merged");
      Ok(MergeResult::Merged { commit: hash })
    }
  }

  /// Push на remote.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче push.
  pub async fn push(&self) -> anyhow::Result<PushResult> {
    let _lock = self.op_lock.write().await;

    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["push", &self.remote, &self.branch])
      .output()
      .await?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
      if stderr.contains("rejected") || stderr.contains("non-fast-forward") {
        warn!("push отклонён: нужен pull");
        return Ok(PushResult::Rejected);
      }
      if stderr.contains("No configured push destination") {
        return Ok(PushResult::NoRemote);
      }
      anyhow::bail!("git push failed: {stderr}");
    }

    info!("push: success");
    Ok(PushResult::Success)
  }

  /// Получить список файлов с конфликтами.
  async fn get_conflict_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["diff", "--name-only", "--diff-filter=U"])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(Vec::new());
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|l| self.repo_path.join(l))
      .collect();

    Ok(files)
  }

  /// Проверить наличие незакоммиченных изменений.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git status`.
  pub async fn has_changes(&self) -> anyhow::Result<bool> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["status", "--porcelain"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git status failed: {stderr}");
    }

    Ok(!output.stdout.is_empty())
  }

  /// Получить список изменённых файлов.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче `git status`.
  pub async fn modified_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let output = tokio::process::Command::new("git")
      .current_dir(&self.repo_path)
      .args(["status", "--porcelain"])
      .output()
      .await?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git status failed: {stderr}");
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .filter_map(|line| {
        if line.len() > 3 {
          Some(self.repo_path.join(&line[3..]))
        } else {
          None
        }
      })
      .collect();

    Ok(files)
  }
}
