//! Observability primitives and shared logging initialization.

use std::{
  fs::{File, OpenOptions},
  io,
  path::{Path, PathBuf},
  process,
  sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering}
  },
  time::{Instant, SystemTime, UNIX_EPOCH}
};

use serde::Serialize;
use tracing_subscriber::{
  EnvFilter,
  fmt::writer::{BoxMakeWriter, MakeWriter}
};

use crate::LoggingConfig;

static NEXT_MOUNT_ID: AtomicU64 = AtomicU64::new(1);
static LOGGING_INITIALIZED: OnceLock<()> = OnceLock::new();

/// Operation kind for correlated runtime events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
  /// Mount lifecycle.
  Mount,
  /// Backend initialization.
  Init,
  /// Synchronization of dirty files.
  Sync,
  /// Remote polling.
  Poll,
  /// Applying remote changes.
  ApplyRemote,
  /// File lifecycle operation.
  File
}

/// Event severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSeverity {
  /// Debug-only diagnostic.
  Debug,
  /// Informational event.
  Info,
  /// Warning event.
  Warn,
  /// Error event.
  Error
}

/// Runtime outcome for an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
  /// Operation completed successfully.
  Success,
  /// Operation completed with conflicts.
  Conflict,
  /// Operation deferred because backend is offline.
  Deferred,
  /// Operation failed.
  Failure
}

/// Stable semantic classification for failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
  /// Backend or network is unavailable.
  Offline,
  /// Operation timed out.
  Timeout,
  /// Authentication failed.
  AuthFailed,
  /// Access denied by the remote or filesystem.
  PermissionDenied,
  /// Requested entity not found.
  NotFound,
  /// Merge or write conflict.
  Conflict,
  /// Remote rate limiting.
  RateLimited,
  /// Invalid configuration.
  InvalidConfig,
  /// Invalid user input.
  InvalidInput,
  /// Filesystem I/O failure.
  FsIo,
  /// Backend command failure.
  BackendCommandFailed,
  /// Remote protocol or API contract violation.
  ProtocolViolation,
  /// Internal error.
  Internal
}

/// Origin layer for an operational event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSource {
  /// Mount orchestration.
  Mount,
  /// VFS layer.
  Vfs,
  /// Sync engine.
  SyncEngine,
  /// Git backend.
  Git,
  /// Wiki backend.
  Wiki,
  /// FUSE / WinFsp adapter.
  Fuse,
  /// GUI / Tauri bridge.
  Gui
}

/// Recommended action for the caller or UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
  /// Safe to retry manually.
  Retryable,
  /// The system is already retrying automatically.
  AutoRetrying,
  /// User action is required.
  UserActionRequired,
  /// Fatal failure.
  Fatal
}

/// Correlated runtime session for a single mount.
#[derive(Debug, Clone)]
pub struct ObservabilitySession {
  mount_id: String,
  backend: &'static str,
  mount_point: PathBuf,
  local_dir: PathBuf,
  session_started_at_ms: u128,
  op_seq: Arc<AtomicU64>
}

impl ObservabilitySession {
  /// Create a new correlated session.
  #[must_use]
  pub fn new(backend: &'static str, mount_point: PathBuf, local_dir: PathBuf) -> Self {
    let seq = NEXT_MOUNT_ID.fetch_add(1, Ordering::Relaxed);
    let now_ms = now_unix_ms();
    let mount_id = format!("m{now_ms:x}-{:x}-{:x}", process::id(), seq);

    Self {
      mount_id,
      backend,
      mount_point,
      local_dir,
      session_started_at_ms: now_ms,
      op_seq: Arc::new(AtomicU64::new(0))
    }
  }

  /// Mount id.
  #[must_use]
  pub fn mount_id(&self) -> &str {
    &self.mount_id
  }

  /// Backend name.
  #[must_use]
  pub fn backend(&self) -> &'static str {
    self.backend
  }

  /// Mount point.
  #[must_use]
  pub fn mount_point(&self) -> &Path {
    &self.mount_point
  }

  /// Local working directory.
  #[must_use]
  pub fn local_dir(&self) -> &Path {
    &self.local_dir
  }

  /// Session start timestamp in milliseconds since epoch.
  #[must_use]
  pub const fn session_started_at_ms(&self) -> u128 {
    self.session_started_at_ms
  }

  /// Start a correlated operation.
  #[must_use]
  pub fn start_operation(&self, kind: OperationKind, attempt: u32, path: Option<PathBuf>) -> OperationContext {
    let op_id = self.op_seq.fetch_add(1, Ordering::Relaxed) + 1;

    OperationContext {
      mount_id: self.mount_id.clone(),
      op_id,
      kind,
      attempt,
      path,
      started_at_ms: now_unix_ms(),
      started_at: Instant::now()
    }
  }
}

