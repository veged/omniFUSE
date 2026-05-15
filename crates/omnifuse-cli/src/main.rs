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

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use omnifuse_app::{GitMountArgs, MountService, WikiMountArgs};
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
enum Commands {
  /// Mount a backend.
  Mount {
    /// Backend type.
    #[command(subcommand)]
    backend: MountBackend
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
  }
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

  MountService::default()
    .run_git(
      GitMountArgs {
        source,
        mount_point: mountpoint,
        branch: Some(branch),
        poll_interval_secs: Some(poll_interval),
        allow_other,
        read_only
      },
      omnifuse_core::NoopSink
    )
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

  MountService::default()
    .run_wiki(
      WikiMountArgs {
        base_url,
        root_slug,
        auth_token: auth,
        org_id,
        mount_point: mountpoint,
        poll_interval_secs: Some(poll_interval),
        allow_other,
        read_only
      },
      omnifuse_core::NoopSink
    )
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
