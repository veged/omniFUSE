//! Daemon process: persistent cache + hot-tier sharing + UDS server.
//!
//! The daemon owns a single [`FilesystemCache`] and a pool of `FileBufferManager`s
//! keyed by `InstanceHash`, so concurrent mounts of the same instance share both
//! the on-disk and the in-memory caches.

use std::{
  path::{Path, PathBuf},
  sync::Arc,
  time::Instant
};

use anyhow::Context as _;
use dashmap::DashMap;
use omnifuse_core::{BufferConfig, FileBufferManager, FilesystemCache, FilesystemCacheConfig, InstanceHash, NoopSink};
use omnifuse_git::GitBackend;
use serde_json::Value;
use tokio::{
  io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
  net::{UnixListener, UnixStream},
  task::JoinHandle
};
use tracing::{info, warn};

use super::{
  paths::DaemonPaths,
  protocol::{AckResult, ListResult, MountSummary, PROTOCOL_VERSION, Request, Response, StatusResult}
};
use crate::{
  GitMountArgs, MountService, S3MountArgs, StdMountEnvironment, WikiMountArgs, mount_service::PreparedMount
};

/// Active mount tracked by the daemon.
struct ActiveMount {
  backend: String,
  instance: InstanceHash,
  mount_point: PathBuf,
  started_at: Instant,
  /// Held to keep the hosting task alive for the lifetime of the entry. We never
  /// `await` it: cleanup happens when the task observes an unmount and removes
  /// its own entry from the registry.
  _task: JoinHandle<()>
}

/// Daemon state.
pub struct Daemon {
  cache: Arc<FilesystemCache>,
  buffer_managers: DashMap<InstanceHash, Arc<FileBufferManager>>,
  mounts: DashMap<PathBuf, ActiveMount>,
  service: MountService<StdMountEnvironment>
}

impl Daemon {
  /// Build a new daemon with an explicit persistent cache.
  #[must_use]
  pub fn new(cache: Arc<FilesystemCache>) -> Arc<Self> {
    let service = MountService::default().with_cache(Arc::clone(&cache));
    Arc::new(Self {
      cache,
      buffer_managers: DashMap::new(),
      mounts: DashMap::new(),
      service
    })
  }

  /// Build a daemon with a cache opened under the user's cache directory.
  ///
  /// # Errors
  ///
  /// Returns an error if the cache cannot be opened.
  pub fn with_default_cache() -> anyhow::Result<Arc<Self>> {
    let env = StdMountEnvironment;
    let cache_root = crate::MountEnvironment::cache_base_dir(&env)?
      .join("omnifuse")
      .join("cache");
    let cache = FilesystemCache::open(cache_root, FilesystemCacheConfig::from_env())?;
    Ok(Self::new(cache))
  }

  /// Borrow the underlying cache (mainly for tests).
  #[must_use]
  pub const fn cache(&self) -> &Arc<FilesystemCache> {
    &self.cache
  }

  /// Look up or create the shared `FileBufferManager` for an instance.
  ///
  /// Two concurrent mounts of the same `instance` see the same in-memory hot
  /// tier — that is the V1 hot-tier sharing model.
  fn shared_buffer_manager(&self, instance: &InstanceHash, config: &BufferConfig) -> Arc<FileBufferManager> {
    if let Some(existing) = self.buffer_managers.get(instance) {
      return Arc::clone(existing.value());
    }
    let manager = Arc::new(FileBufferManager::new(config.clone()));
    self.buffer_managers.insert(instance.clone(), Arc::clone(&manager));
    manager
  }

  fn drop_buffer_manager_if_idle(&self, instance: &InstanceHash) {
    let still_used = self.mounts.iter().any(|entry| entry.value().instance == *instance);
    if !still_used {
      self.buffer_managers.remove(instance);
    }
  }

  /// Return the list of active mounts as a protocol payload.
  #[must_use]
  pub fn list_mounts(&self) -> ListResult {
    let mounts = self
      .mounts
      .iter()
      .map(|entry| MountSummary {
        backend: entry.value().backend.clone(),
        instance: entry.value().instance.as_str().to_string(),
        mount_point: entry.value().mount_point.clone(),
        age_secs: entry.value().started_at.elapsed().as_secs()
      })
      .collect();
    ListResult { mounts }
  }

