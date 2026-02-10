//! CLI for `OmniFuse` — a universal VFS utility.
//!
//! ```bash
//! of mount git https://github.com/user/repo /mnt/repo --branch=main
//! of mount git /path/to/local/repo /mnt/repo
//! of mount wiki https://wiki.example.com my/project /mnt/wiki --auth TOKEN
//! of check
//! ```

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// `OmniFuse` — a universal VFS utility.
///
/// Mounts git repositories and other sources as a filesystem.
#[derive(Parser)]
#[command(name = "of", version, about)]
struct Cli {
  /// Verbose output (can be repeated: -v, -vv).
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  verbose: u8,

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
  GenConfig
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
    /// Authentication token.
    #[arg(long, env = "OMNIFUSE_WIKI_TOKEN")]
    auth: String,
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

fn init_tracing(verbose: u8) {
  let filter = match verbose {
    0 => "info",
    1 => "debug",
    _ => "trace"
  };

  tracing_subscriber::fmt()
    .with_env_filter(
      EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter))
    )
    .compact()
    .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();
  init_tracing(cli.verbose);

  match cli.command {
    Commands::Mount { backend } => cmd_mount(backend).await,
    Commands::Check => cmd_check(),
    Commands::GenConfig => cmd_gen_config()
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
    } => {
      // Check FUSE availability
      if !omnifuse_core::is_fuse_available() {
        anyhow::bail!(
          "FUSE not found. Install macFUSE (macOS) or libfuse3 (Linux).\n\
           Check: of check"
        );
      }

      info!(
        source = %source,
        mountpoint = %mountpoint.display(),
        branch = %branch,
        "mounting git repository"
      );

      let git_config = omnifuse_git::GitConfig {
        source,
        branch,
        max_push_retries: 3,
        poll_interval_secs: poll_interval
      };

      let git_backend = omnifuse_git::GitBackend::new(git_config);

      let mount_config = omnifuse_core::MountConfig {
        mount_point: mountpoint.clone(),
        local_dir: mountpoint.clone(),
        sync: omnifuse_core::SyncConfig::default(),
        buffer: omnifuse_core::BufferConfig::default(),
        mount_options: omnifuse_core::FuseMountOptions {
          fs_name: "omnifuse-git".to_string(),
          allow_other,
          read_only
        },
        logging: omnifuse_core::LoggingConfig::default()
      };

      omnifuse_core::run_mount(mount_config, git_backend, omnifuse_core::NoopEventHandler)
        .await
        .context("mount error")?;

      info!("unmounted");
      Ok(())
    }
    MountBackend::Wiki {
      base_url,
      root_slug,
      mountpoint,
      auth,
      poll_interval,
      allow_other,
      read_only
    } => {
      if !omnifuse_core::is_fuse_available() {
        anyhow::bail!(
          "FUSE not found. Install macFUSE (macOS) or libfuse3 (Linux).\n\
           Check: of check"
        );
      }

      info!(
        base_url = %base_url,
        root_slug = %root_slug,
        mountpoint = %mountpoint.display(),
        "mounting wiki"
      );

      let wiki_config = omnifuse_wiki::WikiConfig {
        base_url,
        auth_token: auth,
        root_slug,
        poll_interval_secs: poll_interval,
        max_depth: 10,
        max_pages: 500
      };

      let wiki_backend =
        omnifuse_wiki::WikiBackend::new(wiki_config).context("failed to create wiki backend")?;

      let mount_config = omnifuse_core::MountConfig {
        mount_point: mountpoint.clone(),
        local_dir: mountpoint.clone(),
        sync: omnifuse_core::SyncConfig::default(),
        buffer: omnifuse_core::BufferConfig::default(),
        mount_options: omnifuse_core::FuseMountOptions {
          fs_name: "omnifuse-wiki".to_string(),
          allow_other,
          read_only
        },
        logging: omnifuse_core::LoggingConfig::default()
      };

      omnifuse_core::run_mount(mount_config, wiki_backend, omnifuse_core::NoopEventHandler)
        .await
        .context("mount error")?;

      info!("unmounted");
      Ok(())
    }
  }
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
