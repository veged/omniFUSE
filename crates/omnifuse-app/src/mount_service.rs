//! Application-facing mount service.

use std::path::PathBuf;

use anyhow::Context;
use omnifuse_core::{BufferConfig, FuseMountOptions, LoggingConfig, MountConfig, Sink, SyncConfig};
use omnifuse_git::{GitBackend, GitConfig};
use omnifuse_s3::{S3Backend, S3Config};
use omnifuse_wiki::{WikiBackend, WikiConfig};

use crate::{CacheKey, MountEnvironment, MountLayout, StdMountEnvironment};

/// Default values used for application-level mount preparation.
#[derive(Debug, Clone)]
pub struct MountDefaults {
  /// Default Git branch.
  pub git_branch: String,
  /// Default Git remote polling interval in seconds.
  pub git_poll_interval_secs: u64,
  /// Default Wiki remote polling interval in seconds.
  pub wiki_poll_interval_secs: u64,
  /// Default S3 remote polling interval in seconds.
  pub s3_poll_interval_secs: u64,
  /// Maximum number of Git push retries.
  pub git_max_push_retries: u32,
  /// Maximum Wiki tree depth.
  pub wiki_max_depth: u32,
  /// Maximum number of Wiki pages fetched during tree loading.
  pub wiki_max_pages: u32
}

impl Default for MountDefaults {
  fn default() -> Self {
    Self {
      git_branch: "main".to_string(),
      git_poll_interval_secs: 30,
      wiki_poll_interval_secs: 60,
      s3_poll_interval_secs: 60,
      git_max_push_retries: 3,
      wiki_max_depth: 10,
      wiki_max_pages: 500
    }
  }
}

/// Git mount request.
#[derive(Debug, Clone)]
pub struct GitMountArgs {
  /// Source URL or local repository path.
  pub source: String,
  /// User-visible mount point.
  pub mount_point: PathBuf,
  /// Git branch. Defaults to `main`.
  pub branch: Option<String>,
  /// Remote polling interval in seconds. Defaults to 30.
  pub poll_interval_secs: Option<u64>,
  /// Allow access by other users.
  pub allow_other: bool,
  /// Mount as read-only.
  pub read_only: bool
}

/// Wiki mount request.
#[derive(Debug, Clone)]
pub struct WikiMountArgs {
  /// Base URL of the Wiki API.
  pub base_url: String,
  /// Root slug.
  pub root_slug: String,
  /// Authentication token.
  pub auth_token: String,
  /// Organization ID header value.
  pub org_id: Option<String>,
  /// User-visible mount point.
  pub mount_point: PathBuf,
  /// Remote polling interval in seconds. Defaults to 60.
  pub poll_interval_secs: Option<u64>,
  /// Allow access by other users.
  pub allow_other: bool,
  /// Mount as read-only.
  pub read_only: bool
}

/// S3-compatible mount request.
#[derive(Debug, Clone)]
pub struct S3MountArgs {
  /// Bucket name.
  pub bucket: String,
  /// User-visible mount point.
  pub mount_point: PathBuf,
  /// Object prefix mounted as root.
  pub prefix: Option<String>,
  /// S3-compatible endpoint URL.
  pub endpoint: Option<String>,
  /// Region value.
  pub region: Option<String>,
  /// Access key ID.
  pub access_key_id: Option<String>,
  /// Secret access key.
  pub secret_access_key: Option<String>,
  /// Session token.
  pub session_token: Option<String>,
  /// Use virtual-hosted style.
  pub virtual_host_style: bool,
  /// Remote polling interval in seconds. Defaults to 60.
  pub poll_interval_secs: Option<u64>,
  /// Allow access by other users.
  pub allow_other: bool,
  /// Mount as read-only.
  pub read_only: bool
}

/// Prepared mount configuration and backend.
pub struct PreparedMount<B> {
  /// Core mount config.
  pub config: MountConfig,
  /// Backend instance.
  pub backend: B,
  /// Resolved path layout.
  pub layout: MountLayout
}

/// Application-level mount service shared by CLI and GUI.
#[derive(Debug, Clone)]
pub struct MountService<E = StdMountEnvironment> {
  env: E,
  defaults: MountDefaults
}

impl Default for MountService<StdMountEnvironment> {
  fn default() -> Self {
    Self::new(StdMountEnvironment)
  }
}

impl<E: MountEnvironment> MountService<E> {
  /// Create a service with default mount settings.
  #[must_use]
  pub fn new(env: E) -> Self {
    Self {
      env,
      defaults: MountDefaults::default()
    }
  }

