//! Mock Wiki API server on axum.
//!
//! Provides `FakeWikiApi::spawn()` — starts an HTTP server on a random port.

#![allow(clippy::expect_used)]

use std::{
  collections::HashMap,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering}
  }
};

use axum::{
  Json, Router,
  extract::{Path, Query, State},
  http::StatusCode,
  response::IntoResponse,
  routing::{get, post}
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Stored page data.
#[derive(Debug, Clone)]
pub struct StoredPage {
  pub id: u64,
  pub title: String,
  pub slug: String,
  pub page_type: String,
  pub content: String,
  pub modified_at: String,
  pub parent_id: Option<u64>,
}

/// Internal state of the fake API.
#[derive(Debug)]
pub struct FakeState {
  pub pages: RwLock<HashMap<u64, StoredPage>>,
  pub slug_to_id: RwLock<HashMap<String, u64>>,
  next_id: AtomicU64,
  pub op_statuses: RwLock<HashMap<String, String>>,
}

impl FakeState {
  fn new() -> Self {
    Self {
      pages: RwLock::new(HashMap::new()),
      slug_to_id: RwLock::new(HashMap::new()),
      next_id: AtomicU64::new(1),
      op_statuses: RwLock::new(HashMap::new()),
    }
  }

  /// Add a page to the store.
  pub async fn add_page(
    &self,
    slug: &str,
    title: &str,
    content: &str,
    modified_at: &str,
    parent_id: Option<u64>,
  ) -> u64 {
    let id = self.next_id.fetch_add(1, Ordering::SeqCst);
    let page = StoredPage {
      id,
      title: title.to_string(),
      slug: slug.to_string(),
      page_type: "page".to_string(),
      content: content.to_string(),
      modified_at: modified_at.to_string(),
      parent_id,
    };
    self.pages.write().await.insert(id, page);
    self.slug_to_id.write().await.insert(slug.to_string(), id);
    id
  }

  /// Set operation status for polling.
  pub async fn set_op_status(&self, op_id: &str, status: &str) {
    self.op_statuses.write().await.insert(op_id.to_string(), status.to_string());
  }
}

/// Fake Wiki API — start and get base URL + state.
pub struct FakeWikiApi;

impl FakeWikiApi {
  /// Start a fake API server on a random port.
  pub async fn spawn() -> (String, Arc<FakeState>) {
    let state = Arc::new(FakeState::new());

    let app = Router::new()
      .route("/api/v2/public/pages/tree", get(handle_tree))
      .route("/api/v2/public/pages/move", post(handle_move))
      .route("/api/v2/public/pages/:id/descendants", get(handle_descendants))
      .route(
        "/api/v2/public/pages/:id",
        get(handle_get_by_id)
          .post(handle_update)
          .delete(handle_delete),
      )
      .route(
        "/api/v2/public/pages",
        get(handle_get_by_slug).post(handle_create),
      )
      .route("/api/status/:id", get(handle_status))
      .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
      axum::serve(listener, app).await.expect("serve");
    });

    // Wait for the server to start.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (base_url, state)
  }
}

// -- JSON models for responses --

#[derive(Serialize)]
struct PageResponse {
  id: u64,
  title: String,
  slug: String,
  page_type: String,
  content: Option<String>,
  modified_at: String,
}

impl From<&StoredPage> for PageResponse {
  fn from(p: &StoredPage) -> Self {
    Self {
      id: p.id,
      title: p.title.clone(),
      slug: p.slug.clone(),
      page_type: p.page_type.clone(),
      content: Some(p.content.clone()),
      modified_at: p.modified_at.clone(),
    }
  }
}

#[derive(Serialize)]
struct PageBrief {
  id: u64,
  slug: String,
}

#[derive(Serialize)]
struct CollectionResponse<T: Serialize> {
  results: Vec<T>,
  next_cursor: Option<String>,
  has_next: bool,
  metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct TreeNode {
  id: u64,
  slug: String,
  title: String,
  modified_at: String,
  children: Option<Vec<TreeNode>>,
}

#[derive(Serialize)]
struct TreeResponse {
  root: TreeNode,
}

#[derive(Serialize)]
struct OpCreated {
  operation: OpIdentity,
  status_url: Option<String>,
}

#[derive(Serialize)]
struct OpIdentity {
  #[serde(rename = "type")]
  ty: String,
  id: String,
}

#[derive(Serialize)]
struct StatusResponse {
  status: String,
}

// -- Handlers --

#[derive(Deserialize)]
struct SlugQuery {
  slug: Option<String>,
  #[allow(dead_code)]
  fields: Option<String>,
}

async fn handle_get_by_slug(
  State(state): State<Arc<FakeState>>,
  Query(q): Query<SlugQuery>,
) -> impl IntoResponse {
  let slug = match q.slug {
    Some(s) => s,
    None => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "slug required"}))).into_response(),
  };

  let slug_map = state.slug_to_id.read().await;
  let Some(&id) = slug_map.get(&slug) else {
    return (
      StatusCode::NOT_FOUND,
      Json(serde_json::json!({
        "error_code": "NOT_FOUND",
        "debug_message": "page not found",
        "details": null
      })),
    ).into_response();
  };

  let pages = state.pages.read().await;
  let page = &pages[&id];
  Json(PageResponse::from(page)).into_response()
}

async fn handle_get_by_id(
  State(state): State<Arc<FakeState>>,
  Path(id): Path<u64>,
) -> impl IntoResponse {
  let pages = state.pages.read().await;
  match pages.get(&id) {
    Some(page) => Json(PageResponse::from(page)).into_response(),
    None => (
      StatusCode::NOT_FOUND,
      Json(serde_json::json!({
        "error_code": "NOT_FOUND",
        "debug_message": "page not found",
        "details": null
      })),
    ).into_response(),
  }
}

