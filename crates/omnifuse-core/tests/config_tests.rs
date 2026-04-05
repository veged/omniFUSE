//! Configuration TOML parsing and validation tests.
//!
//! Run: `cargo test -p omnifuse-core --test config_tests`

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use omnifuse_core::{BufferConfig, FuseMountOptions, LoggingConfig, MountConfig, SyncConfig};

/// Full config with all fields populated -> TOML -> parse back -> verify all fields.
#[test]
fn test_mount_config_full_toml_roundtrip() {
  let config = MountConfig {
    mount_point: PathBuf::from("/mnt/test"),
    local_dir: PathBuf::from("/tmp/test"),
    sync: SyncConfig {
      debounce_timeout_secs: 5,
      max_retry_attempts: 3,
      initial_backoff_ms: 200,
      max_backoff_secs: 60
    },
    buffer: BufferConfig {
      max_memory_mb: 256,
      lru_eviction_enabled: false
    },
    mount_options: FuseMountOptions {
      fs_name: "myfs".to_string(),
      allow_other: true,
      read_only: true
    },
    logging: LoggingConfig {
      level: "debug".to_string(),
      log_file: Some(PathBuf::from("/var/log/omnifuse.log"))
    }
  };

  let toml_str = toml::to_string(&config).expect("serialize to TOML");
  let restored: MountConfig = toml::from_str(&toml_str).expect("deserialize from TOML");

  assert_eq!(restored.mount_point, PathBuf::from("/mnt/test"));
  assert_eq!(restored.local_dir, PathBuf::from("/tmp/test"));
  assert_eq!(restored.sync.debounce_timeout_secs, 5);
  assert_eq!(restored.sync.max_retry_attempts, 3);
  assert_eq!(restored.sync.initial_backoff_ms, 200);
  assert_eq!(restored.sync.max_backoff_secs, 60);
  assert_eq!(restored.buffer.max_memory_mb, 256);
  assert!(!restored.buffer.lru_eviction_enabled);
  assert_eq!(restored.mount_options.fs_name, "myfs");
  assert!(restored.mount_options.allow_other);
  assert!(restored.mount_options.read_only);
  assert_eq!(restored.logging.level, "debug");
  assert_eq!(restored.logging.log_file, Some(PathBuf::from("/var/log/omnifuse.log")));
}

/// TOML with only mount_point and local_dir -> parse -> verify sync/buffer/mount_options/logging get defaults.
#[test]
fn test_mount_config_partial_toml_defaults_applied() {
  let toml_str = r#"
mount_point = "/mnt/partial"
local_dir = "/tmp/partial"
"#;

  let config: MountConfig = toml::from_str(toml_str).expect("parse partial TOML");

  assert_eq!(config.mount_point, PathBuf::from("/mnt/partial"));
  assert_eq!(config.local_dir, PathBuf::from("/tmp/partial"));

  // Sync defaults
  assert_eq!(config.sync.debounce_timeout_secs, 1);
  assert_eq!(config.sync.max_retry_attempts, 10);
  assert_eq!(config.sync.initial_backoff_ms, 100);
  assert_eq!(config.sync.max_backoff_secs, 30);

  // Buffer defaults
  assert_eq!(config.buffer.max_memory_mb, 512);
  assert!(config.buffer.lru_eviction_enabled);

  // Mount options defaults
  assert_eq!(config.mount_options.fs_name, "omnifuse");
  assert!(!config.mount_options.allow_other);
  assert!(!config.mount_options.read_only);

  // Logging defaults
  assert_eq!(config.logging.level, "info");
  assert!(config.logging.log_file.is_none());
}

/// SyncConfig with custom debounce=5, max_retry=3, backoff values -> TOML roundtrip.
#[test]
fn test_sync_config_custom_values_roundtrip() {
  let config = SyncConfig {
    debounce_timeout_secs: 5,
    max_retry_attempts: 3,
    initial_backoff_ms: 500,
    max_backoff_secs: 120
  };

  let toml_str = toml::to_string(&config).expect("serialize SyncConfig");
  let restored: SyncConfig = toml::from_str(&toml_str).expect("deserialize SyncConfig");

  assert_eq!(restored.debounce_timeout_secs, 5);
  assert_eq!(restored.max_retry_attempts, 3);
  assert_eq!(restored.initial_backoff_ms, 500);
  assert_eq!(restored.max_backoff_secs, 120);
}

/// BufferConfig with max_memory_mb=256, lru=false -> TOML roundtrip.
#[test]
fn test_buffer_config_custom_values_roundtrip() {
  let config = BufferConfig {
    max_memory_mb: 256,
    lru_eviction_enabled: false
  };

  let toml_str = toml::to_string(&config).expect("serialize BufferConfig");
  let restored: BufferConfig = toml::from_str(&toml_str).expect("deserialize BufferConfig");

  assert_eq!(restored.max_memory_mb, 256);
  assert!(!restored.lru_eviction_enabled);
}

