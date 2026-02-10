//! Обработчик событий VFS для UI (Tauri, CLI, логи).

use std::path::Path;

/// Уровень лога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
  /// Отладочная информация.
  Debug,
  /// Информационное сообщение.
  Info,
  /// Предупреждение.
  Warn,
  /// Ошибка.
  Error
}

/// Обработчик событий для UI (Tauri, CLI, логи).
///
/// Все методы имеют default-реализации (noop), поэтому
/// реализатор может перегрузить только нужные.
#[allow(unused_variables)]
pub trait VfsEventHandler: Send + Sync + 'static {
  /// Файловая система смонтирована.
  fn on_mounted(&self, source: &str, mount_point: &Path) {}

  /// Файловая система размонтирована.
  fn on_unmounted(&self) {}

  /// Произошла ошибка.
  fn on_error(&self, message: &str) {}

  /// Лог-сообщение.
  fn on_log(&self, level: LogLevel, message: &str) {}

  /// Файл записан.
  fn on_file_written(&self, path: &Path, bytes: usize) {}

  /// Файл помечен как грязный.
  fn on_file_dirty(&self, path: &Path) {}

  /// Файл создан.
  fn on_file_created(&self, path: &Path) {}

  /// Файл удалён.
  fn on_file_deleted(&self, path: &Path) {}

  /// Файл переименован.
  fn on_file_renamed(&self, old_path: &Path, new_path: &Path) {}

  /// Выполнен коммит.
  fn on_commit(&self, hash: &str, files_count: usize, message: &str) {}

  /// Выполнен push.
  fn on_push(&self, items_count: usize) {}

  /// Push отклонён.
  fn on_push_rejected(&self) {}

  /// Синхронизация завершена.
  fn on_sync(&self, result: &str) {}
}

/// Пустой обработчик событий (для тестов и CLI без UI).
pub struct NoopEventHandler;

impl VfsEventHandler for NoopEventHandler {}
