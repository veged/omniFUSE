//! Движок синхронизации.
//!
//! Оркестрирует timing: собирает dirty файлы через debounce,
//! триггерит sync на close, обрабатывает periodic poll remote.
//!
//! НЕ занимается merge — это ответственность `Backend`.

use std::{
  collections::HashSet,
  path::PathBuf,
  sync::Arc
};

use tokio::{
  sync::mpsc,
  time::{Instant, sleep, sleep_until}
};
use tracing::{debug, error, warn};

use crate::{
  backend::{Backend, SyncResult},
  config::SyncConfig,
  events::{LogLevel, VfsEventHandler}
};

/// Событие от FUSE слоя.
#[derive(Debug, Clone)]
pub enum FsEvent {
  /// Файл изменён (запись в буфер).
  FileModified(PathBuf),
  /// Файл закрыт (после flush).
  FileClosed(PathBuf),
  /// Принудительный sync.
  Flush,
  /// Завершение работы.
  Shutdown
}

/// Движок синхронизации.
///
/// Оркестрирует timing: собирает dirty файлы, батчит через debounce,
/// триггерит sync на close, обрабатывает periodic poll.
///
/// НЕ занимается merge — это ответственность `Backend`.
pub struct SyncEngine {
  event_tx: mpsc::Sender<FsEvent>
}

impl SyncEngine {
  /// Создать и запустить `SyncEngine`.
  ///
  /// Запускает два background worker'а:
  /// 1. `dirty_worker` — собирает `FsEvent`, ведёт `DirtySet`, триггерит sync
  /// 2. `poll_worker` — периодически вызывает `backend.poll_remote()`
  pub fn start<B: Backend>(
    config: SyncConfig,
    backend: Arc<B>,
    events: Arc<dyn VfsEventHandler>
  ) -> (Self, tokio::task::JoinHandle<()>) {
    let (event_tx, event_rx) = mpsc::channel(256);

    let handle = tokio::spawn(Self::dirty_worker(
      config,
      backend.clone(),
      events.clone(),
      event_rx
    ));

    // Poll worker — отдельная задача
    let poll_backend = backend;
    let poll_events = events;
    tokio::spawn(async move {
      Self::poll_worker(poll_backend, poll_events).await;
    });

    (Self { event_tx }, handle)
  }

  /// Получить `Sender` для отправки событий.
  #[must_use]
  pub fn sender(&self) -> mpsc::Sender<FsEvent> {
    self.event_tx.clone()
  }

  /// Завершить работу `SyncEngine`.
  ///
  /// # Errors
  ///
  /// Возвращает ошибку если канал уже закрыт.
  pub async fn shutdown(&self) -> anyhow::Result<()> {
    self
      .event_tx
      .send(FsEvent::Shutdown)
      .await
      .map_err(|e| anyhow::anyhow!("ошибка отправки Shutdown: {e}"))
  }

  /// Worker обработки dirty файлов.
  ///
  /// Логика:
  /// - `FileModified` → добавить в dirty set, установить debounce deadline
  /// - `FileClosed` → добавить в dirty set, немедленный sync (не ждём debounce)
  /// - `Flush` → немедленный sync
  /// - `Shutdown` → финальный sync, выход
  /// - debounce timeout → sync если dirty set не пуст
  async fn dirty_worker<B: Backend>(
    config: SyncConfig,
    backend: Arc<B>,
    events: Arc<dyn VfsEventHandler>,
    mut event_rx: mpsc::Receiver<FsEvent>
  ) {
    let mut dirty_set: HashSet<PathBuf> = HashSet::new();
    let mut trigger_sync = false;
    let debounce_duration = config.debounce_timeout();

    // Deadline для debounce — далеко в будущем (не активен)
    let far_future = Instant::now() + std::time::Duration::from_secs(365 * 24 * 3600);
    let mut debounce_deadline = far_future;

    loop {
      tokio::select! {
        event = event_rx.recv() => {
          match event {
            Some(FsEvent::FileModified(path)) => {
              dirty_set.insert(path);
              debounce_deadline = Instant::now() + debounce_duration;
            }
            Some(FsEvent::FileClosed(path)) => {
              dirty_set.insert(path);
              trigger_sync = true;
            }
            Some(FsEvent::Flush) => {
              trigger_sync = true;
            }
            Some(FsEvent::Shutdown) | None => {
              // Финальный sync
              if !dirty_set.is_empty() {
                Self::do_sync(&backend, &events, &mut dirty_set).await;
              }
              break;
            }
          }
        }
        () = sleep_until(debounce_deadline), if !dirty_set.is_empty() => {
          trigger_sync = true;
          debounce_deadline = far_future;
        }
      }

      if trigger_sync && !dirty_set.is_empty() {
        Self::do_sync(&backend, &events, &mut dirty_set).await;
        trigger_sync = false;
      }
    }

    debug!("dirty_worker завершён");
  }

  /// Выполнить синхронизацию dirty файлов.
  async fn do_sync<B: Backend>(
    backend: &Arc<B>,
    events: &Arc<dyn VfsEventHandler>,
    dirty_set: &mut HashSet<PathBuf>
  ) {
    let files: Vec<PathBuf> = dirty_set.drain().filter(|f| backend.should_track(f)).collect();

    if files.is_empty() {
      return;
    }

    debug!(count = files.len(), "синхронизация dirty файлов");

    match backend.sync(&files).await {
      Ok(SyncResult::Success { synced_files }) => {
        events.on_push(synced_files);
        debug!(synced_files, "sync успешен");
      }
      Ok(SyncResult::Conflict {
        synced_files,
        conflict_files
      }) => {
        events.on_push(synced_files);
        warn!(
          synced_files,
          conflicts = conflict_files.len(),
          "sync с конфликтами"
        );
        events.on_log(
          LogLevel::Warn,
          &format!("конфликты: {conflict_files:?}")
        );
      }
      Ok(SyncResult::Offline) => {
        warn!("remote недоступен, файлы будут синхронизированы позже");
        events.on_log(LogLevel::Warn, "remote недоступен");
      }
      Err(e) => {
        error!(error = %e, "ошибка sync");
        events.on_log(LogLevel::Error, &format!("ошибка sync: {e}"));
      }
    }
  }

  /// Worker периодического опроса remote.
  async fn poll_worker<B: Backend>(backend: Arc<B>, events: Arc<dyn VfsEventHandler>) {
    let interval = backend.poll_interval();

    loop {
      sleep(interval).await;

      match backend.poll_remote().await {
        Ok(changes) if !changes.is_empty() => {
          let count = changes.len();
          debug!(count, "получены изменения с remote");

          if let Err(e) = backend.apply_remote(changes).await {
            warn!(error = %e, "ошибка применения remote изменений");
            events.on_log(
              LogLevel::Warn,
              &format!("ошибка применения remote изменений: {e}")
            );
          } else {
            events.on_sync("updated");
          }
        }
        Ok(_) => {
          // Нет изменений
        }
        Err(e) => {
          warn!(error = %e, "ошибка poll remote");
          events.on_log(LogLevel::Warn, &format!("poll failed: {e}"));
        }
      }
    }
  }
}
