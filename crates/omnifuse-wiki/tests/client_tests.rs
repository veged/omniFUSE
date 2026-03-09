//! WikiClient tests via FakeWikiApi.

#![allow(clippy::expect_used)]

mod common;

use common::FakeWikiApi;
use omnifuse_wiki::{client::Client, models::PageFullDetailsSchema};

/// Timeout for async tests (30s — HTTP server operations).
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Create a client connected to the fake API.
async fn setup() -> (Client, std::sync::Arc<common::FakeState>) {
  let (base_url, state) = FakeWikiApi::spawn().await;
  let client = Client::new(&base_url, "test-token", None).expect("client");
  (client, state)
}

#[tokio::test]
async fn test_get_page_by_slug() {
  eprintln!("[TEST] test_get_page_by_slug");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    state
      .add_page("docs/intro", "Introduction", "# Intro", "2024-01-01T00:00:00Z", None)
      .await;

    let page = client.get_page_by_slug("docs/intro").await.expect("get");
    assert_eq!(page.slug, "docs/intro");
    assert_eq!(page.title, "Introduction");
    assert_eq!(page.content.as_deref(), Some("# Intro"));
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_get_page_by_idx() {
  eprintln!("[TEST] test_get_page_by_idx");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    let id = state
      .add_page("page/test", "Test Page", "content", "2024-01-01T00:00:00Z", None)
      .await;

    let page = client.get_page_by_idx(id).await.expect("get by idx");
    assert_eq!(page.id, id);
    assert_eq!(page.slug, "page/test");
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_update_page() {
  eprintln!("[TEST] test_update_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    let id = state
      .add_page("docs/update", "Old Title", "old content", "2024-01-01T00:00:00Z", None)
      .await;

    let updated = client
      .update_page(id, Some("New Title"), Some("new content"), false)
      .await
      .expect("update");

    assert_eq!(updated.title, "New Title");
    assert_eq!(updated.content.as_deref(), Some("new content"));
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_create_page() {
  eprintln!("[TEST] test_create_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, _state) = setup().await;

    let page = client
      .create_page("new/page", "New Page", Some("# New"), "page")
      .await
      .expect("create");

    assert_eq!(page.slug, "new/page");
    assert_eq!(page.title, "New Page");
    assert_eq!(page.content.as_deref(), Some("# New"));
    assert!(page.id > 0);
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_delete_page() {
  eprintln!("[TEST] test_delete_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    let id = state
      .add_page("to/delete", "Delete Me", "content", "2024-01-01T00:00:00Z", None)
      .await;

    client.delete_page(id).await.expect("delete");

    // After deletion -> 404
    let result = client.get_page_by_idx(id).await;
    assert!(result.is_err(), "deleted page should return an error");
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_get_page_tree() {
  eprintln!("[TEST] test_get_page_tree");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    let root_id = state
      .add_page("root", "Root", "# Root", "2024-01-01T00:00:00Z", None)
      .await;
    state
      .add_page("root/child1", "Child 1", "# C1", "2024-01-01T00:00:01Z", Some(root_id))
      .await;
    state
      .add_page("root/child2", "Child 2", "# C2", "2024-01-01T00:00:02Z", Some(root_id))
      .await;

    let tree = client.get_page_tree("root", 100, 5).await.expect("tree");
    assert_eq!(tree.root.slug, "root");
    let children = tree.root.children.as_ref().expect("children");
    assert_eq!(children.len(), 2, "should have 2 children");
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_get_page_not_found() {
  eprintln!("[TEST] test_get_page_not_found");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, _state) = setup().await;

    let result = client.get_page_by_slug("nonexistent/page").await;
    assert!(result.is_err(), "nonexistent page should return an error");
    let err_msg = result.expect_err("error").to_string();
    assert!(
      err_msg.contains("page not found"),
      "error should contain 'page not found': {err_msg}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_get_descendants() {
  eprintln!("[TEST] test_get_descendants");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    let parent_id = state
      .add_page("parent", "Parent", "# P", "2024-01-01T00:00:00Z", None)
      .await;
    state
      .add_page("parent/a", "A", "# A", "2024-01-01T00:00:01Z", Some(parent_id))
      .await;
    state
      .add_page("parent/b", "B", "# B", "2024-01-01T00:00:02Z", Some(parent_id))
      .await;

    let descendants = client.get_descendants(parent_id).await.expect("descendants");
    assert_eq!(descendants.len(), 2, "should have 2 descendants");
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_move_cluster() {
  eprintln!("[TEST] test_move_cluster");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    state
      .add_page("src", "Source", "# S", "2024-01-01T00:00:00Z", None)
      .await;

    let op = client.move_cluster("src", "dst").await.expect("move");
    assert_eq!(op.operation.ty, "move");
    assert!(op.status_url.is_some(), "should have status_url");
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_poll_status_url() {
  eprintln!("[TEST] test_poll_status_url");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (client, state) = setup().await;
    state.set_op_status("test-op", "success").await;

    let status = client
      .poll_status_url("/api/status/test-op", std::time::Duration::from_secs(1))
      .await
      .expect("poll");

    assert!(
      matches!(status, omnifuse_wiki::models::Status::Success),
      "status should be Success"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_redirect_returns_clear_error() {
  eprintln!("[TEST] test_redirect_returns_clear_error");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Start a server that always returns 302
    let app = axum::Router::new().route(
      "/api/v2/public/pages/tree",
      axum::routing::get(|| async {
        (
          axum::http::StatusCode::FOUND,
          [("location", "https://passport.example.com/login")],
          ""
        )
      })
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = Client::new(&format!("http://{addr}"), "test-token", None).expect("client");
    let result = client.get_page_tree("root", 10, 2).await;

    assert!(result.is_err(), "redirect should be an error");
    let err = result.expect_err("error").to_string();
    assert!(err.contains("redirect"), "error should mention 'redirect': {err}");
    assert!(
      err.contains("passport.example.com"),
      "error should contain the redirect target: {err}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_org_id_header_sent() {
  eprintln!("[TEST] test_org_id_header_sent");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Start a server that echoes the X-Org-Id header back in the response
    let app = axum::Router::new().route(
      "/api/v2/public/pages",
      axum::routing::get(|headers: axum::http::HeaderMap| async move {
        let org_id = headers
          .get("x-org-id")
          .and_then(|v| v.to_str().ok())
          .unwrap_or("missing");
        axum::Json(serde_json::json!({
          "id": 1,
          "title": org_id,
          "slug": "test",
          "page_type": "page",
          "content": org_id,
          "modified_at": "2024-01-01T00:00:00Z"
        }))
      })
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = Client::new(&format!("http://{addr}"), "token", Some("org-12345")).expect("client");
    let page = client.get_page_by_slug("test").await.expect("get");

    assert_eq!(
      page.content.as_deref(),
      Some("org-12345"),
      "server should receive X-Org-Id header"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_content_deserialize_json_object() {
  eprintln!("[TEST] test_content_deserialize_json_object");
  // Grid pages return content as a JSON object, not a string
  let json = r#"{
    "id": 42,
    "title": "Grid Page",
    "slug": "grid/page",
    "page_type": "grid",
    "content": {"rows": [{"cells": ["a", "b"]}]},
    "modified_at": "2024-01-01T00:00:00Z"
  }"#;
  let page: PageFullDetailsSchema = serde_json::from_str(json).expect("deserialize grid content");
  assert_eq!(page.id, 42);
  assert_eq!(page.page_type, "grid");
  // JSON object is serialized to string
  let content = page.content.expect("content should be Some");
  assert!(
    content.contains("rows"),
    "content should contain the JSON object as string: {content}"
  );
}

#[tokio::test]
async fn test_content_deserialize_string() {
  eprintln!("[TEST] test_content_deserialize_string");
  // Normal pages return content as a string
  let json = r##"{
    "id": 1,
    "title": "Normal",
    "slug": "normal",
    "page_type": "wysiwyg",
    "content": "# Hello world",
    "modified_at": "2024-01-01T00:00:00Z"
  }"##;
  let page: PageFullDetailsSchema = serde_json::from_str(json).expect("deserialize string content");
  assert_eq!(page.content.as_deref(), Some("# Hello world"));
}

#[tokio::test]
async fn test_content_deserialize_null() {
  eprintln!("[TEST] test_content_deserialize_null");
  let json = r#"{
    "id": 1,
    "title": "No Content",
    "slug": "empty",
    "page_type": "page",
    "content": null,
    "modified_at": "2024-01-01T00:00:00Z"
  }"#;
  let page: PageFullDetailsSchema = serde_json::from_str(json).expect("deserialize null content");
  assert!(page.content.is_none());
}
