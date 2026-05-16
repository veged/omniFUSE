//! Newline-delimited JSON client used by the CLI to talk to a running daemon.

use std::{
  io,
  path::{Path, PathBuf},
  sync::atomic::{AtomicU64, Ordering},
  time::Duration
};

use anyhow::Context as _;
use serde_json::Value;
use tokio::{
  io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
  net::UnixStream,
  time::timeout
};

use super::protocol::{AckResult, ListResult, PROTOCOL_VERSION, Request, Response, StatusResult};

/// Default timeout for a single request/response round trip.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Daemon client tied to a single connection.
pub struct DaemonClient {
  stream: UnixStream,
  next_id: AtomicU64
}

impl DaemonClient {
  /// Connect to the daemon at `socket`.
  ///
  /// # Errors
  ///
  /// Returns an error if the socket cannot be connected.
  pub async fn connect(socket: &Path) -> anyhow::Result<Self> {
    let stream = UnixStream::connect(socket)
      .await
      .with_context(|| format!("connecting to daemon at {}", socket.display()))?;
    Ok(Self {
      stream,
      next_id: AtomicU64::new(1)
    })
  }

  async fn call(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
    let id = self.next_id.fetch_add(1, Ordering::SeqCst);
    let request = Request::new(id, method, params);
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    self.stream.write_all(line.as_bytes()).await?;
    self.stream.flush().await?;

    let mut reader = BufReader::new(&mut self.stream);
    let mut buffer = String::new();
    let bytes = timeout(DEFAULT_TIMEOUT, reader.read_line(&mut buffer))
      .await
      .map_err(|_| anyhow::anyhow!("daemon did not respond within {DEFAULT_TIMEOUT:?}"))??;
    if bytes == 0 {
      anyhow::bail!("daemon closed the connection");
    }
    let response: Response = serde_json::from_str(buffer.trim_end_matches(['\r', '\n']))?;
    if response.v != PROTOCOL_VERSION {
      anyhow::bail!(
        "protocol version mismatch: daemon v={}, client v={PROTOCOL_VERSION}",
        response.v
      );
    }
    if let Some(error) = response.error {
      anyhow::bail!("{}", error.message);
    }
    response
      .result
      .ok_or_else(|| anyhow::anyhow!("daemon response had neither result nor error"))
  }

  /// Send `list` and decode the response.
  ///
  /// # Errors
  ///
  /// Propagates IO and protocol errors.
  pub async fn list(&mut self) -> anyhow::Result<ListResult> {
    let value = self.call("list", None).await?;
    Ok(serde_json::from_value(value)?)
  }

  /// Send `status` for a mount point.
  ///
  /// # Errors
  ///
  /// Propagates IO and protocol errors.
  pub async fn status(&mut self, mount_point: &Path) -> anyhow::Result<StatusResult> {
    let value = self
      .call(
        "status",
        Some(serde_json::json!({ "mountpoint": mount_point.display().to_string() }))
      )
      .await?;
    Ok(serde_json::from_value(value)?)
  }

  /// Send `unmount` for a mount point.
  ///
  /// # Errors
  ///
  /// Propagates IO and protocol errors.
  pub async fn unmount(&mut self, mount_point: &Path) -> anyhow::Result<AckResult> {
    let value = self
      .call(
        "unmount",
        Some(serde_json::json!({ "mountpoint": mount_point.display().to_string() }))
      )
      .await?;
    Ok(serde_json::from_value(value)?)
  }

  /// Request a `mount.<backend>` operation.
  ///
  /// # Errors
  ///
  /// Propagates IO and protocol errors.
  pub async fn mount(&mut self, backend: &str, args: Value) -> anyhow::Result<AckResult> {
    let method = format!("mount.{backend}");
    let value = self.call(&method, Some(args)).await?;
    Ok(serde_json::from_value(value)?)
  }
}

/// Attempt to connect to a running daemon. Returns `Ok(None)` if no daemon is reachable.
///
/// # Errors
///
/// Returns a hard error only for unexpected IO failures (other than the socket
/// being absent or unreachable).
pub async fn try_connect(socket: &Path) -> io::Result<Option<DaemonClient>> {
  match UnixStream::connect(socket).await {
    Ok(stream) => Ok(Some(DaemonClient {
      stream,
      next_id: AtomicU64::new(1)
    })),
    Err(error) if error.kind() == io::ErrorKind::NotFound || error.kind() == io::ErrorKind::ConnectionRefused => {
      Ok(None)
    }
    Err(error) => Err(error)
  }
}

/// Resolve the socket path from the standard daemon-paths layout.
///
/// # Errors
///
/// Returns the resolution error from [`DaemonPaths::resolve`](super::paths::DaemonPaths::resolve).
pub fn default_socket() -> anyhow::Result<PathBuf> {
  let paths = super::paths::DaemonPaths::resolve(&crate::StdMountEnvironment)?;
  Ok(paths.socket)
}
