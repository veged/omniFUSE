//! Wiki page synchronization session.

use std::{
  path::{Path, PathBuf},
  sync::Arc
};

use omnifuse_core::InitResult;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::{
  WikiConfig,
  client::Client,
  meta::{MetaStore, PageMeta},
  models::PageTreeNodeSchema,
  page::{PageRef, PageSnapshot, Slug},
  page_index::PageIndex
};

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
    let meta_store = MetaStore::new(local_dir)?;

    Ok(Self {
      config,
      client,
      local_dir: local_dir.to_path_buf(),
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
