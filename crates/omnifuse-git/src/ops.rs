//! Высокоуровневые git-операции.
//!
//! Комбинирует низкоуровневые git-команды в операции
//! подходящие для VFS sync workflow.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::engine::{FetchResult, GitEngine, MergeResult, PushResult};

/// Высокоуровневые git-операции.
#[derive(Debug)]
pub struct GitOps {
  /// Базовый git engine.
  engine: GitEngine
}

impl GitOps {
  /// Создать новый wrapper для git-операций.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку если репозиторий не открывается.
  pub fn new(repo_path: PathBuf, branch: String) -> anyhow::Result<Self> {
    let engine = GitEngine::new(repo_path, branch)?;
    Ok(Self { engine })
  }

  /// Получить engine.
  #[must_use]
  pub const fn engine(&self) -> &GitEngine {
    &self.engine
  }

  /// Путь к репозиторию.
  #[must_use]
  pub fn repo_path(&self) -> &Path {
    self.engine.repo_path()
  }

  /// Синхронизация при запуске.
  ///
  /// Fetch + merge.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при сбое sync.
  pub async fn startup_sync(&self) -> anyhow::Result<StartupSyncResult> {
    info!("startup sync");

    match self.engine.fetch().await {
      Ok(FetchResult::UpToDate) => {
        debug!("startup sync: already up to date");
        return Ok(StartupSyncResult::UpToDate);
      }
      Ok(FetchResult::Updated { commits }) => {
        debug!(commits, "startup sync: получены новые коммиты");
      }
      Err(e) => {
        warn!("startup sync: fetch не удался, работаем offline: {e}");
        return Ok(StartupSyncResult::Offline);
      }
    }

    match self.engine.pull().await {
      Ok(MergeResult::UpToDate) => Ok(StartupSyncResult::UpToDate),
      Ok(MergeResult::FastForward) => {
        info!("startup sync: fast-forward merge");
        Ok(StartupSyncResult::Updated)
      }
      Ok(MergeResult::Merged { commit }) => {
        info!(commit = %commit, "startup sync: merge commit создан");
        Ok(StartupSyncResult::Merged)
      }
      Ok(MergeResult::Conflict { files }) => {
        warn!(files = ?files, "startup sync: конфликты");
        Ok(StartupSyncResult::Conflicts { files })
      }
      Err(e) => {
        warn!("startup sync: pull не удался: {e}");
        Ok(StartupSyncResult::Offline)
      }
    }
  }

  /// Закоммитить конкретные файлы.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче коммита.
  pub async fn commit_changes(&self, files: &[PathBuf], message: &str) -> anyhow::Result<String> {
    if files.is_empty() {
      anyhow::bail!("нет файлов для коммита");
    }

    self.engine.stage(files).await?;
    let hash = self.engine.commit(message).await?;
    info!(hash = %hash, files = files.len(), "коммит создан");
    Ok(hash)
  }

  /// Автокоммит с меткой времени.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче коммита.
  pub async fn auto_commit(&self, files: &[PathBuf]) -> anyhow::Result<String> {
    let message = format!(
      "[auto] {} file(s) changed at {}",
      files.len(),
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    self.commit_changes(files, &message).await
  }

  /// Push с повторами и автопулом при отклонении.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку если push окончательно не удался.
  pub async fn push_with_retry(&self, max_retries: u32) -> anyhow::Result<()> {
    let mut retries = 0;

    loop {
      match self.engine.push().await? {
        PushResult::Success => {
          info!("push успешен");
          return Ok(());
        }
        PushResult::NoRemote => {
          warn!("remote не настроен, пропускаем push");
          return Ok(());
        }
        PushResult::Rejected => {
          if retries >= max_retries {
            anyhow::bail!("push отклонён после {max_retries} попыток");
          }

          retries += 1;
          warn!(retries, "push отклонён, pull и повтор");

          match self.engine.pull().await? {
            MergeResult::Conflict { files } => {
              anyhow::bail!("{} файлов в конфликте", files.len());
            }
            _ => {
              tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
          }
        }
      }
    }
  }

  /// Проверить наличие удалённых изменений.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку при неудаче fetch.
  pub async fn check_remote(&self) -> anyhow::Result<bool> {
    let local_head = self.engine.get_head_commit().await?;
    self.engine.fetch().await?;
    let remote_head = self.engine.get_remote_head().await?;
    Ok(remote_head.is_some_and(|r| r != local_head))
  }
}

/// Результат startup sync.
#[derive(Debug, Clone)]
pub enum StartupSyncResult {
  /// Уже актуально.
  UpToDate,
  /// Обновлено с remote.
  Updated,
  /// Создан merge коммит.
  Merged,
  /// Обнаружены конфликты.
  Conflicts {
    /// Файлы с конфликтами.
    files: Vec<PathBuf>
  },
  /// Работаем offline.
  Offline
}
