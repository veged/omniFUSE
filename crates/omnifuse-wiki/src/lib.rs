//! omnifuse-wiki — Wiki backend for `OmniFuse`.
//!
//! Implements the `omnifuse_core::Backend` trait via Wiki HTTP API.
//! Ported from `YaWikiFS`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod client;
pub mod merge;
pub mod meta;
pub mod models;

use std::{
  future::Future,
  path::{Path, PathBuf},
  sync::{Arc, OnceLock},
  time::Duration
};

use omnifuse_core::{Backend, InitResult, RemoteChange, SyncResult};
use tracing::{debug, info, warn};

use crate::{
  client::Client,
  merge::{MergeResult, three_way_merge},
  meta::{MetaStore, PageMeta, path_to_slug},
  models::PageTreeNodeSchema
};

/// Wiki backend configuration.
#[derive(Debug, Clone)]
pub struct WikiConfig {
  /// Base URL of the wiki API.
  pub base_url: String,
  /// Authentication token.
  pub auth_token: String,
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
  local_dir: OnceLock<PathBuf>
}

impl WikiBackend {
  /// Create a new wiki backend.
  ///
  /// # Errors
  ///
  /// Returns an error if the HTTP client cannot be created.
  pub fn new(config: WikiConfig) -> anyhow::Result<Self> {
    let client = Client::new(&config.base_url, &config.auth_token)?;

    Ok(Self {
      config,
      client: Arc::new(client),
      meta_store: OnceLock::new(),
      local_dir: OnceLock::new()
    })
  }

  /// Get the `MetaStore` (after initialization).
  fn meta(&self) -> anyhow::Result<&MetaStore> {
    self
      .meta_store
      .get()
      .ok_or_else(|| anyhow::anyhow!("backend not initialized"))
  }

  /// Get `local_dir` (after initialization).
  fn local_dir(&self) -> anyhow::Result<&Path> {
    self
      .local_dir
      .get()
      .map(PathBuf::as_path)
      .ok_or_else(|| anyhow::anyhow!("backend not initialized"))
  }

  /// Recursively traverse the page tree -> write files + meta.
  fn write_tree<'a>(
    &'a self,
    node: &'a PageTreeNodeSchema,
    local_dir: &'a Path,
    meta_store: &'a MetaStore,
    depth: u32
  ) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<usize>> + Send + 'a>> {
    Box::pin(async move {
      let mut count = 0;

      // Download page content
      match self.client.get_page_by_slug(&node.slug).await {
        Ok(page) => {
          let content = page.content.as_deref().unwrap_or("");

          // Check if content has changed since last time
          let file_path = local_dir.join(format!("{}.md", node.slug));
          let existing = std::fs::read_to_string(&file_path).ok();
          let changed = existing.as_deref() != Some(content);

          // Write .md file
          if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
          }
          std::fs::write(&file_path, content)?;

          // Save meta and base
          let meta = PageMeta {
            id: node.id,
            title: node.title.clone(),
            slug: node.slug.clone(),
            modified_at: node.modified_at.clone()
          };
          meta_store.save_meta(&node.slug, &meta)?;
          meta_store.save_base(&node.slug, content)?;

          if changed {
            count += 1;
          }
          debug!(slug = %node.slug, changed, "page downloaded");
        }
        Err(e) => {
          warn!(slug = %node.slug, error = %e, "failed to download page");
        }
      }

      // Recursively traverse children
      if let Some(children) = &node.children {
        for child in children {
          if depth < self.config.max_depth {
            count += self
              .write_tree(child, local_dir, meta_store, depth + 1)
              .await?;
          }
        }
      }

      Ok(count)
    })
  }

  /// Synchronize a single dirty file.
  async fn sync_file(
    &self,
    path: &Path,
    meta_store: &MetaStore,
    local_dir: &Path
  ) -> anyhow::Result<bool> {
    let Some(slug) = path_to_slug(path, local_dir) else {
      return Ok(false);
    };

    // Read local content
    let local_content = std::fs::read_to_string(path)
      .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;

    // Load base and meta
    let base_content = meta_store.load_base(&slug).unwrap_or_default();
    let Some(page_meta) = meta_store.load_meta(&slug) else {
      // New page — create it
      let title = slug
        .rsplit('/')
        .next()
        .unwrap_or(&slug)
        .replace(['-', '_'], " ");

      let page = self
        .client
        .create_page(&slug, &title, Some(&local_content), "page")
        .await?;

      let new_meta = PageMeta {
        id: page.id,
        title: page.title,
        slug: page.slug.clone(),
        modified_at: page.modified_at
      };
      meta_store.save_meta(&page.slug, &new_meta)?;
      meta_store.save_base(&page.slug, &local_content)?;

      info!(slug = %slug, "page created");
      return Ok(true);
    };

    // Download current remote version
    let remote_page = self.client.get_page_by_slug(&slug).await?;
    let remote_content = remote_page.content.as_deref().unwrap_or("");

    // Check if merge is needed
    if remote_page.modified_at == page_meta.modified_at {
      // Remote unchanged — just PUT
      let updated = self
        .client
        .update_page(page_meta.id, None, Some(&local_content), false)
        .await?;

      let new_meta = PageMeta {
        id: updated.id,
        title: updated.title,
        slug: updated.slug.clone(),
        modified_at: updated.modified_at
      };
      meta_store.save_meta(&slug, &new_meta)?;
      meta_store.save_base(&slug, &local_content)?;

      debug!(slug = %slug, "page updated (no conflict)");
      return Ok(true);
    }

    // Remote changed — three-way merge
    match three_way_merge(&base_content, &local_content, remote_content) {
      MergeResult::NoConflict => {
        // Update base to remote
        let new_meta = PageMeta {
          id: remote_page.id,
          title: remote_page.title,
          slug: remote_page.slug.clone(),
          modified_at: remote_page.modified_at
        };
        meta_store.save_meta(&slug, &new_meta)?;
        meta_store.save_base(&slug, remote_content)?;

        debug!(slug = %slug, "no conflict (remote == local)");
        Ok(true)
      }
      MergeResult::Merged(merged) => {
        // Push merged content
        let updated = self
          .client
          .update_page(page_meta.id, None, Some(&merged), true)
          .await?;

        let new_meta = PageMeta {
          id: updated.id,
          title: updated.title,
          slug: updated.slug.clone(),
          modified_at: updated.modified_at
        };
        meta_store.save_meta(&slug, &new_meta)?;
        meta_store.save_base(&slug, &merged)?;

        // Update local file with merged content
        std::fs::write(path, &merged)?;

        info!(slug = %slug, "merge successful");
        Ok(true)
      }
      MergeResult::Failed { .. } => {
        warn!(slug = %slug, "conflict: local wins");
        // Strategy: local wins (write local version)
        let updated = self
          .client
          .update_page(page_meta.id, None, Some(&local_content), true)
          .await?;

        let new_meta = PageMeta {
          id: updated.id,
          title: updated.title,
          slug: updated.slug.clone(),
          modified_at: updated.modified_at
        };
        meta_store.save_meta(&slug, &new_meta)?;
        meta_store.save_base(&slug, &local_content)?;

        Ok(false) // Conflict occurred
      }
    }
  }
}