/// Correlated context for a single operation.
#[derive(Debug, Clone, Serialize)]
pub struct OperationContext {
  mount_id: String,
  op_id: u64,
  kind: OperationKind,
  attempt: u32,
  path: Option<PathBuf>,
  started_at_ms: u128,
  #[serde(skip_serializing)]
  started_at: Instant
}

impl OperationContext {
  /// Mount id for this operation.
  #[must_use]
  pub fn mount_id(&self) -> &str {
    &self.mount_id
  }

  /// Monotonic operation id within the mount session.
  #[must_use]
  pub const fn op_id(&self) -> u64 {
    self.op_id
  }

  /// Operation kind.
  #[must_use]
  pub const fn kind(&self) -> OperationKind {
    self.kind
  }

  /// Retry or execution attempt.
  #[must_use]
  pub const fn attempt(&self) -> u32 {
    self.attempt
  }

  /// Optional path attached to the operation.
  #[must_use]
  pub fn path(&self) -> Option<&Path> {
    self.path.as_deref()
  }

  /// Operation start timestamp in milliseconds since epoch.
  #[must_use]
  pub const fn started_at_ms(&self) -> u128 {
    self.started_at_ms
  }

  /// Elapsed time in milliseconds.
  #[must_use]
  pub fn elapsed_ms(&self) -> u64 {
    elapsed_ms(self.started_at)
  }
}

/// Structured operational event for logs, UI and tests.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum OperationalEvent {
  /// Mount orchestration started.
  MountStarted {
    /// Correlated context.
    context: OperationContext,
    /// Backend name.
    backend: String,
    /// Mount point.
    mount_point: PathBuf,
    /// Local directory.
    local_dir: PathBuf
  },
  /// Mount orchestration completed.
  MountFinished {
    /// Correlated context.
    context: OperationContext,
    /// Outcome.
    outcome: OperationOutcome,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Backend initialization started.
  BackendInitStarted {
    /// Correlated context.
    context: OperationContext,
    /// Backend name.
    backend: String
  },
  /// Backend initialization finished.
  BackendInitFinished {
    /// Correlated context.
    context: OperationContext,
    /// Outcome.
    outcome: OperationOutcome,
    /// Backend name.
    backend: String,
    /// Human-readable details.
    details: Option<String>,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Dirty sync started.
  SyncStarted {
    /// Correlated context.
    context: OperationContext,
    /// Number of dirty files.
    dirty_count: usize
  },
  /// Dirty sync finished.
  SyncFinished {
    /// Correlated context.
    context: OperationContext,
    /// Outcome.
    outcome: OperationOutcome,
    /// Number of synced files.
    synced_files: usize,
    /// Number of conflict files.
    conflict_files: usize,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Explicit conflict marker.
  ConflictDetected {
    /// Correlated context.
    context: OperationContext,
    /// Number of conflict files.
    conflict_files: usize
  },
  /// Remote poll started.
  RemotePollStarted {
    /// Correlated context.
    context: OperationContext,
    /// Poll interval in milliseconds.
    interval_ms: u64
  },
  /// Remote poll detected incoming changes.
  RemoteChangesDetected {
    /// Correlated context.
    context: OperationContext,
    /// Number of changed files.
    changed_files: usize
  },
  /// Remote poll failed.
  RemotePollFailed {
    /// Correlated context.
    context: OperationContext,
    /// Failure severity.
    severity: EventSeverity,
    /// Failure kind.
    error_kind: ErrorKind,
    /// Human-readable message.
    message: String,
    /// Recommended disposition.
    disposition: Disposition,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Applying remote changes failed.
  RemoteApplyFailed {
    /// Correlated context.
    context: OperationContext,
    /// Failure severity.
    severity: EventSeverity,
    /// Failure kind.
    error_kind: ErrorKind,
    /// Human-readable message.
    message: String,
    /// Recommended disposition.
    disposition: Disposition,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Slow operation marker.
  SlowOperationDetected {
    /// Correlated context.
    context: OperationContext,
    /// Elapsed milliseconds.
    elapsed_ms: u64,
    /// Warning severity.
    severity: EventSeverity
  },
  /// A file became dirty.
  FileMarkedDirty {
    /// Correlated context.
    context: OperationContext,
    /// Relative path.
    path: PathBuf
  },
  /// A file was flushed.
  FileFlushed {
    /// Correlated context.
    context: OperationContext,
    /// Relative path.
    path: PathBuf
  },
  /// Push was rejected.
  PushRejected {
    /// Correlated context.
    context: OperationContext,
    /// Human-readable message.
    message: String
  },
  /// Mount orchestration failed.
  MountFailed {
    /// Correlated context.
    context: OperationContext,
    /// Failure severity.
    severity: EventSeverity,
    /// Failure kind.
    error_kind: ErrorKind,
    /// Origin.
    source: ErrorSource,
    /// Recommended disposition.
    disposition: Disposition,
    /// Human-readable message.
    message: String,
    /// Elapsed milliseconds.
    elapsed_ms: u64
  },
  /// Generic warning for UI/log bridge.
  UserVisibleWarning {
    /// Correlated context.
    context: OperationContext,
    /// Human-readable message.
    message: String,
    /// Origin.
    source: ErrorSource,
    /// Recommended disposition.
    disposition: Disposition
  }
}

/// Sink for operational events.
pub trait OperationalEventSink: Send + Sync + 'static {
  /// Emit an operational event.
  fn emit(&self, event: OperationalEvent);
}

/// No-op operational event sink.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopOperationalEventSink;

impl OperationalEventSink for NoopOperationalEventSink {
  fn emit(&self, _event: OperationalEvent) {}
}

/// Test-friendly sink that records all operational events in memory.
#[derive(Debug, Default)]
pub struct RecordingEventSink {
  events: Mutex<Vec<OperationalEvent>>
}

impl RecordingEventSink {
  /// Recorded events snapshot.
  #[must_use]
  pub fn snapshot(&self) -> Vec<OperationalEvent> {
    self.events.lock().map_or_else(|_| Vec::new(), |events| events.clone())
  }
}

impl OperationalEventSink for RecordingEventSink {
  fn emit(&self, event: OperationalEvent) {
    if let Ok(mut events) = self.events.lock() {
      events.push(event);
    }
  }
}

/// Initialize shared logging for CLI and GUI.
///
/// Repeated calls are no-ops once a global subscriber has been installed.
///
/// # Errors
///
/// Returns an error if file sink initialization or subscriber installation fails.
pub fn init_logging(config: &LoggingConfig) -> anyhow::Result<()> {
  if LOGGING_INITIALIZED.get().is_some() {
    return Ok(());
  }

  let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(config.filter_directive()));

  let writer = make_writer(config)?;
  tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_writer(writer)
    .with_target(false)
    .compact()
    .try_init()
    .map_err(|error| anyhow::anyhow!("failed to initialize tracing subscriber: {error}"))?;

  let _ = LOGGING_INITIALIZED.set(());
  Ok(())
}

fn make_writer(config: &LoggingConfig) -> anyhow::Result<BoxMakeWriter> {
  match &config.log_file {
    Some(path) => Ok(BoxMakeWriter::new(SharedFileMakeWriter::new(path)?)),
    None => Ok(BoxMakeWriter::new(io::stderr))
  }
}

fn now_unix_ms() -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis()
}

fn elapsed_ms(started_at: Instant) -> u64 {
  let elapsed = started_at.elapsed();
  elapsed
    .as_secs()
    .saturating_mul(1000)
    .saturating_add(u64::from(elapsed.subsec_millis()))
}

#[derive(Clone, Debug)]
struct SharedFileMakeWriter {
  file: Arc<Mutex<File>>
}

impl SharedFileMakeWriter {
  fn new(path: &Path) -> anyhow::Result<Self> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Self {
      file: Arc::new(Mutex::new(file))
    })
  }
}

