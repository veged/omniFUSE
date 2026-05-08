//! Wiki page synchronization session.

use std::{
  path::{Path, PathBuf},
  sync::Arc
};

use omnifuse_core::{
  InitResult, RemoteApplyMode, RemoteChange, RemoteDeferReason, RemoteRefresh, RemoteRefreshResult, SyncResult
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::{
  WikiConfig,
  client::Client,
  error::classify_wiki_error,
  merge::{MergeDecision, decide_merge},
  meta::{MetaStore, PageMeta},
  models::{PageFullDetailsSchema, PageTreeNodeSchema},
  page::{PageRef, PageSnapshot, Slug},
  page_index::PageIndex
};

/// Dirty local file batch.
pub struct DirtyBatch<'a> {
  /// Absolute local paths reported as dirty by the VFS layer.
  pub paths: &'a [PathBuf]
}

/// Remote changes plus wiki snapshots needed for coherent apply.
#[derive(Debug, Clone, Default)]
pub struct RemoteBatch {
  /// Core-level file changes.
  pub changes: Vec<RemoteChange>,
  /// Wiki page snapshots matching modified changes.
  pub snapshots: Vec<(PageRef, PageSnapshot)>
}

impl RemoteBatch {
  /// Build a compatibility batch from core changes without wiki snapshots.
  #[must_use]
  pub fn from_changes(changes: Vec<RemoteChange>) -> Self {
    Self {
      changes,
      snapshots: Vec::new()
    }
  }

  fn snapshot_for_path(&self, path: &Path) -> Option<(&PageRef, &PageSnapshot)> {
    self
      .snapshots
      .iter()
      .find_map(|(page_ref, snapshot)| (page_ref.path == path).then_some((page_ref, snapshot)))
  }
}

/// Result of applying a remote batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
  /// Modified files written locally.
  pub modified: usize,
  /// Deleted files removed locally.
  pub deleted: usize
}

/// Wiki page sync state bound to a concrete local directory.
pub struct WikiPageSyncSession {
  config: WikiConfig,
  client: Arc<Client>,
  local_dir: PathBuf,
  meta_store: MetaStore,
  page_index: RwLock<PageIndex>
}

impl WikiPageSyncSession {
  /// Attach a session to an existing local directory without network IO.
  ///
  /// # Errors
  ///
  /// Returns an error if the local metadata store cannot be created.
  pub async fn attach(config: WikiConfig, client: Arc<Client>, local_dir: &Path) -> anyhow::Result<Self> {
    let local_dir = absolute_local_dir(local_dir)?;
    let meta_store = MetaStore::new(&local_dir)?;

    Ok(Self {
      config,
      client,
      local_dir,
      meta_store,
      page_index: RwLock::new(PageIndex::default())
    })
  }

  /// Download the configured remote tree into local files, base content and metadata.
  ///
  /// # Errors
  ///
  /// Returns an error when remote data is unavailable and there is no local metadata fallback,
  /// or when local files/metadata cannot be written.
  pub async fn initialize(&self) -> anyhow::Result<InitResult> {
    std::fs::create_dir_all(&self.local_dir)?;

    info!(
      root = %self.config.root_slug,
      "loading page tree"
    );

    let tree = match self
      .client
      .get_page_tree(&self.config.root_slug, self.config.max_pages, self.config.max_depth)
      .await
    {
      Ok(tree) => tree,
      Err(e) => {
        warn!(error = %e, "failed to load tree (offline?)");
        if self.meta_store.all_slugs()?.is_empty() {
          anyhow::bail!("no local data and remote is unavailable: {e}");
        }

        return Ok(InitResult::Offline);
      }
    };

    let mut nodes = Vec::new();
    collect_tree_nodes(&tree.root, self.config.max_depth, 0, &mut nodes);

    let mut index = PageIndex::default();
    let mut changed = 0;

    for node in nodes {
      if self.materialize_node(node, &mut index).await? {
        changed += 1;
      }
    }

    *self.page_index.write().await = index;
    info!(count = changed, "tree loaded");

    if changed > 0 {
      Ok(InitResult::Updated)
    } else {
      Ok(InitResult::UpToDate)
    }
  }

