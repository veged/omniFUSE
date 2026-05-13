//! Test utilities: `MockBackend` and `TestEventHandler`.

#![allow(clippy::expect_used)]

use std::{
  path::{Path, PathBuf},
  sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicUsize, Ordering}
  },
  time::Duration
};

use crate::{
  Code, Event, Kind, Level, Sink,
  backend::{Backend, InitResult, RemoteRefresh, RemoteRefreshResult, SyncResult}
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().expect("test mutex should not be poisoned")
}

/// Mock `Backend` for unit testing `SyncEngine` and `OmniFuseVfs`.
///
/// Records all calls for subsequent assertions.
pub struct MockBackend {
  /// Recorded `sync()` calls — each with a list of files.
  pub sync_calls: Arc<Mutex<Vec<Vec<PathBuf>>>>,
  /// Call counter for `refresh_remote()`.
  pub refresh_calls: Arc<AtomicUsize>,
  /// Result returned by `sync()`.
  pub sync_result: Arc<Mutex<SyncResult>>,
  /// Result returned by `refresh_remote()`.
  pub refresh_result: Arc<Mutex<RemoteRefreshResult>>,
  /// Error returned by `sync()` (if set).
  pub sync_error: Arc<Mutex<Option<String>>>,
  /// Error returned by `refresh_remote()` (if set).
  pub refresh_error: Arc<Mutex<Option<String>>>,
  /// The `should_track` function.
  pub track_fn: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
  /// Poll interval.
  pub poll_interval_dur: Duration,
  /// Online status.
  pub online: Arc<AtomicBool>,
  /// Delay before returning from `sync()` (for race condition testing).
  pub sync_delay: Arc<Mutex<Option<Duration>>>
}

impl Default for MockBackend {
  fn default() -> Self {
    Self::new()
  }
}

impl MockBackend {
  /// Create a `MockBackend` with default settings.
  #[must_use]
  pub fn new() -> Self {
    Self {
      sync_calls: Arc::new(Mutex::new(Vec::new())),
      refresh_calls: Arc::new(AtomicUsize::new(0)),
      sync_result: Arc::new(Mutex::new(SyncResult::Success { synced_files: 0 })),
      refresh_result: Arc::new(Mutex::new(RemoteRefreshResult::Unchanged)),
      sync_error: Arc::new(Mutex::new(None)),
      refresh_error: Arc::new(Mutex::new(None)),
      track_fn: Arc::new(|path| !path.to_string_lossy().contains(".git")),
      poll_interval_dur: Duration::from_secs(3600),
      online: Arc::new(AtomicBool::new(true)),
      sync_delay: Arc::new(Mutex::new(None))
    }
  }

  /// Set the `sync()` result.
  pub fn set_sync_result(&self, result: SyncResult) {
    *lock(&self.sync_result) = result;
  }

  /// Set the `sync()` error.
  pub fn set_sync_error(&self, msg: &str) {
    *lock(&self.sync_error) = Some(msg.to_string());
  }

  /// Set the result returned by `refresh_remote()`.
  pub fn set_refresh_result(&self, result: RemoteRefreshResult) {
    *lock(&self.refresh_result) = result;
  }

  /// Set the `refresh_remote()` error.
  pub fn set_refresh_error(&self, msg: &str) {
    *lock(&self.refresh_error) = Some(msg.to_string());
  }

  /// Set the delay before returning from `sync()`.
  pub fn set_sync_delay(&self, delay: Duration) {
    *lock(&self.sync_delay) = Some(delay);
  }

  /// Clear the `sync()` delay.
  pub fn clear_sync_delay(&self) {
    *lock(&self.sync_delay) = None;
  }

  /// Number of `sync()` calls.
  #[must_use]
  pub fn sync_call_count(&self) -> usize {
    lock(&self.sync_calls).len()
  }