impl<'a> MakeWriter<'a> for SharedFileMakeWriter {
  type Writer = SharedFileWriter;

  fn make_writer(&'a self) -> Self::Writer {
    SharedFileWriter {
      file: Arc::clone(&self.file)
    }
  }
}

struct SharedFileWriter {
  file: Arc<Mutex<File>>
}

impl io::Write for SharedFileWriter {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let mut file = self
      .file
      .lock()
      .map_err(|_| io::Error::other("log file lock poisoned"))?;
    io::Write::write(&mut *file, buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    let mut file = self
      .file
      .lock()
      .map_err(|_| io::Error::other("log file lock poisoned"))?;
    io::Write::flush(&mut *file)
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::expect_used)]

  use std::fs;

  use tempfile::tempdir;

  use super::*;

  #[test]
  fn test_file_sink_writes_problem_log_records() {
    let temp = tempdir().expect("create temp dir");
    let log_path = temp.path().join("logs").join("omnifuse.log");
    let config = LoggingConfig {
      level: "warn".to_string(),
      log_file: Some(log_path.clone())
    };

    let writer = make_writer(&config).expect("create file writer");
    let subscriber = tracing_subscriber::fmt()
      .with_env_filter(EnvFilter::new(config.filter_directive()))
      .with_writer(writer)
      .with_target(false)
      .compact()
      .finish();

    tracing::subscriber::with_default(subscriber, || {
      tracing::warn!(error_kind = "offline", op = "poll_remote", "poll failed");
    });

    let contents = fs::read_to_string(&log_path).expect("read log file");
    assert!(
      contents.contains("poll failed"),
      "expected problem log in file sink, got: {contents}"
    );
    assert!(
      contents.contains("offline"),
      "expected structured fields in file sink, got: {contents}"
    );
    assert!(
      contents.contains("poll_remote"),
      "expected operation name in file sink, got: {contents}"
    );
  }
}