/// FuseMountOptions with allow_other=true, read_only=true, custom fs_name -> TOML roundtrip.
#[test]
fn test_fuse_mount_options_all_flags() {
  let opts = FuseMountOptions {
    fs_name: "customfs".to_string(),
    allow_other: true,
    read_only: true
  };

  let toml_str = toml::to_string(&opts).expect("serialize FuseMountOptions");
  let restored: FuseMountOptions = toml::from_str(&toml_str).expect("deserialize FuseMountOptions");

  assert_eq!(restored.fs_name, "customfs");
  assert!(restored.allow_other);
  assert!(restored.read_only);
}

/// LoggingConfig with level="trace", log_file=Some("/var/log/of.log") -> TOML roundtrip.
#[test]
fn test_logging_config_with_file() {
  let config = LoggingConfig {
    level: "trace".to_string(),
    log_file: Some(PathBuf::from("/var/log/of.log"))
  };

  let toml_str = toml::to_string(&config).expect("serialize LoggingConfig");
  let restored: LoggingConfig = toml::from_str(&toml_str).expect("deserialize LoggingConfig");

  assert_eq!(restored.level, "trace");
  assert_eq!(restored.log_file, Some(PathBuf::from("/var/log/of.log")));
}

/// Empty TOML string -> parse MountConfig should fail (missing required fields).
#[test]
fn test_empty_toml_section_fails() {
  let result: Result<MountConfig, _> = toml::from_str("");
  assert!(result.is_err(), "empty TOML should fail to parse as MountConfig");
}

/// Parse a realistic multi-backend config TOML (like gen-config output).
/// Verify it contains sync, buffer, mount_options, and logging sections.
#[test]
fn test_mount_config_backends_example() {
  let toml_str = r#"
mount_point = "/mnt/wiki"
local_dir = "/var/lib/omnifuse/wiki"

[sync]
debounce_timeout_secs = 2
max_retry_attempts = 5
initial_backoff_ms = 250
max_backoff_secs = 45

[buffer]
max_memory_mb = 1024
lru_eviction_enabled = true

[mount_options]
fs_name = "omniwiki"
allow_other = false
read_only = false

[logging]
level = "warn"
log_file = "/var/log/omnifuse-wiki.log"
"#;

  let config: MountConfig = toml::from_str(toml_str).expect("parse realistic config");

  assert_eq!(config.mount_point, PathBuf::from("/mnt/wiki"));
  assert_eq!(config.local_dir, PathBuf::from("/var/lib/omnifuse/wiki"));

  assert_eq!(config.sync.debounce_timeout_secs, 2);
  assert_eq!(config.sync.max_retry_attempts, 5);
  assert_eq!(config.sync.initial_backoff_ms, 250);
  assert_eq!(config.sync.max_backoff_secs, 45);

  assert_eq!(config.buffer.max_memory_mb, 1024);
  assert!(config.buffer.lru_eviction_enabled);

  assert_eq!(config.mount_options.fs_name, "omniwiki");
  assert!(!config.mount_options.allow_other);
  assert!(!config.mount_options.read_only);

  assert_eq!(config.logging.level, "warn");
  assert_eq!(
    config.logging.log_file,
    Some(PathBuf::from("/var/log/omnifuse-wiki.log"))
  );
}

/// SyncConfig.debounce_timeout() returns Duration::from_secs(debounce_timeout_secs).
#[test]
fn test_sync_config_debounce_timeout_conversion() {
  let config = SyncConfig {
    debounce_timeout_secs: 10,
    ..SyncConfig::default()
  };
  assert_eq!(config.debounce_timeout(), std::time::Duration::from_secs(10));

  let config_zero = SyncConfig {
    debounce_timeout_secs: 0,
    ..SyncConfig::default()
  };
  assert_eq!(config_zero.debounce_timeout(), std::time::Duration::from_secs(0));

  let config_large = SyncConfig {
    debounce_timeout_secs: 3600,
    ..SyncConfig::default()
  };
  assert_eq!(config_large.debounce_timeout(), std::time::Duration::from_secs(3600));
}

/// BufferConfig.max_memory_bytes() = max_memory_mb * 1024 * 1024 for various values.
#[test]
fn test_buffer_config_max_memory_bytes() {
  let config_default = BufferConfig::default();
  assert_eq!(config_default.max_memory_bytes(), 512 * 1024 * 1024);

  let config_256 = BufferConfig {
    max_memory_mb: 256,
    ..BufferConfig::default()
  };
  assert_eq!(config_256.max_memory_bytes(), 256 * 1024 * 1024);

  let config_1 = BufferConfig {
    max_memory_mb: 1,
    ..BufferConfig::default()
  };
  assert_eq!(config_1.max_memory_bytes(), 1 * 1024 * 1024);

  let config_zero = BufferConfig {
    max_memory_mb: 0,
    ..BufferConfig::default()
  };
  assert_eq!(config_zero.max_memory_bytes(), 0);
}
