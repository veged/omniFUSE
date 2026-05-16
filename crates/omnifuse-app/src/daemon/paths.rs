//! Filesystem locations used by the daemon (PID file, socket).

use std::path::PathBuf;

use crate::MountEnvironment;

/// PID file name under the daemon directory.
pub const PID_FILE_NAME: &str = "daemon.pid";
/// Socket file name under the daemon directory.
pub const SOCKET_FILE_NAME: &str = "daemon.sock";
/// Subdirectory containing daemon runtime files.
pub const DAEMON_DIR: &str = "omnifuse";

/// Resolved daemon paths.
#[derive(Debug, Clone)]
pub struct DaemonPaths {
  /// Directory containing all daemon runtime files.
  pub dir: PathBuf,
  /// PID file path.
  pub pid: PathBuf,
  /// Unix domain socket path.
  pub socket: PathBuf
}

impl DaemonPaths {
  /// Resolve daemon paths for the given environment.
  ///
  /// Linux: `$XDG_RUNTIME_DIR/omnifuse/` with a fallback to `$XDG_CACHE_HOME/omnifuse/runtime/`.
  /// macOS: `$HOME/Library/Caches/omnifuse/runtime/`.
  /// Windows / other: under the user's cache directory.
  ///
  /// # Errors
  ///
  /// Returns an error when the environment cannot resolve the user's home or cache directories.
  pub fn resolve(env: &impl MountEnvironment) -> anyhow::Result<Self> {
    let base = runtime_dir(env)?;
    let dir = base.join(DAEMON_DIR);
    Ok(Self {
      pid: dir.join(PID_FILE_NAME),
      socket: dir.join(SOCKET_FILE_NAME),
      dir
    })
  }

  /// Use an explicit directory as the daemon runtime root.
  #[must_use]
  pub fn with_root(root: PathBuf) -> Self {
    Self {
      pid: root.join(PID_FILE_NAME),
      socket: root.join(SOCKET_FILE_NAME),
      dir: root
    }
  }

  /// Ensure the daemon directory exists.
  ///
  /// # Errors
  ///
  /// Propagates IO errors from `create_dir_all`.
  pub fn ensure_dir(&self) -> std::io::Result<()> {
    std::fs::create_dir_all(&self.dir)
  }
}

fn runtime_dir(env: &impl MountEnvironment) -> anyhow::Result<PathBuf> {
  #[cfg(target_os = "linux")]
  {
    if let Some(value) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
      return Ok(PathBuf::from(value));
    }
  }

  // macOS and Linux without XDG_RUNTIME_DIR both fall through to the cache base — it is
  // user-private and persists across sessions, which is good enough for a desktop daemon.
  let _ = env; // suppress unused-warning on platforms where the branch is taken above
  env.cache_base_dir()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;
  use crate::environment::StdMountEnvironment;

  #[test]
  fn resolve_uses_explicit_root() {
    let paths = DaemonPaths::with_root(PathBuf::from("/tmp/omnifuse-test"));
    assert_eq!(paths.pid, PathBuf::from("/tmp/omnifuse-test/daemon.pid"));
    assert_eq!(paths.socket, PathBuf::from("/tmp/omnifuse-test/daemon.sock"));
  }

  #[test]
  fn resolve_places_files_under_runtime_dir() {
    let env = StdMountEnvironment;
    let paths = DaemonPaths::resolve(&env).expect("paths");
    assert!(paths.dir.ends_with("omnifuse"));
    assert_eq!(paths.pid.file_name().and_then(|n| n.to_str()), Some("daemon.pid"));
    assert_eq!(paths.socket.file_name().and_then(|n| n.to_str()), Some("daemon.sock"));
  }
}
