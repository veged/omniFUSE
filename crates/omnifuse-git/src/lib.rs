//! omnifuse-git — Git backend для `OmniFuse`.
//!
//! Реализует `omnifuse_core::Backend` trait через git CLI.
//! Портировано из `SimpleGitFS`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod engine;
pub mod filter;
pub mod ops;
pub mod repo_source;

use std::{
  path::{Path, PathBuf},
  sync::OnceLock,
  time::Duration
};

use omnifuse_core::{Backend, InitResult, RemoteChange, SyncResult};
use tracing::{debug, info, warn};

use crate::{
  filter::GitignoreFilter,
  ops::{GitOps, StartupSyncResult}
};

/// Конфигурация git backend'а.
#[derive(Debug, Clone)]
pub struct GitConfig {
  /// Источник: URL или локальный путь.
  pub source: String,
  /// Ветка.
  pub branch: String,
  /// Максимальное количество повторов push.
  pub max_push_retries: u32,
  /// Интервал опроса remote (секунды).
  pub poll_interval_secs: u64
}

impl Default for GitConfig {
  fn default() -> Self {
    Self {
      source: String::new(),
      branch: "main".to_string(),
      max_push_retries: 3,
      poll_interval_secs: 30
    }
  }
}

/// Git backend для `OmniFuse`.
///
/// Реализует `Backend` trait: init → clone/fetch, sync → commit+push,
/// poll → fetch+diff, apply → pull.
#[derive(Debug)]
pub struct GitBackend {
  /// Конфигурация.
  config: GitConfig,
  /// Git-операции (инициализируется в `init`).
  ops: OnceLock<GitOps>,
  /// Фильтр `.gitignore` (инициализируется в `init`).
  filter: OnceLock<GitignoreFilter>
}

impl GitBackend {
  /// Создать новый git backend.
  #[must_use]
  pub const fn new(config: GitConfig) -> Self {
    Self {
      config,
      ops: OnceLock::new(),
      filter: OnceLock::new()
    }
  }

  /// Получить `GitOps` (после инициализации).
  ///
  /// # Errors
  ///
  /// Возвращает ошибку если backend не инициализирован.
  fn ops(&self) -> anyhow::Result<&GitOps> {
    self
      .ops
      .get()
      .ok_or_else(|| anyhow::anyhow!("backend не инициализирован"))
  }

  /// Получить список изменённых файлов между local и remote HEAD.
  async fn diff_remote_files(&self) -> anyhow::Result<Vec<PathBuf>> {
    let ops = self.ops()?;
    let repo_path = ops.repo_path();
    let engine = ops.engine();

    let local_head = engine.get_head_commit().await?;
    let remote_head = engine.get_remote_head().await?;

    let Some(remote_head) = remote_head else {
      return Ok(Vec::new());
    };

    if local_head == remote_head {
      return Ok(Vec::new());
    }

    let output = tokio::process::Command::new("git")
      .current_dir(repo_path)
      .args(["diff", "--name-only", &local_head, &remote_head])
      .output()
      .await?;

    if !output.status.success() {
      return Ok(Vec::new());
    }

    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|l| repo_path.join(l))
      .collect();

    Ok(files)
  }
}

impl Backend for GitBackend {
  async fn init(&self, _local_dir: &Path) -> anyhow::Result<InitResult> {
    // Подготовить репозиторий (clone если remote)
    let source = crate::repo_source::RepoSource::parse(&self.config.source);
    let repo_path = source.ensure_available(&self.config.branch).await?;

    // Инициализировать git-операции
    let ops = GitOps::new(repo_path.clone(), self.config.branch.clone())?;
    let _ = self.ops.set(ops);

    // Инициализировать фильтр .gitignore
    let filter = GitignoreFilter::new(&repo_path);
    let _ = self.filter.set(filter);

    // Startup sync: fetch + pull
    let ops = self.ops()?;
    match ops.startup_sync().await? {
      StartupSyncResult::UpToDate => Ok(InitResult::UpToDate),
      StartupSyncResult::Updated | StartupSyncResult::Merged => Ok(InitResult::Updated),
      StartupSyncResult::Conflicts { files } => Ok(InitResult::Conflicts { files }),
      StartupSyncResult::Offline => Ok(InitResult::Offline)
    }
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    let ops = self.ops()?;

    // Коммит dirty файлов
    if let Err(e) = ops.auto_commit(dirty_files).await {
      let msg = e.to_string();
      if !msg.contains("nothing to commit") {
        return Err(e);
      }
      debug!("sync: нет изменений для коммита");
    }

    // Push с retry (внутри: push → rejected → pull → retry)
    match ops.push_with_retry(self.config.max_push_retries).await {
      Ok(()) => Ok(SyncResult::Success {
        synced_files: dirty_files.len()
      }),
      Err(e) => {
        let msg = e.to_string();
        if msg.contains("конфликт") || msg.contains("conflict") {
          warn!("sync: конфликты при push");
          Ok(SyncResult::Conflict {
            synced_files: 0,
            conflict_files: dirty_files.to_vec()
          })
        } else if msg.contains("сеть") || msg.contains("network") {
          Ok(SyncResult::Offline)
        } else {
          Err(e)
        }
      }
    }
  }

  async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
    let ops = self.ops()?;

    // Fetch и проверить наличие новых коммитов
    if !ops.check_remote().await? {
      return Ok(Vec::new());
    }

    // Получить список изменённых файлов
    let changed_files = self.diff_remote_files().await?;

    if changed_files.is_empty() {
      return Ok(Vec::new());
    }

    info!(count = changed_files.len(), "обнаружены remote изменения");

    // Возвращаем маркеры — содержимое подтянется через pull в apply_remote
    let changes = changed_files
      .into_iter()
      .map(|path| RemoteChange::Modified {
        path,
        content: Vec::new()
      })
      .collect();

    Ok(changes)
  }

  async fn apply_remote(&self, _changes: Vec<RemoteChange>) -> anyhow::Result<()> {
    let ops = self.ops()?;

    // Git pull подтянет все изменения разом
    let result = ops.engine().pull().await?;
    debug!(?result, "apply_remote: pull завершён");

    Ok(())
  }

  fn should_track(&self, path: &Path) -> bool {
    // Всегда скрывать .git/
    if path.components().any(|c| c.as_os_str() == ".git") {
      return false;
    }

    // Проверить gitignore (если фильтр инициализирован)
    if let Some(filter) = self.filter.get() {
      return !filter.is_ignored(path);
    }

    true
  }

  fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.config.poll_interval_secs)
  }

  async fn is_online(&self) -> bool {
    let Ok(ops) = self.ops() else {
      return false;
    };

    ops.engine().fetch().await.is_ok()
  }

  fn name(&self) -> &'static str {
    "git"
  }
}