  fn mount_summary(&self, mount_point: &Path) -> Option<MountSummary> {
    self.mounts.get(mount_point).map(|entry| MountSummary {
      backend: entry.value().backend.clone(),
      instance: entry.value().instance.as_str().to_string(),
      mount_point: entry.value().mount_point.clone(),
      age_secs: entry.value().started_at.elapsed().as_secs()
    })
  }

  /// Mount an S3 instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the mount is already active or preparation fails.
  pub async fn mount_s3(self: &Arc<Self>, args: S3MountArgs) -> anyhow::Result<PathBuf> {
    let prepared = self.service.prepare_s3(args)?;
    let instance = prepared.backend.config().instance_hash();
    self.start_mount("s3", instance, prepared).await
  }

  /// Mount a Wiki instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the mount is already active or preparation fails.
  pub async fn mount_wiki(self: &Arc<Self>, args: WikiMountArgs) -> anyhow::Result<PathBuf> {
    let prepared = self.service.prepare_wiki(args)?;
    let instance = prepared.backend.config().instance_hash();
    self.start_mount("wiki", instance, prepared).await
  }

  /// Mount a Git instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the mount is already active or preparation fails.
  pub async fn mount_git(self: &Arc<Self>, args: GitMountArgs) -> anyhow::Result<PathBuf> {
    let prepared = self.service.prepare_git(args)?;
    let instance = git_instance_hash(&prepared);
    self.start_mount("git", instance, prepared).await
  }

  #[allow(clippy::unused_async)]
  async fn start_mount<B>(
    self: &Arc<Self>,
    backend_name: &'static str,
    instance: InstanceHash,
    prepared: PreparedMount<B>
  ) -> anyhow::Result<PathBuf>
  where
    B: omnifuse_core::Backend
  {
    let mount_point = prepared.config.mount_point.clone();
    if self.mounts.contains_key(&mount_point) {
      anyhow::bail!("mount already active at {}", mount_point.display());
    }

    let buffer_manager = self.shared_buffer_manager(&instance, &prepared.config.buffer);
    let this = Arc::clone(self);
    let mount_point_for_task = mount_point.clone();
    let instance_for_task = instance.clone();

    let task = tokio::spawn(async move {
      let result =
        omnifuse_core::run_mount_with_buffer(prepared.config, prepared.backend, NoopSink, Some(buffer_manager)).await;
      if let Err(error) = result {
        warn!(mount_point = %mount_point_for_task.display(), error = %error, "daemon-hosted mount exited with error");
      } else {
        info!(mount_point = %mount_point_for_task.display(), "daemon-hosted mount stopped");
      }
      this.mounts.remove(&mount_point_for_task);
      this.drop_buffer_manager_if_idle(&instance_for_task);
    });

    self.mounts.insert(
      mount_point.clone(),
      ActiveMount {
        backend: backend_name.to_string(),
        instance,
        mount_point: mount_point.clone(),
        started_at: Instant::now(),
        _task: task
      }
    );

    info!(backend = backend_name, mount_point = %mount_point.display(), "daemon mounted instance");
    Ok(mount_point)
  }