  /// Synchronize dirty local markdown files to remote wiki pages.
  ///
  /// # Errors
  ///
  /// Returns an error when local file reading fails or when an unexpected remote/API error occurs.
  pub async fn sync_dirty(&self, dirty: DirtyBatch<'_>) -> anyhow::Result<SyncResult> {
    let mut synced = 0;
    let mut conflicts = Vec::new();

    for path in dirty.paths {
      match self.sync_dirty_file(path).await {
        Ok(Some(true)) => synced += 1,
        Ok(Some(false)) => conflicts.push(path.clone()),
        Ok(None) => {}
        Err(e) => match classify_wiki_error(&e) {
          Some(omnifuse_core::ErrorKind::NotFound | omnifuse_core::ErrorKind::PermissionDenied) => {
            warn!(path = %path.display(), error = %e, "skipping file");
          }
          Some(omnifuse_core::ErrorKind::Offline) => return Ok(SyncResult::Offline),
          Some(omnifuse_core::ErrorKind::Conflict) => conflicts.push(path.clone()),
          _ => return Err(e)
        }
      }
    }

    if conflicts.is_empty() {
      Ok(SyncResult::Success { synced_files: synced })
    } else {
      Ok(SyncResult::Conflict {
        synced_files: synced,
        conflict_files: conflicts
      })
    }
  }

  /// Poll remote wiki tree and return a batch with content and page snapshots.
  ///
  /// # Errors
  ///
  /// Returns an error when the remote tree or modified page content cannot be fetched.
  pub async fn poll_remote(&self) -> anyhow::Result<RemoteBatch> {
    let tree = self
      .client
      .get_page_tree(&self.config.root_slug, self.config.max_pages, self.config.max_depth)
      .await?;

    let mut nodes = Vec::new();
    collect_tree_nodes(&tree.root, self.config.max_depth, 0, &mut nodes);

    let mut batch = RemoteBatch::default();
    for node in nodes {
      let page_ref = PageRef::from_slug(&self.local_dir, Slug::new(&node.slug))
        .ok_or_else(|| anyhow::anyhow!("invalid wiki page slug: {}", node.slug))?;
      let local_snapshot = self.snapshot_for(&page_ref).await;
      let needs_update = local_snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.modified_at != node.modified_at);

      if needs_update {
        let page = self.client.get_page_by_slug(page_ref.slug.as_str()).await?;
        let content = page.content.unwrap_or_default();
        let snapshot = PageSnapshot {
          id: node.id,
          title: node.title.clone(),
          modified_at: node.modified_at.clone()
        };

        batch.changes.push(RemoteChange::Modified {
          path: page_ref.path.clone(),
          content: content.into_bytes()
        });
        batch.snapshots.push((page_ref, snapshot));
      }
    }

    if !batch.changes.is_empty() {
      info!(count = batch.changes.len(), "remote changes detected");
    }

