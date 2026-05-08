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
use omnifuse_core::{Backend, InitResult, RemoteChange, SyncResult};
use tracing::{debug, info, warn};

use crate::{
  client::Client,
  meta::{MetaStore, PageMeta, path_to_slug},
  models::PageTreeNodeSchema,
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
/// Implements the `Backend` trait: init -> fetch tree, sync -> merge+PUT,
/// poll -> compare `modified_at`, apply -> download modified pages.
pub struct WikiBackend {
  /// Configuration.
  config: WikiConfig,
  /// HTTP client (initialized in `new`).
  client: Arc<Client>,
  /// Metadata store (initialized in `init`).
  meta_store: OnceLock<MetaStore>,
  /// Local directory (initialized in `init`).
  local_dir: OnceLock<PathBuf>,
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
      meta_store: OnceLock::new(),
      local_dir: OnceLock::new(),
      session: OnceLock::new()
    })
  }

  /// Get the `MetaStore` (after initialization).
  fn meta(&self) -> anyhow::Result<&MetaStore> {
    self.meta_store.get().ok_or_else(|| WikiError::NotInitialized.into())
  }

  /// Get `local_dir` (after initialization).
  fn local_dir(&self) -> anyhow::Result<&Path> {
    self
      .local_dir
      .get()
      .map(PathBuf::as_path)
      .ok_or_else(|| WikiError::NotInitialized.into())
  }

  /// Get initialized page sync session.
  fn session(&self) -> anyhow::Result<&WikiPageSyncSession> {
    self.session.get().ok_or_else(|| WikiError::NotInitialized.into())
  }
}

impl Backend for WikiBackend {
  async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
    let local_dir = if local_dir.is_absolute() {
      local_dir.to_path_buf()
    } else {
      std::env::current_dir()?.join(local_dir)
    };

    std::fs::create_dir_all(&local_dir)?;
    let meta_store = MetaStore::new(&local_dir)?;
    let _ = self.meta_store.set(meta_store);
    let _ = self.local_dir.set(local_dir.clone());

    if self.session.get().is_none() {
      let session = WikiPageSyncSession::attach(self.config.clone(), self.client.clone(), &local_dir).await?;
      let _ = self.session.set(session);
    }

    self.session()?.initialize().await
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    self.session()?.sync_dirty(DirtyBatch { paths: dirty_files }).await
  }

  async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
    let meta_store = self.meta()?;
    let local_dir = self.local_dir()?;

    // Fetch current tree
    let tree = self
      .client
      .get_page_tree(&self.config.root_slug, self.config.max_pages, self.config.max_depth)
      .await?;

    let mut changes = Vec::new();
    Self::collect_changes(&tree.root, meta_store, local_dir, &self.client, &mut changes).await;

    if !changes.is_empty() {
      info!(count = changes.len(), "remote changes detected");
    }

    Ok(changes)
  }

  async fn apply_remote(&self, changes: Vec<RemoteChange>) -> anyhow::Result<()> {
    let meta_store = self.meta()?;
    let local_dir = self.local_dir()?;

    for change in changes {
      match change {
        RemoteChange::Modified { path, content } => {
          // Write file
          if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
          }
          std::fs::write(&path, &content)?;

          // Update base
          if let Some(slug) = path_to_slug(&path, local_dir) {
            let content_str = String::from_utf8_lossy(&content);
            meta_store.save_base(&slug, &content_str)?;
          }

          debug!(path = %path.display(), "remote change applied");
        }
        RemoteChange::Deleted { path } => {
          let _ = std::fs::remove_file(&path);

          if let Some(slug) = path_to_slug(&path, local_dir) {
            meta_store.remove(&slug);
          }

          debug!(path = %path.display(), "file deleted (remote)");
        }
      }
    }

    Ok(())
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

  fn classify_error(&self, error: &anyhow::Error) -> omnifuse_core::ErrorKind {
    classify_wiki_error(error).unwrap_or(omnifuse_core::ErrorKind::Internal)
  }
}

impl WikiBackend {
  /// Recursively collect changes from the tree.
  async fn collect_changes(
    node: &PageTreeNodeSchema,
    meta_store: &MetaStore,
    local_dir: &Path,
    client: &Client,
    changes: &mut Vec<RemoteChange>
  ) {
    let local_meta = meta_store.load_meta(&node.slug);

    // Check if modified_at has changed
    let needs_update = local_meta
      .as_ref()
      .is_none_or(|meta| meta.modified_at != node.modified_at);

    if needs_update {
      // Download content
      if let Ok(page) = client.get_page_by_slug(&node.slug).await {
        let content = page.content.unwrap_or_default();
        let file_path = local_dir.join(format!("{}.md", node.slug));

        changes.push(RemoteChange::Modified {
          path: file_path,
          content: content.into_bytes()
        });

        // Update meta
        let meta = PageMeta {
          id: node.id,
          title: node.title.clone(),
          slug: node.slug.clone(),
          modified_at: node.modified_at.clone()
        };
        let _ = meta_store.save_meta(&node.slug, &meta);
      }
    }

    // Recurse into children
    if let Some(children) = &node.children {
      for child in children {
        Box::pin(Self::collect_changes(child, meta_store, local_dir, client, changes)).await;
      }
    }
  }
}