  /// Unmount a mount point.
  ///
  /// # Errors
  ///
  /// Returns an error when the mount point is unknown or the unmount command fails.
  pub async fn unmount(&self, mount_point: &Path) -> anyhow::Result<()> {
    if !self.mounts.contains_key(mount_point) {
      anyhow::bail!("no active mount at {}", mount_point.display());
    }
    super::unmount_filesystem(mount_point).await?;
    // Wait for the hosting task to observe the unmount and clean up — bounded so a stuck
    // task does not hang the daemon.
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while self.mounts.contains_key(mount_point) {
      if Instant::now() >= deadline {
        warn!(mount_point = %mount_point.display(), "unmount cleanup timed out");
        break;
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
  }

  /// Build a `status` payload for a mount.
  #[must_use]
  pub fn status(&self, mount_point: &Path) -> StatusResult {
    StatusResult {
      mount: self.mount_summary(mount_point),
      dirty_files: 0,
      cache_hit_ratio: None
    }
  }

  /// Mount count.
  #[must_use]
  pub fn mount_count(&self) -> usize {
    self.mounts.len()
  }

  /// Gracefully unmount everything.
  ///
  /// # Errors
  ///
  /// Returns the first unmount failure encountered. Subsequent mounts are still attempted.
  pub async fn shutdown(&self) -> anyhow::Result<()> {
    let active: Vec<PathBuf> = self.mounts.iter().map(|entry| entry.key().clone()).collect();
    let mut first_error: Option<anyhow::Error> = None;
    for mp in active {
      if let Err(error) = self.unmount(&mp).await {
        warn!(mount_point = %mp.display(), %error, "shutdown unmount failed");
        first_error.get_or_insert(error);
      }
    }
    first_error.map_or_else(|| Ok(()), Err)
  }
}

fn git_instance_hash(prepared: &PreparedMount<GitBackend>) -> InstanceHash {
  let cfg = prepared.backend.config();
  InstanceHash::from_parts(&["git", &cfg.source, &cfg.branch])
}

/// Run the UDS server, accepting client connections until the shutdown signal fires.
///
/// # Errors
///
/// Returns an error if the listener cannot be bound or the PID file cannot be written.
pub async fn serve(
  paths: DaemonPaths,
  daemon: Arc<Daemon>,
  mut shutdown: tokio::sync::oneshot::Receiver<()>
) -> anyhow::Result<()> {
  paths.ensure_dir()?;
  let _pid_guard = PidFileGuard::write(&paths.pid)?;
  let listener = bind_listener(&paths.socket)?;
  info!(socket = %paths.socket.display(), "daemon listening");

  let serve_loop = async {
    loop {
      match listener.accept().await {
        Ok((stream, _addr)) => {
          let daemon = Arc::clone(&daemon);
          tokio::spawn(async move {
            if let Err(error) = handle_client(stream, &daemon).await {
              warn!(error = %error, "daemon client handler exited with error");
            }
          });
        }
        Err(error) => {
          warn!(%error, "daemon accept failed");
          tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
      }
    }
  };

  tokio::select! {
    () = serve_loop => {},
    _ = &mut shutdown => {
      info!("daemon received shutdown signal");
    }
  }

  if let Err(error) = daemon.shutdown().await {
    warn!(%error, "daemon shutdown encountered errors");
  }
  Ok(())
}

fn bind_listener(socket: &Path) -> anyhow::Result<UnixListener> {
  if socket.exists() {
    let _ = std::fs::remove_file(socket);
  }
  let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
  // Restrict to owner only — the daemon brokers mount operations with the user's credentials.
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
  }
  Ok(listener)
}

/// Best-effort PID file handler. Removes the file on drop.
struct PidFileGuard {
  path: PathBuf
}

impl PidFileGuard {
  fn write(path: &Path) -> anyhow::Result<Self> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, std::process::id().to_string())?;
    Ok(Self {
      path: path.to_path_buf()
    })
  }
}

impl Drop for PidFileGuard {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.path);
  }
}

async fn handle_client(stream: UnixStream, daemon: &Arc<Daemon>) -> anyhow::Result<()> {
  let (read_half, mut write_half) = stream.into_split();
  let mut reader = BufReader::new(read_half);
  let mut line = String::new();

  loop {
    line.clear();
    let bytes = reader.read_line(&mut line).await?;
    if bytes == 0 {
      return Ok(()); // peer closed
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
      continue;
    }
    let response = dispatch(trimmed, daemon).await;
    let mut encoded = serde_json::to_string(&response)?;
    encoded.push('\n');
    write_half.write_all(encoded.as_bytes()).await?;
    write_half.flush().await?;
  }
}

async fn dispatch(line: &str, daemon: &Arc<Daemon>) -> Response {
  let request: Request = match serde_json::from_str(line) {
    Ok(req) => req,
    Err(error) => return Response::err(0, format!("invalid request: {error}"))
  };
  if request.v != PROTOCOL_VERSION {
    return Response::err(
      request.id,
      format!(
        "protocol version mismatch: client v={}, daemon v={PROTOCOL_VERSION}",
        request.v
      )
    );
  }
  let result = handle_method(&request, daemon).await;
  match result {
    Ok(value) => Response::ok(request.id, value),
    Err(error) => Response::err(request.id, error.to_string())
  }
}

