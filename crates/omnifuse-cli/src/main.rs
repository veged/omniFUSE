//! CLI для `OmniFuse` — универсальная VFS-утилита.
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

/// `OmniFuse` — универсальная VFS-утилита.
///
/// Монтирует git-репозитории и другие источники как файловую систему.
#[derive(Parser)]
#[command(name = "of", version, about)]
struct Cli {
  /// Подробный вывод (можно повторять: -v, -vv).
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  verbose: u8,

  /// Команда.
  #[command(subcommand)]
  command: Commands
}

/// Доступные команды.
#[derive(Subcommand)]
enum Commands {
  /// Смонтировать backend.
  Mount {
    /// Тип backend'а.
    #[command(subcommand)]
    backend: MountBackend
  },

  /// Проверить доступность FUSE/`WinFsp`.
  Check,

  /// Сгенерировать пример конфигурации.
  GenConfig
}

/// Backend для монтирования.
#[derive(Subcommand)]
enum MountBackend {
  /// Смонтировать git-репозиторий.
  Git {
    /// Источник: URL или путь к локальному репозиторию.
    source: String,
    /// Точка монтирования.
    mountpoint: PathBuf,
    /// Ветка.
    #[arg(short, long, default_value = "main")]
    branch: String,
    /// Интервал опроса remote (секунды).
    #[arg(long, default_value = "30")]
    poll_interval: u64,
    /// Разрешить доступ другим пользователям.
    #[arg(long)]
    allow_other: bool,
    /// Монтировать только для чтения.
    #[arg(long)]
    read_only: bool
  },

  /// Смонтировать wiki.
  Wiki {
    /// Базовый URL wiki API.
    base_url: String,
    /// Корневой slug.
    root_slug: String,
    /// Точка монтирования.
    mountpoint: PathBuf,
    /// Токен аутентификации.
    #[arg(long, env = "OMNIFUSE_WIKI_TOKEN")]
    auth: String,
    /// Интервал опроса remote (секунды).
    #[arg(long, default_value = "60")]
    poll_interval: u64,
    /// Разрешить доступ другим пользователям.
    #[arg(long)]
    allow_other: bool,
    /// Монтировать только для чтения.
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

/// Команда mount.
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
      // Проверить FUSE
      if !omnifuse_core::is_fuse_available() {
        anyhow::bail!(
          "FUSE не найден. Установите macFUSE (macOS) или libfuse3 (Linux).\n\
           Проверка: of check"
        );
      }

      info!(
        source = %source,
        mountpoint = %mountpoint.display(),
        branch = %branch,
        "монтирование git-репозитория"
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
        .context("ошибка монтирования")?;

      info!("размонтировано");
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
          "FUSE не найден. Установите macFUSE (macOS) или libfuse3 (Linux).\n\
           Проверка: of check"
        );
      }

      info!(
        base_url = %base_url,
        root_slug = %root_slug,
        mountpoint = %mountpoint.display(),
        "монтирование wiki"
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
        omnifuse_wiki::WikiBackend::new(wiki_config).context("ошибка создания wiki backend")?;

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
        .context("ошибка монтирования")?;

      info!("размонтировано");
      Ok(())
    }
  }
}

/// Команда check — проверка FUSE.
fn cmd_check() -> anyhow::Result<()> {
  if omnifuse_core::is_fuse_available() {
    println!("FUSE доступен");
    Ok(())
  } else {
    println!("FUSE не найден.");
    println!();

    #[cfg(target_os = "macos")]
    {
      println!("macOS: установите macFUSE");
      println!("  brew install --cask macfuse");
      println!("  или: https://osxfuse.github.io/");
    }

    #[cfg(target_os = "linux")]
    {
      println!("Linux: установите libfuse3");
      println!("  sudo apt install libfuse3-dev fuse3    # Debian/Ubuntu");
      println!("  sudo dnf install fuse3-devel fuse3     # Fedora");
      println!("  sudo pacman -S fuse3                   # Arch");
    }

    #[cfg(windows)]
    {
      println!("Windows: установите WinFsp");
      println!("  winget install WinFsp.WinFsp");
      println!("  или: https://winfsp.dev/");
    }

    anyhow::bail!("FUSE не установлен")
  }
}

/// Команда gen-config — пример конфигурации.
#[allow(clippy::unnecessary_wraps)]
fn cmd_gen_config() -> anyhow::Result<()> {
  let example = r#"# OmniFuse — пример конфигурации
# Расположение: ~/.config/omnifuse/config.toml

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
