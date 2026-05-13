//! Product event contract shared by core, GUI, CLI and tests.

use std::{
  path::{Path, PathBuf},
  process,
  sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering}
  },
  time::{Instant, SystemTime, UNIX_EPOCH}
};

use serde::Serialize;
use serde_json::Value;

static NEXT_MOUNT_ID: AtomicU64 = AtomicU64::new(1);

/// Wire contract version.
pub const VERSION: u16 = 1;

/// Product event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
  /// Event contract version.
  pub version: u16,
  /// Monotonic event sequence inside a mount session.
  pub seq: u64,
  /// Unix timestamp in milliseconds.
  pub time: u64,
  /// Mount session id.
  pub mount_id: String,
  /// Optional operation id.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub op_id: Option<u64>,
  /// Semantic event kind.
  pub kind: Kind,
  /// User-facing severity.
  pub level: Level,
  /// Source subsystem.
  pub source: Source,
  /// Small structured payload.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub data: Option<Value>,
  /// Error details for failed/deferred events.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<Error>
}

impl Event {
  /// Attach structured payload.
  #[must_use]
  pub fn data(mut self, data: Value) -> Self {
    self.data = Some(data);
    self
  }

  /// Attach error details.
  #[must_use]
  pub fn error(mut self, error: Error) -> Self {
    self.error = Some(error);
    self
  }
}

/// Stable event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Kind {
  /// Mount orchestration started.
  #[serde(rename = "mount.start")]
  MountStart,
  /// Mount is available for user I/O.
  #[serde(rename = "mount.ready")]
  MountReady,
  /// Mount stopped.
  #[serde(rename = "mount.stop")]
  MountStop,
  /// Mount failed.
  #[serde(rename = "mount.fail")]
  MountFail,
  /// File changed.
  #[serde(rename = "file.change")]
  FileChange,
  /// File flushed.
  #[serde(rename = "file.flush")]
  FileFlush,
  /// File created.
  #[serde(rename = "file.create")]
  FileCreate,
  /// File deleted.
  #[serde(rename = "file.delete")]
  FileDelete,
  /// File renamed.
  #[serde(rename = "file.rename")]
  FileRename,
  /// Dirty sync started.
  #[serde(rename = "sync.start")]
  SyncStart,
  /// Dirty sync completed.
  #[serde(rename = "sync.done")]
  SyncDone,
  /// Dirty sync deferred.
  #[serde(rename = "sync.defer")]
  SyncDefer,
  /// Dirty sync failed.
  #[serde(rename = "sync.fail")]
  SyncFail,
  /// Remote poll executed.
  #[serde(rename = "remote.poll")]
  RemotePoll,
  /// Remote changes were detected/applied.
  #[serde(rename = "remote.change")]
  RemoteChange,
  /// Remote apply step executed.
  #[serde(rename = "remote.apply")]
  RemoteApply,
  /// Remote work was deferred.
  #[serde(rename = "remote.defer")]
  RemoteDefer,
  /// Remote work failed.
  #[serde(rename = "remote.fail")]
  RemoteFail,
  /// Conflict detected.
  #[serde(rename = "conflict")]
  Conflict,
  /// Internal queue dropped work.
  #[serde(rename = "queue.drop")]
  QueueDrop,
  /// Authentication failed.
  #[serde(rename = "auth.fail")]
  AuthFail
}

/// User-facing severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
  /// Informational event.
  Info,
  /// Recoverable problem.
  Warn,
  /// Failed operation.
  Error
}

/// Event source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
  /// Mount orchestration.
  Mount,
  /// VFS layer.
  Vfs,
  /// Sync engine.
  Sync,
  /// Git backend.
  Git,
  /// Wiki backend.
  Wiki,
  /// FUSE / `WinFsp` adapter.
  Fuse,
  /// GUI / Tauri bridge.
  Gui
}

/// Stable error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
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

/// Suggested user/system action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
  /// Manual retry is reasonable.
  Retry,
  /// The system is already waiting/retrying.
  Wait,
  /// User must fix configuration, auth or input.
  Fix,
  /// Current operation cannot continue.
  Stop
}