async fn handle_method(req: &Request, daemon: &Arc<Daemon>) -> anyhow::Result<Value> {
  match req.method.as_str() {
    "list" => Ok(serde_json::to_value(daemon.list_mounts())?),
    "status" => {
      let params = req.params.clone().unwrap_or_default();
      let mp: PathBuf = params
        .get("mountpoint")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("status requires `mountpoint`"))?;
      Ok(serde_json::to_value(daemon.status(&mp))?)
    }
    "unmount" => {
      let params = req.params.clone().unwrap_or_default();
      let mp: PathBuf = params
        .get("mountpoint")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("unmount requires `mountpoint`"))?;
      daemon.unmount(&mp).await?;
      Ok(serde_json::to_value(AckResult { mount_point: mp })?)
    }
    "mount.s3" => {
      let params = req.params.clone().unwrap_or_default();
      let args: S3MountArgs = serde_json::from_value(params)?;
      let mp = daemon.mount_s3(args).await?;
      Ok(serde_json::to_value(AckResult { mount_point: mp })?)
    }
    "mount.wiki" => {
      let params = req.params.clone().unwrap_or_default();
      let args: WikiMountArgs = serde_json::from_value(params)?;
      let mp = daemon.mount_wiki(args).await?;
      Ok(serde_json::to_value(AckResult { mount_point: mp })?)
    }
    "mount.git" => {
      let params = req.params.clone().unwrap_or_default();
      let args: GitMountArgs = serde_json::from_value(params)?;
      let mp = daemon.mount_git(args).await?;
      Ok(serde_json::to_value(AckResult { mount_point: mp })?)
    }
    other => anyhow::bail!("unknown method `{other}`")
  }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
  use omnifuse_core::FilesystemCacheConfig;
  use tempfile::TempDir;

  use super::*;

  fn make_daemon(dir: &Path) -> Arc<Daemon> {
    let cache = FilesystemCache::open(dir, FilesystemCacheConfig::default()).expect("cache");
    Daemon::new(cache)
  }

  #[test]
  fn shared_buffer_manager_is_same_arc_per_instance() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = make_daemon(dir.path());
    let cfg = BufferConfig::default();

    let i1 = InstanceHash::from_parts(&["test", "alpha"]);
    let a = daemon.shared_buffer_manager(&i1, &cfg);
    let b = daemon.shared_buffer_manager(&i1, &cfg);
    assert!(Arc::ptr_eq(&a, &b), "same instance must share the buffer manager");

    let i2 = InstanceHash::from_parts(&["test", "beta"]);
    let c = daemon.shared_buffer_manager(&i2, &cfg);
    assert!(
      !Arc::ptr_eq(&a, &c),
      "different instances must get distinct buffer managers"
    );
  }

  #[tokio::test]
  async fn dispatch_rejects_unknown_method() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = make_daemon(dir.path());
    let line = serde_json::to_string(&Request::new(42, "bogus.method", None)).expect("encode");
    let response = dispatch(&line, &daemon).await;
    assert_eq!(response.id, 42);
    assert!(response.error.is_some());
  }

  #[tokio::test]
  async fn dispatch_rejects_wrong_protocol_version() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = make_daemon(dir.path());
    let mut req = Request::new(1, "list", None);
    req.v = 999;
    let line = serde_json::to_string(&req).expect("encode");
    let response = dispatch(&line, &daemon).await;
    assert!(response.error.is_some());
    assert!(response.error.expect("err").message.contains("protocol version"));
  }

  #[tokio::test]
  async fn list_returns_empty_when_no_mounts() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = make_daemon(dir.path());
    let result = handle_method(&Request::new(1, "list", None), &daemon)
      .await
      .expect("ok");
    let parsed: ListResult = serde_json::from_value(result).expect("decode");
    assert!(parsed.mounts.is_empty());
  }

  #[tokio::test]
  async fn unmount_unknown_path_returns_error() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = make_daemon(dir.path());
    let err = daemon.unmount(Path::new("/tmp/never-mounted")).await;
    assert!(err.is_err());
  }
}