  /// Create a service with explicit defaults.
  #[must_use]
  pub const fn with_defaults(env: E, defaults: MountDefaults) -> Self {
    Self { env, defaults }
  }

  /// Prepare a Git mount without starting FUSE.
  ///
  /// # Errors
  ///
  /// Returns an error if mount paths cannot be resolved.
  pub fn prepare_git(&self, args: GitMountArgs) -> anyhow::Result<PreparedMount<GitBackend>> {
    let branch = args.branch.unwrap_or_else(|| self.defaults.git_branch.clone());
    let poll_interval_secs = args.poll_interval_secs.unwrap_or(self.defaults.git_poll_interval_secs);
    let layout = MountLayout::resolve(
      &self.env,
      &args.mount_point,
      &CacheKey::new("git", format!("{}:{branch}", args.source))
    )?;

    let config = mount_config(&layout, "omnifuse-git", args.allow_other, args.read_only);
    let backend = GitBackend::new(GitConfig {
      source: args.source,
      branch,
      max_push_retries: self.defaults.git_max_push_retries,
      poll_interval_secs,
      local_dir: layout.work_dir.clone()
    });

    Ok(PreparedMount {
      config,
      backend,
      layout
    })
  }

  /// Prepare a Wiki mount without starting FUSE.
  ///
  /// # Errors
  ///
  /// Returns an error if mount paths cannot be resolved or the Wiki backend cannot be created.
  pub fn prepare_wiki(&self, args: WikiMountArgs) -> anyhow::Result<PreparedMount<WikiBackend>> {
    let poll_interval_secs = args.poll_interval_secs.unwrap_or(self.defaults.wiki_poll_interval_secs);
    let layout = MountLayout::resolve(
      &self.env,
      &args.mount_point,
      &CacheKey::new(
        "wiki",
        wiki_cache_identity(&args.base_url, &args.root_slug, args.org_id.as_deref())
      )
    )?;

    let config = mount_config(&layout, "omnifuse-wiki", args.allow_other, args.read_only);
    let backend = WikiBackend::new(WikiConfig {
      base_url: args.base_url,
      auth_token: args.auth_token,
      org_id: args.org_id,
      root_slug: args.root_slug,
      poll_interval_secs,
      max_depth: self.defaults.wiki_max_depth,
      max_pages: self.defaults.wiki_max_pages
    })
    .context("failed to create wiki backend")?;

    Ok(PreparedMount {
      config,
      backend,
      layout
    })
  }

  /// Prepare and run a Git mount.
  ///
  /// # Errors
  ///
  /// Returns an error if preparation or mounting fails.
  pub async fn run_git(&self, args: GitMountArgs, events: impl Sink) -> anyhow::Result<()>
  where
    E: Sync
  {
    let prepared = self.prepare_git(args)?;
    omnifuse_core::run_mount(prepared.config, prepared.backend, events).await
  }

  /// Prepare and run a Wiki mount.
  ///
  /// # Errors
  ///
  /// Returns an error if preparation or mounting fails.
  pub async fn run_wiki(&self, args: WikiMountArgs, events: impl Sink) -> anyhow::Result<()>
  where
    E: Sync
  {
    let prepared = self.prepare_wiki(args)?;
    omnifuse_core::run_mount(prepared.config, prepared.backend, events).await
  }

  /// Prepare an S3 mount without starting FUSE.
  ///
  /// # Errors
  ///
  /// Returns an error if mount paths cannot be resolved.
  pub fn prepare_s3(&self, args: S3MountArgs) -> anyhow::Result<PreparedMount<S3Backend>> {
    let poll_interval_secs = args.poll_interval_secs.unwrap_or(self.defaults.s3_poll_interval_secs);
    let prefix = args.prefix.unwrap_or_default();
    let identity = s3_cache_identity(&args.bucket, &prefix, args.endpoint.as_deref(), args.region.as_deref());
    let layout = MountLayout::resolve(&self.env, &args.mount_point, &CacheKey::new("s3", identity))?;
    let config = mount_config(&layout, "omnifuse-s3", args.allow_other, args.read_only);
    let backend = S3Backend::new(S3Config {
      bucket: args.bucket,
      prefix,
      endpoint: args.endpoint,
      region: args.region,
      access_key_id: args.access_key_id,
      secret_access_key: args.secret_access_key,
      session_token: args.session_token,
      virtual_host_style: args.virtual_host_style,
      poll_interval_secs
    });

    Ok(PreparedMount {
      config,
      backend,
      layout
    })
  }