#[derive(Deserialize)]
struct CreateBody {
  page_type: String,
  title: String,
  slug: String,
  content: Option<String>,
}

async fn handle_create(
  State(state): State<Arc<FakeState>>,
  Json(body): Json<CreateBody>,
) -> impl IntoResponse {
  let id = state.next_id.fetch_add(1, Ordering::SeqCst);
  let now = chrono_now();

  let page = StoredPage {
    id,
    title: body.title,
    slug: body.slug.clone(),
    page_type: body.page_type,
    content: body.content.unwrap_or_default(),
    modified_at: now,
    parent_id: None,
  };

  state.slug_to_id.write().await.insert(body.slug, id);
  let resp = PageResponse::from(&page);
  state.pages.write().await.insert(id, page);

  (StatusCode::CREATED, Json(resp)).into_response()
}

#[derive(Deserialize)]
struct UpdateBody {
  title: Option<String>,
  content: Option<String>,
}

async fn handle_update(
  State(state): State<Arc<FakeState>>,
  Path(id): Path<u64>,
  Json(body): Json<UpdateBody>,
) -> impl IntoResponse {
  let mut pages = state.pages.write().await;
  match pages.get_mut(&id) {
    Some(page) => {
      if let Some(title) = body.title {
        page.title = title;
      }
      if let Some(content) = body.content {
        page.content = content;
      }
      page.modified_at = chrono_now();
      Json(PageResponse::from(&*page)).into_response()
    }
    None => (
      StatusCode::NOT_FOUND,
      Json(serde_json::json!({
        "error_code": "NOT_FOUND",
        "debug_message": "page not found",
        "details": null
      })),
    ).into_response(),
  }
}

async fn handle_delete(
  State(state): State<Arc<FakeState>>,
  Path(id): Path<u64>,
) -> impl IntoResponse {
  let mut pages = state.pages.write().await;
  if let Some(page) = pages.remove(&id) {
    state.slug_to_id.write().await.remove(&page.slug);
    StatusCode::OK
  } else {
    StatusCode::NOT_FOUND
  }
}

async fn handle_descendants(
  State(state): State<Arc<FakeState>>,
  Path(parent_id): Path<u64>,
) -> impl IntoResponse {
  let pages = state.pages.read().await;

  let descendants: Vec<PageBrief> = pages
    .values()
    .filter(|p| p.parent_id == Some(parent_id))
    .map(|p| PageBrief {
      id: p.id,
      slug: p.slug.clone(),
    })
    .collect();

  Json(CollectionResponse {
    results: descendants,
    next_cursor: None,
    has_next: false,
    metadata: None,
  })
}

#[derive(Deserialize)]
struct TreeQuery {
  slug: Option<String>,
  #[allow(dead_code)]
  order_by: Option<String>,
  #[allow(dead_code)]
  max_pages: Option<u32>,
  #[allow(dead_code)]
  max_depth: Option<u32>,
}

/// Recursively build a tree node with children.
fn build_tree_node(page: &StoredPage, all_pages: &HashMap<u64, StoredPage>) -> TreeNode {
  let children: Vec<TreeNode> = all_pages
    .values()
    .filter(|p| p.parent_id == Some(page.id))
    .map(|p| build_tree_node(p, all_pages))
    .collect();

  TreeNode {
    id: page.id,
    slug: page.slug.clone(),
    title: page.title.clone(),
    modified_at: page.modified_at.clone(),
    children: Some(children),
  }
}

async fn handle_tree(
  State(state): State<Arc<FakeState>>,
  Query(q): Query<TreeQuery>,
) -> impl IntoResponse {
  let slug = q.slug.unwrap_or_default();
  let slug_map = state.slug_to_id.read().await;

  let Some(&root_id) = slug_map.get(&slug) else {
    return (
      StatusCode::NOT_FOUND,
      Json(serde_json::json!({
        "error_code": "NOT_FOUND",
        "debug_message": "page not found",
        "details": null
      })),
    ).into_response();
  };

  let pages = state.pages.read().await;
  let root_page = &pages[&root_id];

  // Recursively build the tree
  let root = build_tree_node(root_page, &pages);

  Json(TreeResponse { root }).into_response()
}

#[derive(Deserialize)]
struct MoveBody {
  #[allow(dead_code)]
  operations: Vec<MoveOp>,
  #[allow(dead_code)]
  copy_inherited_access: Option<bool>,
  #[allow(dead_code)]
  check_inheritance: Option<bool>,
}

#[derive(Deserialize)]
struct MoveOp {
  #[allow(dead_code)]
  source: String,
  #[allow(dead_code)]
  target: String,
}

async fn handle_move(
  State(state): State<Arc<FakeState>>,
  Json(_body): Json<MoveBody>,
) -> impl IntoResponse {
  let op_id = "move-op-1".to_string();
  state
    .op_statuses
    .write()
    .await
    .insert(op_id.clone(), "scheduled".to_string());

  Json(OpCreated {
    operation: OpIdentity {
      ty: "move".to_string(),
      id: op_id.clone(),
    },
    status_url: Some(format!("/api/status/{op_id}")),
  })
}

async fn handle_status(
  State(state): State<Arc<FakeState>>,
  Path(id): Path<String>,
) -> impl IntoResponse {
  let statuses = state.op_statuses.read().await;
  let status = statuses
    .get(&id)
    .cloned()
    .unwrap_or_else(|| "success".to_string());

  Json(StatusResponse { status })
}

/// Simple timestamp generator.
fn chrono_now() -> String {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  format!("2024-01-01T00:00:{:02}Z", now.as_secs() % 60)
}