    Ok(batch)
  }

  /// Detect remote changes, respect protected paths, and apply safe refreshes.
  ///
  /// # Errors
  ///
  /// Returns an error when remote polling or local apply fails.
  pub async fn refresh_remote(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    let batch = self.poll_remote().await?;
    if batch.changes.is_empty() {
      return Ok(RemoteRefreshResult::Unchanged);
    }

    let protected: Vec<PathBuf> = batch
      .changes
      .iter()
      .map(|change| change.path().to_path_buf())
      .filter(|path| request.protected_paths.is_protected(path))
      .collect();
    if !protected.is_empty() {
      return Ok(RemoteRefreshResult::Deferred {
        affected: protected,
        reason: RemoteDeferReason::ProtectedLocalChange
      });
    }

    let changed: Vec<PathBuf> = batch
      .changes
      .iter()
      .filter_map(|change| match change {
        RemoteChange::Modified { path, .. } => Some(path.clone()),
        RemoteChange::Deleted { .. } => None
      })
      .collect();
    let deleted: Vec<PathBuf> = batch
      .changes
      .iter()
      .filter_map(|change| match change {
        RemoteChange::Modified { .. } => None,
        RemoteChange::Deleted { path } => Some(path.clone())
      })
      .collect();

    if matches!(request.mode, RemoteApplyMode::DetectOnly) {
      let mut affected = changed.clone();
      affected.extend(deleted.iter().cloned());
      return Ok(RemoteRefreshResult::Deferred {
        affected,
        reason: RemoteDeferReason::DetectOnly
      });
    }

    self.apply_remote(batch).await?;
    Ok(RemoteRefreshResult::Applied { changed, deleted })
  }

  /// Apply a previously polled remote batch to local files, base content, metadata and index.
  ///
  /// # Errors
  ///
  /// Returns an error when local files or metadata cannot be updated.
  pub async fn apply_remote(&self, batch: RemoteBatch) -> anyhow::Result<ApplyReport> {
    let mut report = ApplyReport::default();

    for change in &batch.changes {
      match change {
        RemoteChange::Modified { path, content } => {
          if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
          }
          std::fs::write(path, content)?;

          let content = String::from_utf8_lossy(content);
          if let Some((page_ref, snapshot)) = batch.snapshot_for_path(path) {
            self.save_page_state(page_ref, snapshot.clone(), &content).await?;
          } else if let Some(page_ref) = PageRef::from_path(&self.local_dir, path) {
            self.meta_store.save_base(page_ref.slug.as_str(), &content)?;
          }

          report.modified += 1;
          debug!(path = %path.display(), "remote change applied");
        }
        RemoteChange::Deleted { path } => {
          let _ = std::fs::remove_file(path);

          if let Some(page_ref) = PageRef::from_path(&self.local_dir, path) {
            self.meta_store.remove(page_ref.slug.as_str());
            self.page_index.write().await.remove_path(path);
          }

          report.deleted += 1;
          debug!(path = %path.display(), "file deleted (remote)");
        }
      }
    }

    Ok(report)
  }

  async fn materialize_node(&self, node: &PageTreeNodeSchema, index: &mut PageIndex) -> anyhow::Result<bool> {
    let page_ref = PageRef::from_slug(&self.local_dir, Slug::new(&node.slug))
      .ok_or_else(|| anyhow::anyhow!("invalid wiki page slug: {}", node.slug))?;

    let page = self.client.get_page_by_slug(page_ref.slug.as_str()).await?;
    let content = page.content.as_deref().unwrap_or("");
    let changed = std::fs::read_to_string(&page_ref.path).map_or(true, |existing| existing != content);

    if let Some(parent) = page_ref.path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&page_ref.path, content)?;

    let snapshot = PageSnapshot {
      id: node.id,
      title: node.title.clone(),
      modified_at: node.modified_at.clone()
    };

    self
      .meta_store
      .save_meta(page_ref.slug.as_str(), &page_meta(&page_ref, &snapshot))?;
    self.meta_store.save_base(page_ref.slug.as_str(), content)?;
    index.insert(page_ref.clone(), snapshot);

    debug!(slug = %page_ref.slug.as_str(), changed, "page downloaded");
    Ok(changed)
  }

  async fn sync_dirty_file(&self, path: &Path) -> anyhow::Result<Option<bool>> {
    let Some(page_ref) = PageRef::from_path(&self.local_dir, path) else {
      return Ok(None);
    };

    let local_content =
      std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;

    let Some(snapshot) = self.snapshot_for(&page_ref).await else {
      self.create_remote_page(&page_ref, &local_content).await?;
      return Ok(Some(true));
    };

    let remote_page = self.client.get_page_by_slug(page_ref.slug.as_str()).await?;
    let remote_content = remote_page.content.as_deref().unwrap_or("");

    if remote_page.modified_at == snapshot.modified_at {
      self
        .update_remote_page(snapshot.id, &page_ref, &local_content, false, false)
        .await?;
      debug!(slug = %page_ref.slug.as_str(), "page updated (no conflict)");
      return Ok(Some(true));
    }

    let base_content = self.meta_store.load_base(page_ref.slug.as_str()).unwrap_or_default();

    match decide_merge(&base_content, &local_content, remote_content) {
      MergeDecision::UploadLocal => {
        self
          .update_remote_page(snapshot.id, &page_ref, &local_content, true, false)
          .await?;
        Ok(Some(true))
      }
      MergeDecision::UploadMerged(merged) => {
        self
          .update_remote_page(snapshot.id, &page_ref, &merged, true, false)
          .await?;
        std::fs::write(path, &merged)?;
        info!(slug = %page_ref.slug.as_str(), "merge successful");
        Ok(Some(true))
      }
      MergeDecision::AcceptRemote(remote) => {
        std::fs::write(path, &remote)?;
        self
          .save_page_state(&page_ref, snapshot_from_page(&remote_page), &remote)
          .await?;
        Ok(Some(true))
      }
      MergeDecision::AlreadySynced => {
        self
          .save_page_state(&page_ref, snapshot_from_page(&remote_page), remote_content)
          .await?;
        Ok(Some(true))
      }
      MergeDecision::Conflict => {
        warn!(slug = %page_ref.slug.as_str(), "conflict: local wins");
        self
          .update_remote_page(snapshot.id, &page_ref, &local_content, true, false)
          .await?;
        Ok(Some(false))
      }
    }
  }

  async fn snapshot_for(&self, page_ref: &PageRef) -> Option<PageSnapshot> {
    if let Some(entry) = self.page_index.read().await.by_path(&page_ref.path) {
      return Some(entry.snapshot.clone());
    }

    self
      .meta_store
      .load_meta(page_ref.slug.as_str())
      .map(|meta| PageSnapshot {
        id: meta.id,
        title: meta.title,
        modified_at: meta.modified_at
      })
  }

  async fn create_remote_page(&self, page_ref: &PageRef, local_content: &str) -> anyhow::Result<()> {
    let page = self
      .client
      .create_page(
        page_ref.slug.as_str(),
        &title_from_slug(&page_ref.slug),
        Some(local_content),
        "page"
      )
      .await?;

    self
      .save_page_state(page_ref, snapshot_from_page(&page), local_content)
      .await?;
    info!(slug = %page_ref.slug.as_str(), "page created");
    Ok(())
  }

  async fn update_remote_page(
    &self,
    id: u64,
    page_ref: &PageRef,
    content: &str,
    allow_merge: bool,
    update_local: bool
  ) -> anyhow::Result<()> {
    let updated = self.client.update_page(id, None, Some(content), allow_merge).await?;
    if update_local {
      std::fs::write(&page_ref.path, content)?;
    }
    self
      .save_page_state(page_ref, snapshot_from_page(&updated), content)
      .await
  }

  async fn save_page_state(&self, page_ref: &PageRef, snapshot: PageSnapshot, content: &str) -> anyhow::Result<()> {
    self
      .meta_store
      .save_meta(page_ref.slug.as_str(), &page_meta(page_ref, &snapshot))?;
    self.meta_store.save_base(page_ref.slug.as_str(), content)?;
    self.page_index.write().await.insert(page_ref.clone(), snapshot);
    Ok(())
  }
}