/// Error details attached to failed/deferred events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
  /// Stable machine-readable code.
  pub code: Code,
  /// Human-readable diagnostic message.
  pub message: String,
  /// Suggested action.
  pub action: Action
}

impl Error {
  /// Create event error details.
  #[must_use]
  pub fn new(message: impl Into<String>, code: Code, action: Action) -> Self {
    Self {
      code,
      message: message.into(),
      action
    }
  }
}

/// Correlated mount session.
#[derive(Debug, Clone)]
pub struct Session {
  mount_id: String,
  backend: &'static str,
  mount_point: PathBuf,
  local_dir: PathBuf,
  started_at_ms: u64,
  event_seq: Arc<AtomicU64>,
  op_seq: Arc<AtomicU64>
}

impl Session {
  /// Create a mount session.
  #[must_use]
  pub fn new(backend: &'static str, mount_point: PathBuf, local_dir: PathBuf) -> Self {
    let seq = NEXT_MOUNT_ID.fetch_add(1, Ordering::Relaxed);
    let now = now_ms();
    let mount_id = format!("m{now:x}-{:x}-{seq:x}", process::id());

    Self {
      mount_id,
      backend,
      mount_point,
      local_dir,
      started_at_ms: now,
      event_seq: Arc::new(AtomicU64::new(0)),
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
  pub const fn backend(&self) -> &'static str {
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
  pub const fn started_at_ms(&self) -> u64 {
    self.started_at_ms
  }

  /// Start a correlated operation.
  #[must_use]
  pub fn op(&self) -> Op {
    Op {
      id: self.op_seq.fetch_add(1, Ordering::Relaxed) + 1,
      started_at: Instant::now()
    }
  }

  /// Create a session-level event.
  #[must_use]
  pub fn event(&self, kind: Kind, level: Level, source: Source) -> Event {
    self.make_event(None, kind, level, source)
  }

  /// Create an operation-level event.
  #[must_use]
  pub fn op_event(&self, op: &Op, kind: Kind, level: Level, source: Source) -> Event {
    self.make_event(Some(op.id), kind, level, source)
  }

  fn make_event(&self, op_id: Option<u64>, kind: Kind, level: Level, source: Source) -> Event {
    Event {
      version: VERSION,
      seq: self.event_seq.fetch_add(1, Ordering::Relaxed) + 1,
      time: now_ms(),
      mount_id: self.mount_id.clone(),
      op_id,
      kind,
      level,
      source,
      data: None,
      error: None
    }
  }
}

/// Correlated operation.
#[derive(Debug, Clone)]
pub struct Op {
  id: u64,
  started_at: Instant
}

impl Op {
  /// Operation id.
  #[must_use]
  pub const fn id(&self) -> u64 {
    self.id
  }

  /// Elapsed time in milliseconds.
  #[must_use]
  pub fn elapsed_ms(&self) -> u64 {
    elapsed_ms(self.started_at)
  }
}

/// Event sink.
pub trait Sink: Send + Sync + 'static {
  /// Emit an event.
  fn emit(&self, event: Event);
}

/// No-op event sink.
pub struct NoopSink;

impl Sink for NoopSink {
  fn emit(&self, _event: Event) {}
}

/// Test-friendly sink that records all events in memory.
#[derive(Debug, Default)]
pub struct RecordSink {
  events: Mutex<Vec<Event>>
}

impl RecordSink {
  /// Recorded events snapshot.
  #[must_use]
  pub fn snapshot(&self) -> Vec<Event> {
    self.events.lock().map_or_else(|_| Vec::new(), |events| events.clone())
  }
}

impl Sink for RecordSink {
  fn emit(&self, event: Event) {
    if let Ok(mut events) = self.events.lock() {
      events.push(event);
    }
  }
}

fn now_ms() -> u64 {
  let millis = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis();
  millis.try_into().unwrap_or(u64::MAX)
}

fn elapsed_ms(started_at: Instant) -> u64 {
  let elapsed = started_at.elapsed();
  elapsed
    .as_secs()
    .saturating_mul(1000)
    .saturating_add(u64::from(elapsed.subsec_millis()))
}
