//! omnifuse-wiki — Wiki backend for `OmniFuse`.
//!
//! Implements the `omnifuse_core::Backend` trait via Wiki HTTP API.
//! Ported from `YaWikiFS`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod client;
pub mod error;
pub mod merge;
pub mod meta;
pub mod models;
/// Wiki page identity and local path mapping.
pub mod page;
/// Bidirectional index of known wiki pages.
pub mod page_index;
/// Wiki page synchronization session.
pub mod session;

use std::{
  path::{Path, PathBuf},
  sync::{Arc, OnceLock},
  time::Duration
};

pub use error::{WikiError, classify_wiki_error};
use omnifuse_core::{Backend, InitResult, RemoteRefresh, RemoteRefreshResult, SyncResult};

use crate::{
  client::Client,
  session::{DirtyBatch, WikiPageSyncSession}
};

/// Wiki backend configuration.
#[derive(Debug, Clone)]
pub struct WikiConfig {
  /// Base URL of the wiki API.
  pub base_url: String,
  /// Authentication token.
  pub auth_token: String,
  /// Organization ID (`X-Org-Id` header, required for Yandex 360 Wiki).
  pub org_id: Option<String>,
  /// Root slug (the tree is built from it).
  pub root_slug: String,
  /// Remote polling interval (seconds).
  pub poll_interval_secs: u64,
  /// Maximum tree depth.
  pub max_depth: u32,
  /// Maximum number of pages when fetching the tree.
  pub max_pages: u32
}

impl Default for WikiConfig {
  fn default() -> Self {
    Self {
      base_url: String::new(),
      auth_token: String::new(),
      org_id: None,
      root_slug: String::new(),
      poll_interval_secs: 60,
      max_depth: 10,
      max_pages: 500
    }
  }
}

/// Wiki backend for `OmniFuse`.
///
/// Adapts the core `Backend` trait to `WikiPageSyncSession`.
pub struct WikiBackend {
  /// Configuration.
  config: WikiConfig,
  /// HTTP client shared with the page sync session.
  client: Arc<Client>,
  /// Page synchronization session (initialized in `init`).
  session: OnceLock<WikiPageSyncSession>
}

impl WikiBackend {
  /// Create a new wiki backend.
  ///
  /// # Errors
  ///
  /// Returns an error if the HTTP client cannot be created.
  pub fn new(config: WikiConfig) -> anyhow::Result<Self> {
    let client = Client::new(&config.base_url, &config.auth_token, config.org_id.as_deref())?;

    Ok(Self {
      config,
      client: Arc::new(client),
      session: OnceLock::new()
    })
  }

  /// Get initialized page sync session.
  fn session(&self) -> anyhow::Result<&WikiPageSyncSession> {
    self.session.get().ok_or_else(|| WikiError::NotInitialized.into())
  }
}

impl Backend for WikiBackend {
  async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
    if self.session.get().is_none() {
      let session = WikiPageSyncSession::attach(self.config.clone(), self.client.clone(), local_dir)?;
      let _ = self.session.set(session);
    }

    self.session()?.initialize().await
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    self.session()?.sync_dirty(DirtyBatch { paths: dirty_files }).await
  }

  async fn refresh_remote(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    self.session()?.refresh_remote(request).await
  }

  fn should_track(&self, path: &Path) -> bool {
    // Only .md files
    let is_md = path.extension().is_some_and(|e| e == "md");
    // Exclude .vfs/
    let is_vfs = path.components().any(|c| c.as_os_str() == ".vfs");

    is_md && !is_vfs
  }

  fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.config.poll_interval_secs)
  }

  async fn is_online(&self) -> bool {
    self.client.get_page_tree(&self.config.root_slug, 1, 0).await.is_ok()
  }

  fn name(&self) -> &'static str {
    "wiki"
  }

  fn classify_error(&self, error: &anyhow::Error) -> omnifuse_core::Code {
    classify_wiki_error(error).unwrap_or(omnifuse_core::Code::Internal)
  }
}