fn collect_tree_nodes<'a>(
  node: &'a PageTreeNodeSchema,
  max_depth: u32,
  depth: u32,
  nodes: &mut Vec<&'a PageTreeNodeSchema>
) {
  nodes.push(node);

  if depth >= max_depth {
    return;
  }

  if let Some(children) = &node.children {
    for child in children {
      collect_tree_nodes(child, max_depth, depth + 1, nodes);
    }
  }
}

fn page_meta(page_ref: &PageRef, snapshot: &PageSnapshot) -> PageMeta {
  PageMeta {
    id: snapshot.id,
    title: snapshot.title.clone(),
    slug: page_ref.slug.as_str().to_string(),
    modified_at: snapshot.modified_at.clone()
  }
}

fn snapshot_from_page(page: &PageFullDetailsSchema) -> PageSnapshot {
  PageSnapshot {
    id: page.id,
    title: page.title.clone(),
    modified_at: page.modified_at.clone()
  }
}

fn title_from_slug(slug: &Slug) -> String {
  slug
    .as_str()
    .rsplit('/')
    .next()
    .unwrap_or(slug.as_str())
    .replace(['-', '_'], " ")
}

fn absolute_local_dir(local_dir: &Path) -> anyhow::Result<PathBuf> {
  if local_dir.is_absolute() {
    Ok(local_dir.to_path_buf())
  } else {
    Ok(std::env::current_dir()?.join(local_dir))
  }
}