impl Backend for WikiBackend {
  async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
    // Create local directory
    std::fs::create_dir_all(local_dir)?;

    // Initialize stores
    let meta_store = MetaStore::new(local_dir)?;
    let _ = self.meta_store.set(meta_store);
    let _ = self.local_dir.set(local_dir.to_path_buf());

    let meta_store = self.meta()?;

    // Fetch page tree
    info!(
      root = %self.config.root_slug,
      "loading page tree"
    );

    match self
      .client
      .get_page_tree(
        &self.config.root_slug,
        self.config.max_pages,
        self.config.max_depth
      )
      .await
    {
      Ok(tree) => {
        let count = self
          .write_tree(&tree.root, local_dir, meta_store, 0)
          .await?;
        info!(count, "tree loaded");

        if count > 0 {
          Ok(InitResult::Updated)
        } else {
          Ok(InitResult::UpToDate)
        }
      }
      Err(e) => {
        warn!(error = %e, "failed to load tree (offline?)");

        // Check if there is local data
        let slugs = meta_store.all_slugs()?;
        if slugs.is_empty() {
          anyhow::bail!("no local data and remote is unavailable: {e}");
        }

        Ok(InitResult::Offline)
      }
    }
  }

  async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    let meta_store = self.meta()?;
    let local_dir = self.local_dir()?;

    let mut synced = 0;
    let mut conflicts = Vec::new();

    for path in dirty_files {
      match self.sync_file(path, meta_store, local_dir).await {
        Ok(true) => synced += 1,
        Ok(false) => conflicts.push(path.clone()),
        Err(e) => {
          let msg = e.to_string();
          if msg.contains("page not found") || msg.contains("access denied") {
            warn!(path = %path.display(), error = %e, "skipping file");
          } else if msg.contains("network") || msg.contains("Connection") {
            return Ok(SyncResult::Offline);
          } else {
            return Err(e);
          }
        }
      }
    }

    if conflicts.is_empty() {
      Ok(SyncResult::Success {
        synced_files: synced
      })
    } else {
      Ok(SyncResult::Conflict {
        synced_files: synced,
        conflict_files: conflicts
      })
    }
  }

  async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
    let meta_store = self.meta()?;
    let local_dir = self.local_dir()?;

    // Fetch current tree
    let tree = self
      .client
      .get_page_tree(
        &self.config.root_slug,
        self.config.max_pages,
        self.config.max_depth
      )
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
    let is_vfs = path
      .components()
      .any(|c| c.as_os_str() == ".vfs");

    is_md && !is_vfs
  }

  fn poll_interval(&self) -> Duration {
    Duration::from_secs(self.config.poll_interval_secs)
  }

  async fn is_online(&self) -> bool {
    self
      .client
      .get_page_tree(&self.config.root_slug, 1, 0)
      .await
      .is_ok()
  }

  fn name(&self) -> &'static str {
    "wiki"
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
        Box::pin(Self::collect_changes(
          child, meta_store, local_dir, client, changes
        ))
        .await;
      }
    }
  }
}