  /// Number of `refresh_remote()` calls.
  #[must_use]
  pub fn refresh_call_count(&self) -> usize {
    self.refresh_calls.load(Ordering::Relaxed)
  }

  /// Last set of files passed to `sync()`.
  #[must_use]
  pub fn last_sync_files(&self) -> Option<Vec<PathBuf>> {
    lock(&self.sync_calls).last().cloned()
  }
}

impl Backend for MockBackend {
  async fn init(&self, _local_dir: &Path) -> anyhow::Result<InitResult> {
    Ok(InitResult::UpToDate)
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    lock(&self.sync_calls).push(dirty_files.to_vec());

    // Simulate sync delay (for race condition testing)
    let delay = *lock(&self.sync_delay);
    if let Some(d) = delay {
      tokio::time::sleep(d).await;
    }

    let maybe_err = lock(&self.sync_error).as_ref().map(ToString::to_string);
    if let Some(msg) = maybe_err {
      return Err(anyhow::anyhow!("{msg}"));
    }

    Ok(lock(&self.sync_result).clone())
  }

  async fn refresh_remote(&self, _request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    self.refresh_calls.fetch_add(1, Ordering::Relaxed);

    if let Some(ref msg) = *lock(&self.refresh_error) {
      return Err(anyhow::anyhow!("{msg}"));
    }

    Ok(lock(&self.refresh_result).clone())
  }

  fn should_track(&self, path: &Path) -> bool {
    (self.track_fn)(path)
  }

  fn poll_interval(&self) -> Duration {
    self.poll_interval_dur
  }

  async fn is_online(&self) -> bool {
    self.online.load(Ordering::Relaxed)
  }

  fn name(&self) -> &'static str {
    "mock"
  }

  fn classify_error(&self, error: &anyhow::Error) -> Code {
    let message = error.to_string().to_lowercase();
    if message.contains("network") || message.contains("connection") {
      Code::Offline
    } else if message.contains("conflict") {
      Code::Conflict
    } else {
      Code::Internal
    }
  }
}

/// Test event handler that records all calls.
pub struct TestEventHandler {
  /// Recorded events.
  pub event_calls: Arc<Mutex<Vec<Event>>>,
  /// Derived successful sync counts.
  pub push_calls: Arc<Mutex<Vec<usize>>>,
  /// Derived remote update markers.
  pub sync_calls: Arc<Mutex<Vec<String>>>,
  /// Derived warning/error messages.
  pub log_calls: Arc<Mutex<Vec<(Level, String)>>>,
  /// Derived file writes.
  pub written_calls: Arc<Mutex<Vec<(PathBuf, usize)>>>,
  /// Derived file creates.
  pub created_calls: Arc<Mutex<Vec<PathBuf>>>,
  /// Derived file deletes.
  pub deleted_calls: Arc<Mutex<Vec<PathBuf>>>,
  /// Derived file renames.
  pub renamed_calls: Arc<Mutex<Vec<(PathBuf, PathBuf)>>>
}

impl Default for TestEventHandler {
  fn default() -> Self {
    Self::new()
  }
}

impl TestEventHandler {
  /// Create an empty handler.
  #[must_use]
  pub fn new() -> Self {
    Self {
      event_calls: Arc::new(Mutex::new(Vec::new())),
      push_calls: Arc::new(Mutex::new(Vec::new())),
      sync_calls: Arc::new(Mutex::new(Vec::new())),
      log_calls: Arc::new(Mutex::new(Vec::new())),
      written_calls: Arc::new(Mutex::new(Vec::new())),
      created_calls: Arc::new(Mutex::new(Vec::new())),
      deleted_calls: Arc::new(Mutex::new(Vec::new())),
      renamed_calls: Arc::new(Mutex::new(Vec::new()))
    }
  }

  /// Number of `on_push` calls.
  #[must_use]
  pub fn push_count(&self) -> usize {
    lock(&self.push_calls).len()
  }