  /// Prepare and run an S3 mount.
  ///
  /// # Errors
  ///
  /// Returns an error if preparation or mounting fails.
  pub async fn run_s3(&self, args: S3MountArgs, events: impl Sink) -> anyhow::Result<()>
  where
    E: Sync
  {
    let prepared = self.prepare_s3(args)?;
    omnifuse_core::run_mount(prepared.config, prepared.backend, events).await
  }
}

fn s3_cache_identity(bucket: &str, prefix: &str, endpoint: Option<&str>, region: Option<&str>) -> String {
  format!(
    "{}:{}:{}:{}",
    endpoint.unwrap_or(""),
    region.unwrap_or(""),
    bucket,
    prefix.trim_matches('/')
  )
}

fn mount_config(layout: &MountLayout, fs_name: &str, allow_other: bool, read_only: bool) -> MountConfig {
  MountConfig {
    mount_point: layout.mount_point.clone(),
    local_dir: layout.work_dir.clone(),
    sync: SyncConfig::default(),
    buffer: BufferConfig::default(),
    mount_options: FuseMountOptions {
      fs_name: fs_name.to_string(),
      allow_other,
      read_only
    },
    logging: LoggingConfig::default()
  }
}

fn wiki_cache_identity(base_url: &str, root_slug: &str, org_id: Option<&str>) -> String {
  format!(
    "{}:{}:{}",
    base_url.trim_end_matches('/'),
    org_id.unwrap_or(""),
    root_slug
  )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use std::path::PathBuf;

  use crate::{GitMountArgs, MountService, S3MountArgs, WikiMountArgs, environment::FakeMountEnvironment};

  #[test]
  fn prepare_git_builds_consistent_mount_and_backend_config() {
    let service = MountService::new(
      FakeMountEnvironment::new()
        .home("/home/user")
        .canonical("/mnt/repo", "/abs/mnt/repo")
    );

    let prepared = service
      .prepare_git(GitMountArgs {
        source: "https://example.test/repo.git".to_string(),
        mount_point: PathBuf::from("/mnt/repo"),
        branch: Some("main".to_string()),
        poll_interval_secs: Some(15),
        allow_other: true,
        read_only: false
      })
      .expect("prepared");

    assert_eq!(prepared.config.mount_point, PathBuf::from("/abs/mnt/repo"));
    assert_eq!(prepared.config.local_dir, prepared.layout.work_dir);
    assert_eq!(prepared.config.mount_options.fs_name, "omnifuse-git");
    assert!(prepared.config.mount_options.allow_other);
  }

  #[test]
  fn prepare_wiki_uses_same_layout_rules_as_git() {
    let service = MountService::new(
      FakeMountEnvironment::new()
        .home("/home/user")
        .canonical("/mnt/wiki", "/abs/mnt/wiki")
    );

    let prepared = service
      .prepare_wiki(WikiMountArgs {
        base_url: "https://api.wiki.example.test".to_string(),
        root_slug: "root".to_string(),
        auth_token: "token".to_string(),
        org_id: Some("org".to_string()),
        mount_point: PathBuf::from("/mnt/wiki"),
        poll_interval_secs: Some(60),
        allow_other: false,
        read_only: false
      })
      .expect("prepared");

    assert_eq!(prepared.config.mount_point, PathBuf::from("/abs/mnt/wiki"));
    assert_eq!(prepared.config.local_dir, prepared.layout.work_dir);
    assert_eq!(prepared.config.mount_options.fs_name, "omnifuse-wiki");
  }

  #[test]
  fn prepare_s3_uses_s3_layout_and_fs_name() {
    let service = MountService::new(
      FakeMountEnvironment::new()
        .home("/home/user")
        .canonical("/mnt/s3", "/abs/mnt/s3")
    );

    let prepared = service
      .prepare_s3(S3MountArgs {
        bucket: "test-bucket".to_string(),
        mount_point: PathBuf::from("/mnt/s3"),
        prefix: Some("project".to_string()),
        endpoint: Some("https://s3.example.test".to_string()),
        region: Some("us-east-1".to_string()),
        access_key_id: Some("AKIA".to_string()),
        secret_access_key: Some("secret".to_string()),
        session_token: None,
        virtual_host_style: false,
        poll_interval_secs: Some(45),
        allow_other: false,
        read_only: false
      })
      .expect("prepared");

    assert_eq!(prepared.config.mount_point, PathBuf::from("/abs/mnt/s3"));
    assert_eq!(prepared.config.local_dir, prepared.layout.work_dir);
    assert_eq!(prepared.config.mount_options.fs_name, "omnifuse-s3");
  }
}
