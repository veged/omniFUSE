//! CLI for `OmniFuse` — a universal VFS utility.
//!
//! ```bash
//! of mount git https://github.com/user/repo /mnt/repo --branch=main
//! of mount git /path/to/local/repo /mnt/repo
//! of mount wiki https://wiki.example.com my/project /mnt/wiki --auth TOKEN
//! of check
//! of skill mount git --for=claude
//! ```

mod skill;

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use omnifuse_app::{GitMountArgs, MountService, S3MountArgs, WikiMountArgs};
use omnifuse_core::LoggingConfig;
use tracing::info;

/// `OmniFuse` — a universal VFS utility.
///
/// Mounts git repositories and other sources as a filesystem.
#[derive(Parser)]
#[command(name = "of", version, about)]
pub(crate) struct Cli {
  /// Verbose output (can be repeated: -v, -vv).
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  verbose: u8,

  /// Print the agent-oriented manual (markdown). Symmetric to `--help`.
  #[arg(long, global = true)]
  skill: bool,

  /// Tool to tailor the skill manual for (e.g. `claude`, `openai`, `langchain`).
  #[arg(long = "for", global = true, value_name = "TOOL")]
  for_tool: Option<String>,

  /// Command.
  #[command(subcommand)]
  command: Commands
}

/// Available commands.
#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // clap-generated CLI enum, size unavoidable
enum Commands {
  /// Mount a backend.
  Mount {
    /// Backend type.
    #[command(subcommand)]
    backend: MountBackend
  },

  /// List active mounts.
  ///
  /// Uses the daemon when one is running; falls back to the OS mount table.
  List,

  /// Inspect a mounted path.
  Status {
    /// Mount point to inspect.
    mountpoint: PathBuf
  },

  /// Unmount a mounted path.
  Unmount {
    /// Mount point to unmount.
    mountpoint: PathBuf
  },

  /// Manage the background daemon.
  Daemon {
    /// Subcommand.
    #[command(subcommand)]
    action: DaemonAction
  },

  /// Check FUSE/`WinFsp` availability.
  Check,

  /// Generate a sample configuration.
  GenConfig,

  /// Print the agent-oriented manual (markdown).
  Skill {
    /// Subcommand path (e.g. `mount git`).
    path: Vec<String>
  }
}

/// Daemon lifecycle subcommands.
#[derive(Subcommand)]
enum DaemonAction {
  /// Start the daemon in the background (idempotent).
  Start,
  /// Stop the daemon and unmount everything it hosts.
  Stop,
  /// Show daemon state.
  Status,
  /// Run the daemon process in the foreground (used internally by `start`).
  #[command(hide = true)]
  Run
}

/// Backend for mounting.
#[derive(Subcommand)]
enum MountBackend {
  /// Mount a git repository.
  Git {
    /// Source: URL or path to a local repository.
    source: String,
    /// Mount point.
    mountpoint: PathBuf,
    /// Branch.
    #[arg(short, long, default_value = "main")]
    branch: String,
    /// Remote polling interval (seconds).
    #[arg(long, default_value = "30")]
    poll_interval: u64,
    /// Allow access by other users.
    #[arg(long)]
    allow_other: bool,
    /// Mount as read-only.
    #[arg(long)]
    read_only: bool
  },