  /// Number of log entries at a given level.
  #[must_use]
  pub fn log_count(&self, level: Level) -> usize {
    lock(&self.log_calls).iter().filter(|(l, _)| *l == level).count()
  }

  /// Snapshot of product events.
  #[must_use]
  pub fn events(&self) -> Vec<Event> {
    lock(&self.event_calls).clone()
  }

  /// Snapshot of product events.
  #[must_use]
  pub fn operational_events(&self) -> Vec<Event> {
    self.events()
  }
}

/// Default timeout for async tests (10 seconds).
///
/// Prevents tests from hanging indefinitely due to deadlocks or timing issues.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Wrap an async test body with a timeout and status output.
///
/// - Prints `[TEST] Starting: <name>` at the beginning.
/// - Prints `[TEST] Completed: <name>` on success.
/// - Panics with a descriptive message if the timeout expires.
///
/// # Panics
///
/// Panics if the wrapped future does not complete before `TEST_TIMEOUT`.
///
/// Usage:
/// ```ignore
/// #[tokio::test]
/// async fn test_foo() {
///     with_timeout("test_foo", async {
///         // test body
///     }).await;
/// }
/// ```
#[allow(clippy::panic)]
pub async fn with_timeout<F, T>(test_name: &str, f: F) -> T
where
  F: std::future::Future<Output = T>
{
  eprintln!("[TEST] Starting: {test_name}");
  let result = tokio::time::timeout(TEST_TIMEOUT, f)
    .await
    .unwrap_or_else(|_| panic!("[TEST] {test_name} timed out after {TEST_TIMEOUT:?} — possible deadlock"));
  eprintln!("[TEST] Completed: {test_name}");
  result
}

impl Sink for TestEventHandler {
  fn emit(&self, event: Event) {
    self.record_derived_views(&event);
    lock(&self.event_calls).push(event);
  }
}

impl TestEventHandler {
  fn record_derived_views(&self, event: &Event) {
    match event.kind {
      Kind::SyncDone => {
        if let Some(synced) = event_data_usize(event, "synced") {
          lock(&self.push_calls).push(synced);
        }
      }
      Kind::RemoteChange => {
        lock(&self.sync_calls).push("updated".to_string());
      }
      Kind::FileChange => {
        if let (Some(path), Some(bytes)) = (event_data_path(event, "path"), event_data_usize(event, "bytes")) {
          lock(&self.written_calls).push((path, bytes));
        }
      }
      Kind::FileCreate => {
        if let Some(path) = event_data_path(event, "path") {
          lock(&self.created_calls).push(path);
        }
      }
      Kind::FileDelete => {
        if let Some(path) = event_data_path(event, "path") {
          lock(&self.deleted_calls).push(path);
        }
      }
      Kind::FileRename => {
        if let (Some(old_path), Some(new_path)) = (event_data_path(event, "oldPath"), event_data_path(event, "newPath"))
        {
          lock(&self.renamed_calls).push((old_path, new_path));
        }
      }
      _ => {}
    }

    if matches!(event.level, Level::Warn | Level::Error) {
      lock(&self.log_calls).push((event.level, event_message(event)));
    }
  }
}

fn event_data_path(event: &Event, key: &str) -> Option<PathBuf> {
  event.data.as_ref()?.get(key)?.as_str().map(PathBuf::from)
}

fn event_data_usize(event: &Event, key: &str) -> Option<usize> {
  event
    .data
    .as_ref()?
    .get(key)?
    .as_u64()
    .and_then(|value| value.try_into().ok())
}

fn event_message(event: &Event) -> String {
  if let Some(error) = &event.error {
    return error.message.clone();
  }

  if event.kind == Kind::Conflict {
    return "conflict".to_string();
  }

  event
    .data
    .as_ref()
    .and_then(|data| data.get("message").or_else(|| data.get("reason")))
    .and_then(serde_json::Value::as_str)
    .unwrap_or("event")
    .to_string()
}
