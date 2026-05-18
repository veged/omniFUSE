//! End-to-end tests for the daemon server over a real Unix domain socket.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::doc_markdown)]

use std::time::Duration;

use omnifuse_app::{
  StdMountEnvironment,
  daemon::{Daemon, DaemonClient, DaemonPaths, serve}
};
use omnifuse_core::{FilesystemCache, FilesystemCacheConfig};
use tempfile::TempDir;
use tokio::time::timeout;

async fn spawn_daemon(
  tmp: &TempDir
) -> (
  DaemonPaths,
  tokio::task::JoinHandle<()>,
  tokio::sync::oneshot::Sender<()>
) {
  let paths = DaemonPaths::with_root(tmp.path().to_path_buf());
  paths.ensure_dir().expect("ensure_dir");
  let cache = FilesystemCache::open(tmp.path().join("cache"), FilesystemCacheConfig::default()).expect("cache");
  let daemon = Daemon::new(cache);
  let (tx, rx) = tokio::sync::oneshot::channel::<()>();
  let paths_clone = paths.clone();
  let task = tokio::spawn(async move {
    serve(paths_clone, daemon, rx).await.expect("serve");
  });
  // Wait for the socket file to appear.
  for _ in 0..50 {
    if paths.socket.exists() {
      return (paths, task, tx);
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  panic!("daemon socket never appeared at {}", paths.socket.display());
}

#[tokio::test]
async fn daemon_serves_list_request_over_uds() {
  let tmp = TempDir::new().expect("tempdir");
  let (paths, task, shutdown) = spawn_daemon(&tmp).await;

  let mut client = DaemonClient::connect(&paths.socket).await.expect("connect");
  let list = client.list().await.expect("list");
  assert!(list.mounts.is_empty());

  shutdown.send(()).expect("send shutdown");
  timeout(Duration::from_secs(5), task)
    .await
    .expect("join")
    .expect("task");
}

#[tokio::test]
async fn daemon_returns_error_for_unknown_method_path() {
  let tmp = TempDir::new().expect("tempdir");
  let (paths, task, shutdown) = spawn_daemon(&tmp).await;

  let mut client = DaemonClient::connect(&paths.socket).await.expect("connect");
  let result = client
    .status(std::path::Path::new("/nope/nowhere"))
    .await
    .expect("status");
  // Mount does not exist — server returns `mount: None` rather than an error.
  assert!(result.mount.is_none());

  let unmount_err = client.unmount(std::path::Path::new("/nope/nowhere")).await;
  assert!(unmount_err.is_err(), "unmount must error on unknown mount point");

  shutdown.send(()).expect("send shutdown");
  timeout(Duration::from_secs(5), task)
    .await
    .expect("join")
    .expect("task");
}

#[tokio::test]
async fn daemon_refuses_to_replace_live_socket() {
  let tmp = TempDir::new().expect("tempdir");
  let (paths, task, shutdown) = spawn_daemon(&tmp).await;

  let cache = FilesystemCache::open(tmp.path().join("cache-2"), FilesystemCacheConfig::default()).expect("cache");
  let daemon = Daemon::new(cache);
  let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
  let error = serve(paths.clone(), daemon, rx)
    .await
    .expect_err("second daemon must fail");

  assert!(
    error.to_string().contains("already running"),
    "unexpected error: {error}"
  );
  assert!(paths.pid.exists(), "first daemon pid file must remain intact");

  shutdown.send(()).expect("send shutdown");
  timeout(Duration::from_secs(5), task)
    .await
    .expect("join")
    .expect("task");
}

#[tokio::test]
async fn daemon_paths_resolve_via_std_env() {
  // Smoke test that path resolution does not panic on the host environment.
  let _ = DaemonPaths::resolve(&StdMountEnvironment).expect("paths");
}
