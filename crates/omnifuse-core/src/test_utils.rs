//! Test utilities: `MockBackend` and `TestEventHandler`.

#![allow(clippy::expect_used)]

use std::{
  path::{Path, PathBuf},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering}
  },
  time::Duration
};

use crate::{
  backend::{Backend, InitResult, RemoteChange, SyncResult},
  events::{LogLevel, VfsEventHandler}
};

/// Mock Backend for unit testing SyncEngine and OmniFuseVfs.
///
/// Records all calls for subsequent assertions.
pub struct MockBackend {
  /// Recorded `sync()` calls — each with a list of files.
  pub sync_calls: Arc<Mutex<Vec<Vec<PathBuf>>>>,
  /// Call counter for `poll_remote()`.
  pub poll_calls: Arc<AtomicUsize>,
  /// Recorded `apply_remote()` calls.
  pub apply_calls: Arc<Mutex<Vec<Vec<RemoteChange>>>>,
  /// Result returned by `sync()`.
  pub sync_result: Arc<Mutex<SyncResult>>,
  /// Changes returned by `poll_remote()`.
  pub poll_result: Arc<Mutex<Vec<RemoteChange>>>,
  /// Error returned by `sync()` (if set).
  pub sync_error: Arc<Mutex<Option<String>>>,
  /// Error returned by `poll_remote()` (if set).
  pub poll_error: Arc<Mutex<Option<String>>>,
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
  /// Create a MockBackend with default settings.
  #[must_use]
  pub fn new() -> Self {
    Self {
      sync_calls: Arc::new(Mutex::new(Vec::new())),
      poll_calls: Arc::new(AtomicUsize::new(0)),
      apply_calls: Arc::new(Mutex::new(Vec::new())),
      sync_result: Arc::new(Mutex::new(SyncResult::Success { synced_files: 0 })),
      poll_result: Arc::new(Mutex::new(Vec::new())),
      sync_error: Arc::new(Mutex::new(None)),
      poll_error: Arc::new(Mutex::new(None)),
      track_fn: Arc::new(|path| {
        !path.to_string_lossy().contains(".git")
      }),
      poll_interval_dur: Duration::from_secs(3600),
      online: Arc::new(AtomicBool::new(true)),
      sync_delay: Arc::new(Mutex::new(None))
    }
  }

  /// Set the `sync()` result.
  pub fn set_sync_result(&self, result: SyncResult) {
    *self.sync_result.lock().expect("lock") = result;
  }

  /// Set the `sync()` error.
  pub fn set_sync_error(&self, msg: &str) {
    *self.sync_error.lock().expect("lock") = Some(msg.to_string());
  }

  /// Set the changes returned by `poll_remote()`.
  pub fn set_poll_result(&self, changes: Vec<RemoteChange>) {
    *self.poll_result.lock().expect("lock") = changes;
  }

  /// Set the `poll_remote()` error.
  pub fn set_poll_error(&self, msg: &str) {
    *self.poll_error.lock().expect("lock") = Some(msg.to_string());
  }

  /// Set the delay before returning from `sync()`.
  pub fn set_sync_delay(&self, delay: Duration) {
    *self.sync_delay.lock().expect("lock") = Some(delay);
  }

  /// Clear the `sync()` delay.
  pub fn clear_sync_delay(&self) {
    *self.sync_delay.lock().expect("lock") = None;
  }

  /// Number of `sync()` calls.
  pub fn sync_call_count(&self) -> usize {
    self.sync_calls.lock().expect("lock").len()
  }

  /// Number of `poll_remote()` calls.
  pub fn poll_call_count(&self) -> usize {
    self.poll_calls.load(Ordering::Relaxed)
  }

  /// Last set of files passed to `sync()`.
  pub fn last_sync_files(&self) -> Option<Vec<PathBuf>> {
    self.sync_calls.lock().expect("lock").last().cloned()
  }
}

