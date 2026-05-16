//! Daemon process, IPC protocol, and degraded-mode fallback for the `of` CLI.
//!
//! Module layout:
//! - [`protocol`] — wire-format types (`Request`, `Response`, ...).
//! - [`paths`] — PID file and socket file locations.
//! - [`server`] — daemon state + UDS server.
//! - [`client`] — UDS client used by the CLI.
//! - [`mount_table`] — degraded-mode discovery and unmounting via OS mount table.

pub mod client;
pub mod mount_table;
pub mod paths;
pub mod protocol;
pub mod server;

use std::path::Path;

pub use client::{DaemonClient, default_socket, try_connect};
pub use paths::DaemonPaths;
pub use protocol::{AckResult, ListResult, MountSummary, Request, Response, StatusResult};
pub use server::{Daemon, serve};

/// Run the platform-appropriate unmount command for a mount point.
///
/// Exposed at the module root because both `server` and the CLI use it.
///
/// # Errors
///
/// Returns an error when the unmount command fails.
pub async fn unmount_filesystem(mount_point: &Path) -> anyhow::Result<()> {
  mount_table::unmount(mount_point).await
}
