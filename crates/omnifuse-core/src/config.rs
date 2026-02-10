//! `OmniFuse` configuration.

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

/// Mount configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
  /// Mount point.
  pub mount_point: PathBuf,
  /// Local directory for cache.
  pub local_dir: PathBuf,
  /// Sync settings.
  #[serde(default)]
  pub sync: SyncConfig,
  /// Buffer settings.
  #[serde(default)]
  pub buffer: BufferConfig,
  /// FUSE mount settings.
  #[serde(default)]
  pub mount_options: FuseMountOptions,
  /// Logging settings.
  #[serde(default)]
  pub logging: LoggingConfig
}

/// Sync settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
  /// Debounce delay before sync (seconds).
  #[serde(default = "SyncConfig::default_debounce_secs")]
  pub debounce_timeout_secs: u64,
  /// Maximum number of retries.
  #[serde(default = "SyncConfig::default_max_retry")]
  pub max_retry_attempts: u32,
  /// Initial retry delay (ms).
  #[serde(default = "SyncConfig::default_initial_backoff_ms")]
  pub initial_backoff_ms: u64,
  /// Maximum retry delay (seconds).
  #[serde(default = "SyncConfig::default_max_backoff_secs")]
  pub max_backoff_secs: u64
}

impl SyncConfig {
  const fn default_debounce_secs() -> u64 {
    1
  }

  const fn default_max_retry() -> u32 {
    10
  }

  const fn default_initial_backoff_ms() -> u64 {
    100
  }

  const fn default_max_backoff_secs() -> u64 {
    30
  }

  /// Convert to `Duration` — debounce timeout.
  #[must_use]
  pub const fn debounce_timeout(&self) -> Duration {
    Duration::from_secs(self.debounce_timeout_secs)
  }

  /// Convert to `Duration` — initial retry delay.
  #[must_use]
  pub const fn initial_backoff(&self) -> Duration {
    Duration::from_millis(self.initial_backoff_ms)
  }

  /// Convert to `Duration` — maximum retry delay.
  #[must_use]
  pub const fn max_backoff(&self) -> Duration {
    Duration::from_secs(self.max_backoff_secs)
  }
}

impl Default for SyncConfig {
  fn default() -> Self {
    Self {
      debounce_timeout_secs: Self::default_debounce_secs(),
      max_retry_attempts: Self::default_max_retry(),
      initial_backoff_ms: Self::default_initial_backoff_ms(),
      max_backoff_secs: Self::default_max_backoff_secs()
    }
  }
}

/// File buffer settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
  /// Maximum memory usage (MB).
  #[serde(default = "BufferConfig::default_max_memory_mb")]
  pub max_memory_mb: usize,
  /// Enable LRU eviction.
  #[serde(default = "BufferConfig::default_lru_enabled")]
  pub lru_eviction_enabled: bool
}

impl BufferConfig {
  const fn default_max_memory_mb() -> usize {
    512
  }

  const fn default_lru_enabled() -> bool {
    true
  }

  /// Maximum memory usage in bytes.
  #[must_use]
  pub const fn max_memory_bytes(&self) -> usize {
    self.max_memory_mb * 1024 * 1024
  }
}

impl Default for BufferConfig {
  fn default() -> Self {
    Self {
      max_memory_mb: Self::default_max_memory_mb(),
      lru_eviction_enabled: Self::default_lru_enabled()
    }
  }
}

/// FUSE mount settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuseMountOptions {
  /// Filesystem name.
  #[serde(default = "FuseMountOptions::default_fs_name")]
  pub fs_name: String,
  /// Allow access by other users.
  #[serde(default)]
  pub allow_other: bool,
  /// Mount as read-only.
  #[serde(default)]
  pub read_only: bool
}

impl FuseMountOptions {
  fn default_fs_name() -> String {
    "omnifuse".to_string()
  }
}

impl Default for FuseMountOptions {
  fn default() -> Self {
    Self {
      fs_name: Self::default_fs_name(),
      allow_other: false,
      read_only: false
    }
  }
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
  /// Log level.
  #[serde(default = "LoggingConfig::default_level")]
  pub level: String,
  /// Log file path.
  pub log_file: Option<PathBuf>
}

impl LoggingConfig {
  fn default_level() -> String {
    "info".to_string()
  }
}

impl Default for LoggingConfig {
  fn default() -> Self {
    Self {
      level: Self::default_level(),
      log_file: None
    }
  }
}