  /// Mount an S3-compatible bucket.
  S3 {
    /// Bucket name.
    bucket: String,
    /// Mount point.
    mountpoint: PathBuf,
    /// Object key prefix mounted as root.
    #[arg(long)]
    prefix: Option<String>,
    /// S3-compatible endpoint URL.
    #[arg(long, env = "OMNIFUSE_S3_ENDPOINT")]
    endpoint: Option<String>,
    /// S3 region (`auto` for R2, real region for AWS).
    #[arg(long, env = "OMNIFUSE_S3_REGION")]
    region: Option<String>,
    /// Access key ID.
    #[arg(long, env = "OMNIFUSE_S3_ACCESS_KEY_ID")]
    access_key_id: Option<String>,
    /// Secret access key.
    #[arg(long, env = "OMNIFUSE_S3_SECRET_ACCESS_KEY")]
    secret_access_key: Option<String>,
    /// Temporary session token.
    #[arg(long, env = "OMNIFUSE_S3_SESSION_TOKEN")]
    session_token: Option<String>,
    /// Use virtual-hosted-style requests.
    #[arg(long)]
    virtual_host_style: bool,
    /// Remote polling interval (seconds).
    #[arg(long, default_value = "60")]
    poll_interval: u64,
    /// Allow access by other users.
    #[arg(long)]
    allow_other: bool,
    /// Mount as read-only.
    #[arg(long)]
    read_only: bool
  },

  /// Mount a wiki.
  Wiki {
    /// Base URL of the wiki API.
    base_url: String,
    /// Root slug.
    root_slug: String,
    /// Mount point.
    mountpoint: PathBuf,
    /// Authentication token (OAuth or IAM).
    #[arg(long, env = "OMNIFUSE_WIKI_TOKEN")]
    auth: String,
    /// Organization ID (X-Org-Id header, required for Yandex 360 Wiki).
    #[arg(long, env = "OMNIFUSE_WIKI_ORG_ID")]
    org_id: Option<String>,
    /// Remote polling interval (seconds).
    #[arg(long, default_value = "60")]
    poll_interval: u64,
    /// Allow access by other users.
    #[arg(long)]
    allow_other: bool,
    /// Mount as read-only.
    #[arg(long)]
    read_only: bool
  }
}

fn logging_config_for_verbose(verbose: u8) -> LoggingConfig {
  let level = match verbose {
    0 => "info",
    1 => "debug",
    _ => "trace"
  };

  LoggingConfig {
    level: level.to_string(),
    log_file: None
  }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let argv: Vec<String> = std::env::args().collect();
  if let Some(invocation) = skill::detect_skill_flag(&argv) {
    print!("{}", skill::render(&invocation.path, invocation.for_tool.as_deref()));
    return Ok(());
  }

  let cli = Cli::parse();
  omnifuse_core::init_logging(&logging_config_for_verbose(cli.verbose))?;

  match cli.command {
    Commands::Mount { backend } => cmd_mount(backend).await,
    Commands::List => cmd_list().await,
    Commands::Status { mountpoint } => cmd_status(&mountpoint).await,
    Commands::Unmount { mountpoint } => cmd_unmount(&mountpoint).await,
    Commands::Daemon { action } => cmd_daemon(action).await,
    Commands::Check => cmd_check(),
    Commands::GenConfig => cmd_gen_config(),
    Commands::Skill { path } => {
      print!("{}", skill::render(&path, cli.for_tool.as_deref()));
      Ok(())
    }
  }
}

/// Mount command.
async fn cmd_mount(backend: MountBackend) -> anyhow::Result<()> {
  match backend {
    MountBackend::Git {
      source,
      mountpoint,
      branch,
      poll_interval,
      allow_other,
      read_only
    } => cmd_mount_git(source, mountpoint, branch, poll_interval, allow_other, read_only).await,
    MountBackend::Wiki {
      base_url,
      root_slug,
      mountpoint,
      auth,
      org_id,
      poll_interval,
      allow_other,
      read_only
    } => {
      cmd_mount_wiki(
        base_url,
        root_slug,
        mountpoint,
        auth,
        org_id,
        poll_interval,
        allow_other,
        read_only
      )
      .await
    }
    MountBackend::S3 {
      bucket,
      mountpoint,
      prefix,
      endpoint,
      region,
      access_key_id,
      secret_access_key,
      session_token,
      virtual_host_style,
      poll_interval,
      allow_other,
      read_only
    } => {
      cmd_mount_s3(S3MountArgs {
        bucket,
        mount_point: mountpoint,
        prefix,
        endpoint,
        region,
        access_key_id,
        secret_access_key,
        session_token,
        virtual_host_style,
        poll_interval_secs: Some(poll_interval),
        allow_other,
        read_only
      })
      .await
    }
  }
}

