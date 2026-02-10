//! Backend trait — единый интерфейс для синхронизируемых хранилищ.
//!
//! Управляет sync самостоятельно: VFS-ядро говорит ЧТО синхронизировать,
//! backend решает КАК.

use std::{
  path::{Path, PathBuf},
  time::Duration
};

/// Backend для синхронизируемого хранилища.
///
/// Управляет sync самостоятельно: VFS-ядро говорит ЧТО синхронизировать,
/// backend решает КАК.
pub trait Backend: Send + Sync + 'static {
  /// Инициализация: подготовить локальную директорию.
  ///
  /// Git: clone (если remote URL) или проверить существующий repo.
  /// Wiki: создать local dir, fetch дерево, записать файлы.
  fn init(&self, local_dir: &Path) -> impl Future<Output = anyhow::Result<InitResult>> + Send;

  /// Синхронизировать локальные изменения с remote.
  ///
  /// Вызывается `SyncEngine`'ом после debounce/close trigger.
  /// Backend сам решает как мержить при конфликтах.
  fn sync(&self, dirty_files: &[PathBuf]) -> impl Future<Output = anyhow::Result<SyncResult>> + Send;

  /// Проверить remote на изменения (периодический poll).
  fn poll_remote(
    &self
  ) -> impl Future<Output = anyhow::Result<Vec<RemoteChange>>> + Send;

  /// Применить remote изменения к локальной директории.
  ///
  /// НЕ перезаписывает dirty файлы (`SyncEngine` проверяет).
  fn apply_remote(
    &self,
    changes: Vec<RemoteChange>
  ) -> impl Future<Output = anyhow::Result<()>> + Send;

  /// Должен ли файл синхронизироваться с remote?
  ///
  /// Git: проверка `.gitignore`, скрытие `.git/`.
  /// Wiki: только `.md` файлы.
  fn should_track(&self, path: &Path) -> bool;

  /// Интервал опроса remote на изменения.
  fn poll_interval(&self) -> Duration;

  /// Проверить доступность remote.
  fn is_online(&self) -> impl Future<Output = bool> + Send;

  /// Название backend'а для логов и UI.
  fn name(&self) -> &'static str;
}

/// Результат инициализации.
#[derive(Debug, Clone)]
pub enum InitResult {
  /// Новый клон/fetch.
  Fresh,
  /// Локальная директория уже актуальна.
  UpToDate,
  /// Обновлено с remote.
  Updated,
  /// Есть конфликты при инициализации.
  Conflicts {
    /// Файлы с конфликтами.
    files: Vec<PathBuf>
  },
  /// Remote недоступен, работаем с локальным состоянием.
  Offline
}

/// Результат синхронизации.
#[derive(Debug, Clone)]
pub enum SyncResult {
  /// Все файлы синхронизированы успешно.
  Success {
    /// Количество синхронизированных файлов.
    synced_files: usize
  },
  /// Часть файлов конфликтует.
  Conflict {
    /// Количество синхронизированных файлов.
    synced_files: usize,
    /// Файлы с конфликтами.
    conflict_files: Vec<PathBuf>
  },
  /// Remote недоступен.
  Offline
}

/// Изменение на remote.
#[derive(Debug, Clone)]
pub enum RemoteChange {
  /// Файл был изменён.
  Modified {
    /// Путь к файлу.
    path: PathBuf,
    /// Новое содержимое.
    content: Vec<u8>
  },
  /// Файл был удалён.
  Deleted {
    /// Путь к файлу.
    path: PathBuf
  }
}

impl RemoteChange {
  /// Получить путь файла из изменения.
  #[must_use]
  pub fn path(&self) -> &Path {
    match self {
      Self::Modified { path, .. } | Self::Deleted { path } => path
    }
  }
}
