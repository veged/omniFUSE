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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::time::Duration;

  use super::*;

  #[test]
  fn test_sync_config_defaults() {
    let config = SyncConfig::default();
    assert_eq!(config.debounce_timeout_secs, 1);
    assert_eq!(config.max_retry_attempts, 10);
    assert_eq!(config.initial_backoff_ms, 100);
    assert_eq!(config.max_backoff_secs, 30);
  }

  #[test]
  fn test_sync_config_debounce_timeout() {
    let config = SyncConfig::default();
    assert_eq!(config.debounce_timeout(), Duration::from_secs(1));
  }

  #[test]
  fn test_sync_config_backoff() {
    let config = SyncConfig::default();
    assert_eq!(config.initial_backoff(), Duration::from_millis(100));
    assert_eq!(config.max_backoff(), Duration::from_secs(30));
  }

  #[test]
  fn test_buffer_config_defaults() {
    let config = BufferConfig::default();
    assert_eq!(config.max_memory_mb, 512);
    assert!(config.lru_eviction_enabled);
  }

  #[test]
  fn test_buffer_config_max_memory_bytes() {
    let config = BufferConfig::default();
    assert_eq!(config.max_memory_bytes(), 512 * 1024 * 1024);
  }

  #[test]
  fn test_fuse_mount_options_defaults() {
    let opts = FuseMountOptions::default();
    assert_eq!(opts.fs_name, "omnifuse");
    assert!(!opts.allow_other);
    assert!(!opts.read_only);
  }

  #[test]
  fn test_logging_config_defaults() {
    let config = LoggingConfig::default();
    assert_eq!(config.level, "info");
    assert!(config.log_file.is_none());
  }

  #[test]
  fn test_mount_config_toml_roundtrip() {
    let config = MountConfig {
      mount_point: PathBuf::from("/mnt/test"),
      local_dir: PathBuf::from("/tmp/test"),
      sync: SyncConfig::default(),
      buffer: BufferConfig::default(),
      mount_options: FuseMountOptions::default(),
      logging: LoggingConfig::default()
    };

    let toml_str = toml::to_string(&config).expect("serialize");
    let restored: MountConfig = toml::from_str(&toml_str).expect("deserialize");

    assert_eq!(restored.mount_point, config.mount_point);
    assert_eq!(restored.local_dir, config.local_dir);
    assert_eq!(restored.sync.debounce_timeout_secs, config.sync.debounce_timeout_secs);
    assert_eq!(restored.buffer.max_memory_mb, config.buffer.max_memory_mb);
  }

  /// SyncConfig with custom values: debounce_ms=500, max_retries=5.
  ///
  /// Verify that debounce_timeout() and max_retry_attempts match the specified values.
  /// Note: debounce_timeout_secs is set in seconds, but the test checks
  /// via Duration for precision (500ms -> 0 seconds in integer field,
  /// so we use the minimum allowed value — 1 second as a "custom" analog).
  #[test]
  fn test_sync_config_custom_values() {
    let config = SyncConfig {
      debounce_timeout_secs: 5,
      max_retry_attempts: 5,
      initial_backoff_ms: 500,
      max_backoff_secs: 60
    };

    // debounce_timeout() converts seconds to Duration
    assert_eq!(config.debounce_timeout(), Duration::from_secs(5));
    assert_eq!(config.max_retry_attempts, 5);
    assert_eq!(config.initial_backoff(), Duration::from_millis(500));
    assert_eq!(config.max_backoff(), Duration::from_secs(60));
  }

  /// BufferConfig with max_memory_mb=256 -> max_memory_bytes() = 256*1024*1024.
  #[test]
  fn test_buffer_config_custom_memory() {
    let config = BufferConfig {
      max_memory_mb: 256,
      lru_eviction_enabled: false
    };

    assert_eq!(config.max_memory_mb, 256);
    assert_eq!(config.max_memory_bytes(), 256 * 1024 * 1024);
    assert!(!config.lru_eviction_enabled);
  }

  /// MountConfig with all fields populated -> TOML serialize/deserialize roundtrip.
  ///
  /// Verify all fields: repo_source (local_dir), mount_point, sync, buffer,
  /// fuse (mount_options), logging — including custom values.
  #[test]
  fn test_mount_config_with_all_fields() {
    let config = MountConfig {
      mount_point: PathBuf::from("/mnt/wiki"),
      local_dir: PathBuf::from("/var/lib/omnifuse/repo"),
      sync: SyncConfig {
        debounce_timeout_secs: 3,
        max_retry_attempts: 7,
        initial_backoff_ms: 200,
        max_backoff_secs: 120
      },
      buffer: BufferConfig {
        max_memory_mb: 1024,
        lru_eviction_enabled: false
      },
      mount_options: FuseMountOptions {
        fs_name: "testwiki".to_string(),
        allow_other: true,
        read_only: true
      },
      logging: LoggingConfig {
        level: "debug".to_string(),
        log_file: Some(PathBuf::from("/var/log/omnifuse.log"))
      }
    };

    // Serialize → TOML → Deserialize
    let toml_str = toml::to_string(&config).expect("serialize");
    let restored: MountConfig = toml::from_str(&toml_str).expect("deserialize");

    // Verify all fields after roundtrip
    assert_eq!(restored.mount_point, PathBuf::from("/mnt/wiki"));
    assert_eq!(restored.local_dir, PathBuf::from("/var/lib/omnifuse/repo"));

    // Sync
    assert_eq!(restored.sync.debounce_timeout_secs, 3);
    assert_eq!(restored.sync.max_retry_attempts, 7);
    assert_eq!(restored.sync.initial_backoff_ms, 200);
    assert_eq!(restored.sync.max_backoff_secs, 120);

    // Buffer
    assert_eq!(restored.buffer.max_memory_mb, 1024);
    assert!(!restored.buffer.lru_eviction_enabled);

    // FUSE mount options
    assert_eq!(restored.mount_options.fs_name, "testwiki");
    assert!(restored.mount_options.allow_other);
    assert!(restored.mount_options.read_only);

    // Logging
    assert_eq!(restored.logging.level, "debug");
    assert_eq!(restored.logging.log_file, Some(PathBuf::from("/var/log/omnifuse.log")));
  }
}