impl Backend for MockBackend {
  async fn init(&self, _local_dir: &Path) -> anyhow::Result<InitResult> {
    Ok(InitResult::UpToDate)
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    self
      .sync_calls
      .lock()
      .expect("lock")
      .push(dirty_files.to_vec());

    // Simulate sync delay (for race condition testing)
    let delay = *self.sync_delay.lock().expect("lock");
    if let Some(d) = delay {
      tokio::time::sleep(d).await;
    }

    let maybe_err = self
      .sync_error
      .lock()
      .expect("lock")
      .as_ref()
      .map(ToString::to_string);
    if let Some(msg) = maybe_err {
      return Err(anyhow::anyhow!("{msg}"));
    }

    Ok(self.sync_result.lock().expect("lock").clone())
  }

  async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
    self.poll_calls.fetch_add(1, Ordering::Relaxed);

    if let Some(ref msg) = *self.poll_error.lock().expect("lock") {
      return Err(anyhow::anyhow!("{msg}"));
    }

    Ok(self.poll_result.lock().expect("lock").clone())
  }

  async fn apply_remote(&self, changes: Vec<RemoteChange>) -> anyhow::Result<()> {
    self
      .apply_calls
      .lock()
      .expect("lock")
      .push(changes);
    Ok(())
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
}

/// Test event handler that records all calls.
pub struct TestEventHandler {
  /// Recorded `on_push(count)` calls.
  pub push_calls: Arc<Mutex<Vec<usize>>>,
  /// Recorded `on_sync(result)` calls.
  pub sync_calls: Arc<Mutex<Vec<String>>>,
  /// Recorded `on_log(level, message)` calls.
  pub log_calls: Arc<Mutex<Vec<(LogLevel, String)>>>,
  /// Recorded `on_file_written(path, bytes)` calls.
  pub written_calls: Arc<Mutex<Vec<(PathBuf, usize)>>>,
  /// Recorded `on_file_created(path)` calls.
  pub created_calls: Arc<Mutex<Vec<PathBuf>>>,
  /// Recorded `on_file_deleted(path)` calls.
  pub deleted_calls: Arc<Mutex<Vec<PathBuf>>>,
  /// Recorded `on_file_renamed(old, new)` calls.
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
  pub fn push_count(&self) -> usize {
    self.push_calls.lock().expect("lock").len()
  }

  /// Number of log entries at a given level.
  pub fn log_count(&self, level: LogLevel) -> usize {
    self
      .log_calls
      .lock()
      .expect("lock")
      .iter()
      .filter(|(l, _)| *l == level)
      .count()
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
  let result = tokio::time::timeout(TEST_TIMEOUT, f).await.unwrap_or_else(
    |_| {
      panic!(
        "[TEST] {test_name} timed out after {TEST_TIMEOUT:?} — possible deadlock"
      )
    }
  );
  eprintln!("[TEST] Completed: {test_name}");
  result
}

impl VfsEventHandler for TestEventHandler {
  fn on_push(&self, items_count: usize) {
    self.push_calls.lock().expect("lock").push(items_count);
  }

  fn on_sync(&self, result: &str) {
    self
      .sync_calls
      .lock()
      .expect("lock")
      .push(result.to_string());
  }

  fn on_log(&self, level: LogLevel, message: &str) {
    self
      .log_calls
      .lock()
      .expect("lock")
      .push((level, message.to_string()));
  }

  fn on_file_written(&self, path: &Path, bytes: usize) {
    self
      .written_calls
      .lock()
      .expect("lock")
      .push((path.to_path_buf(), bytes));
  }

  fn on_file_created(&self, path: &Path) {
    self
      .created_calls
      .lock()
      .expect("lock")
      .push(path.to_path_buf());
  }

  fn on_file_deleted(&self, path: &Path) {
    self
      .deleted_calls
      .lock()
      .expect("lock")
      .push(path.to_path_buf());
  }

  fn on_file_renamed(&self, old_path: &Path, new_path: &Path) {
    self
      .renamed_calls
      .lock()
      .expect("lock")
      .push((old_path.to_path_buf(), new_path.to_path_buf()));
  }
}