async fn cmd_mount_s3(args: S3MountArgs) -> anyhow::Result<()> {
  info!(
    bucket = %args.bucket,
    mountpoint = %args.mount_point.display(),
    "mounting S3-compatible bucket"
  );

  if try_daemon_mount("s3", &args).await? {
    return Ok(());
  }

  MountService::with_default_cache()?
    .run_s3(args, omnifuse_core::NoopSink)
    .await
    .context("mount error")?;

  info!("unmounted");
  Ok(())
}

async fn cmd_mount_git(
  source: String,
  mountpoint: PathBuf,
  branch: String,
  poll_interval: u64,
  allow_other: bool,
  read_only: bool
) -> anyhow::Result<()> {
  info!(
    source = %source,
    mountpoint = %mountpoint.display(),
    branch = %branch,
    "mounting git repository"
  );

  let args = GitMountArgs {
    source,
    mount_point: mountpoint,
    branch: Some(branch),
    poll_interval_secs: Some(poll_interval),
    allow_other,
    read_only
  };

  if try_daemon_mount("git", &args).await? {
    return Ok(());
  }

  MountService::with_default_cache()?
    .run_git(args, omnifuse_core::NoopSink)
    .await
    .context("mount error")?;

  info!("unmounted");
  Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_mount_wiki(
  base_url: String,
  root_slug: String,
  mountpoint: PathBuf,
  auth: String,
  org_id: Option<String>,
  poll_interval: u64,
  allow_other: bool,
  read_only: bool
) -> anyhow::Result<()> {
  info!(
    base_url = %base_url,
    root_slug = %root_slug,
    mountpoint = %mountpoint.display(),
    "mounting wiki"
  );

  let args = WikiMountArgs {
    base_url,
    root_slug,
    auth_token: auth,
    org_id,
    mount_point: mountpoint,
    poll_interval_secs: Some(poll_interval),
    allow_other,
    read_only
  };

  if try_daemon_mount("wiki", &args).await? {
    return Ok(());
  }

  MountService::with_default_cache()?
    .run_wiki(args, omnifuse_core::NoopSink)
    .await
    .context("mount error")?;

  info!("unmounted");
  Ok(())
}

/// Check command — verify FUSE availability.
fn cmd_check() -> anyhow::Result<()> {
  if omnifuse_core::is_fuse_available() {
    println!("FUSE is available");
    Ok(())
  } else {
    println!("FUSE not found.");
    println!();

    #[cfg(target_os = "macos")]
    {
      println!("macOS: install macFUSE");
      println!("  brew install --cask macfuse");
      println!("  or: https://osxfuse.github.io/");
    }

    #[cfg(target_os = "linux")]
    {
      println!("Linux: install libfuse3");
      println!("  sudo apt install libfuse3-dev fuse3    # Debian/Ubuntu");
      println!("  sudo dnf install fuse3-devel fuse3     # Fedora");
      println!("  sudo pacman -S fuse3                   # Arch");
    }

    #[cfg(windows)]
    {
      println!("Windows: install WinFsp");
      println!("  winget install WinFsp.WinFsp");
      println!("  or: https://winfsp.dev/");
    }

    anyhow::bail!("FUSE is not installed")
  }
}

/// Gen-config command — sample configuration.
#[allow(clippy::unnecessary_wraps)]
fn cmd_gen_config() -> anyhow::Result<()> {
  let example = r#"# OmniFuse — sample configuration
# Location: ~/.config/omnifuse/config.toml

[backends.my-repo]
type = "git"
remote = "https://github.com/user/repo"
branch = "main"

[backends.my-wiki]
type = "wiki"
url = "https://wiki.example.com"
root_slug = "my/project"
token = "secret"

[mounts."/home/user/mnt/repo"]
backend = "my-repo"
sync_interval = "30s"
debounce = "1s"

[mounts."/home/user/mnt/wiki"]
backend = "my-wiki"
sync_interval = "60s"
"#;

  println!("{example}");
  Ok(())
}

// ---------------------------------------------------------------------------
// Daemon-aware lifecycle commands.

/// `OMNIFUSE_NO_DAEMON=1` opts out of daemon use even when a socket exists.
const NO_DAEMON_ENV: &str = "OMNIFUSE_NO_DAEMON";

async fn try_daemon_mount<T: serde::Serialize + Sync>(backend: &str, args: &T) -> anyhow::Result<bool> {
  if std::env::var(NO_DAEMON_ENV).is_ok_and(|value| value == "1") {
    return Ok(false);
  }
  let socket = omnifuse_app::daemon::default_socket()?;
  let Some(mut client) = omnifuse_app::daemon::try_connect(&socket).await? else {
    return Ok(false);
  };
  let payload = serde_json::to_value(args)?;
  let ack = client.mount(backend, payload).await?;
  info!(mount_point = %ack.mount_point.display(), "daemon mounted instance");
  println!("mounted {} (daemon)", ack.mount_point.display());
  Ok(true)
}

async fn cmd_list() -> anyhow::Result<()> {
  let socket = omnifuse_app::daemon::default_socket()?;
  let mounts = if let Some(mut client) = omnifuse_app::daemon::try_connect(&socket).await? {
    client.list().await?.mounts
  } else {
    omnifuse_app::daemon::mount_table::list_active().await?
  };

  if mounts.is_empty() {
    println!("no active mounts");
    return Ok(());
  }

  println!("{:<12} {:<10} MOUNT POINT", "BACKEND", "AGE(s)");
  for entry in mounts {
    println!(
      "{:<12} {:<10} {}",
      entry.backend,
      entry.age_secs,
      entry.mount_point.display()
    );
  }
  Ok(())
}

async fn cmd_status(mountpoint: &Path) -> anyhow::Result<()> {
  let socket = omnifuse_app::daemon::default_socket()?;
  if let Some(mut client) = omnifuse_app::daemon::try_connect(&socket).await? {
    let status = client.status(mountpoint).await?;
    match status.mount {
      Some(entry) => {
        println!(
          "{} ({}, instance {}) age {}s",
          entry.mount_point.display(),
          entry.backend,
          entry.instance,
          entry.age_secs
        );
        println!("dirty files: {}", status.dirty_files);
        if let Some(ratio) = status.cache_hit_ratio {
          println!("cache hit ratio: {ratio:.3}");
        }
      }
      None => {
        println!("{} is not currently mounted by the daemon", mountpoint.display());
      }
    }
    return Ok(());
  }

  match omnifuse_app::daemon::mount_table::lookup(mountpoint).await? {
    Some(entry) => {
      println!(
        "{} ({}, degraded view — no daemon running)",
        entry.mount_point.display(),
        entry.backend
      );
    }
    None => println!("{} is not currently mounted", mountpoint.display())
  }
  Ok(())
}

async fn cmd_unmount(mountpoint: &Path) -> anyhow::Result<()> {
  let socket = omnifuse_app::daemon::default_socket()?;
  if let Some(mut client) = omnifuse_app::daemon::try_connect(&socket).await? {
    let ack = client.unmount(mountpoint).await?;
    println!("unmounted {}", ack.mount_point.display());
    return Ok(());
  }
  omnifuse_app::daemon::unmount_filesystem(mountpoint).await?;
  println!("unmounted {}", mountpoint.display());
  Ok(())
}

async fn cmd_daemon(action: DaemonAction) -> anyhow::Result<()> {
  match action {
    DaemonAction::Start => cmd_daemon_start().await,
    DaemonAction::Stop => cmd_daemon_stop().await,
    DaemonAction::Status => cmd_daemon_status().await,
    DaemonAction::Run => cmd_daemon_run().await
  }
}

async fn cmd_daemon_start() -> anyhow::Result<()> {
  let paths = omnifuse_app::daemon::DaemonPaths::resolve(&omnifuse_app::StdMountEnvironment)?;
  paths.ensure_dir()?;
  if omnifuse_app::daemon::try_connect(&paths.socket).await?.is_some() {
    println!("daemon already running at {}", paths.socket.display());
    return Ok(());
  }

  // Spawn the daemon as a detached child process so the foreground call returns immediately.
  let mut command = std::process::Command::new(std::env::current_exe()?);
  command.arg("daemon").arg("run");
  command.stdin(std::process::Stdio::null());
  command.stdout(std::process::Stdio::null());
  command.stderr(std::process::Stdio::null());
  let child = command.spawn().context("spawning daemon process")?;
  println!("daemon starting (pid {})", child.id());

  // Wait briefly for the socket to appear, but do not block indefinitely.
  for _ in 0..50 {
    if omnifuse_app::daemon::try_connect(&paths.socket).await?.is_some() {
      println!("daemon listening at {}", paths.socket.display());
      return Ok(());
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }
  println!(
    "daemon spawned; socket has not appeared yet at {}",
    paths.socket.display()
  );
  Ok(())
}

#[allow(clippy::unused_async)]
async fn cmd_daemon_stop() -> anyhow::Result<()> {
  let paths = omnifuse_app::daemon::DaemonPaths::resolve(&omnifuse_app::StdMountEnvironment)?;
  let pid_text = match std::fs::read_to_string(&paths.pid) {
    Ok(text) => text,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      println!("daemon is not running");
      return Ok(());
    }
    Err(error) => return Err(error.into())
  };
  let pid: i32 = pid_text
    .trim()
    .parse()
    .context("daemon pid file does not contain an integer")?;

  #[cfg(unix)]
  {
    use nix::{
      sys::signal::{Signal, kill},
      unistd::Pid
    };
    kill(Pid::from_raw(pid), Signal::SIGTERM).context("sending SIGTERM to daemon")?;
    println!("sent SIGTERM to daemon (pid {pid})");
  }
  #[cfg(not(unix))]
  {
    let _ = pid;
    anyhow::bail!("daemon stop is only supported on Unix");
  }

  Ok(())
}

async fn cmd_daemon_status() -> anyhow::Result<()> {
  let paths = omnifuse_app::daemon::DaemonPaths::resolve(&omnifuse_app::StdMountEnvironment)?;
  let pid = std::fs::read_to_string(&paths.pid).ok();
  if let Some(mut client) = omnifuse_app::daemon::try_connect(&paths.socket).await? {
    let mounts = client.list().await?.mounts;
    println!(
      "daemon running ({}, {} mount{})",
      pid
        .as_deref()
        .map(str::trim)
        .map_or_else(|| "pid unknown".to_string(), |pid| format!("pid {pid}")),
      mounts.len(),
      if mounts.len() == 1 { "" } else { "s" }
    );
    return Ok(());
  }
  match pid {
    Some(pid) => println!("daemon socket is unreachable (stale pid file: {})", pid.trim()),
    None => println!("daemon is not running")
  }
  Ok(())
}

async fn cmd_daemon_run() -> anyhow::Result<()> {
  let paths = omnifuse_app::daemon::DaemonPaths::resolve(&omnifuse_app::StdMountEnvironment)?;
  let daemon = omnifuse_app::daemon::Daemon::with_default_cache()?;
  let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

  // Trigger graceful shutdown on SIGINT/SIGTERM.
  #[cfg(unix)]
  {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let trigger = shutdown_tx;
    tokio::spawn(async move {
      tokio::select! {
        _ = sigint.recv() => {},
        _ = sigterm.recv() => {}
      }
      let _ = trigger.send(());
    });
  }
  #[cfg(not(unix))]
  {
    let trigger = shutdown_tx;
    tokio::spawn(async move {
      let _ = tokio::signal::ctrl_c().await;
      let _ = trigger.send(());
    });
  }

  omnifuse_app::daemon::serve(paths, daemon, shutdown_rx).await
}
