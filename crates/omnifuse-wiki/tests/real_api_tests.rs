//! Smoke tests for `WikiClient` against a real API.
//!
//! Required environment variables:
//! - `OMNIFUSE_WIKI_URL`   — base API URL
//! - `OMNIFUSE_WIKI_TOKEN` — authorization token
//! - `OMNIFUSE_WIKI_ROOT_SLUG` — root slug for tests
//!
//! All tests run by default. Without env variables they do an early return.
//! Skip: `cargo test -- --skip real_api`
#![allow(clippy::expect_used)]

use omnifuse_wiki::client::Client;

/// Creates a client from environment variables.
/// Returns `None` (and prints SKIP) if any variable is missing.
fn setup_client() -> Option<Client> {
    let url = match std::env::var("OMNIFUSE_WIKI_URL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("SKIP: OMNIFUSE_WIKI_URL is not set");
            return None;
        }
    };
    let token = match std::env::var("OMNIFUSE_WIKI_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("SKIP: OMNIFUSE_WIKI_TOKEN is not set");
            return None;
        }
    };
    Client::new(&url, &token).ok()
}

/// Root slug for tests (from environment).
fn root_slug() -> Option<String> {
    match std::env::var("OMNIFUSE_WIKI_ROOT_SLUG") {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            eprintln!("SKIP: OMNIFUSE_WIKI_ROOT_SLUG is not set");
            None
        }
    }
}

// ─── CRUD cycle ─────────────────────────────────────────────────

#[tokio::test]
async fn test_smoke_crud_page() {
    // Full cycle: create -> read -> update -> delete
    let Some(client) = setup_client() else { return };
    let Some(slug) = root_slug() else { return };

    let test_slug = format!("{slug}/omnifuse-test-crud-{}", std::process::id());
    let title = "OmniFuse CRUD Test";
    let content_v1 = "# Test page v1";
    let content_v2 = "# Test page v2 (updated)";

    // Create
    let created = client
        .create_page(&test_slug, title, Some(content_v1), "page")
        .await
        .expect("create_page");
    assert!(created.id > 0, "id must be > 0");
    assert_eq!(created.title, title);

    // Read by id
    let fetched = client
        .get_page_by_idx(created.id)
        .await
        .expect("get_page_by_idx");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.content.as_deref(), Some(content_v1));

    // Update
    let updated = client
        .update_page(created.id, None, Some(content_v2), false)
        .await
        .expect("update_page");
    assert_eq!(updated.id, created.id);

    // Verify updated content
    let refetched = client
        .get_page_by_idx(created.id)
        .await
        .expect("get_page_by_idx after update");
    assert_eq!(refetched.content.as_deref(), Some(content_v2));

    // Delete
    client
        .delete_page(created.id)
        .await
        .expect("delete_page");
}

// ─── 404 error ──────────────────────────────────────────────────

#[tokio::test]
async fn test_error_mapping_404() {
    // Requesting a non-existent page should return an error
    let Some(client) = setup_client() else { return };

    let result = client.get_page_by_idx(u64::MAX).await;
    assert!(result.is_err(), "expected error for id=u64::MAX");

    let err_msg = result.expect_err("expected Err").to_string();
    assert!(
        err_msg.contains("not found") || err_msg.contains("404") || err_msg.contains("error"),
        "error message does not contain expected text: {err_msg}"
    );
}

// ─── Page tree ──────────────────────────────────────────────────

#[tokio::test]
async fn test_get_page_tree() {
    // Tree for the root slug should have children
    let Some(client) = setup_client() else { return };
    let Some(slug) = root_slug() else { return };

    let tree = client
        .get_page_tree(&slug, 100, 3)
        .await
        .expect("get_page_tree");

    assert!(!tree.root.slug.is_empty(), "root slug must not be empty");
    assert!(!tree.root.title.is_empty(), "root title must not be empty");

    // Root node typically has children
    let has_children = tree
        .root
        .children
        .as_ref()
        .is_some_and(|c| !c.is_empty());
    assert!(has_children, "root must have children");
}

// ─── Get page by slug ───────────────────────────────────────────

#[tokio::test]
async fn test_get_page_by_slug() {
    // Read page by root slug — title must not be empty
    let Some(client) = setup_client() else { return };
    let Some(slug) = root_slug() else { return };

    let page = client
        .get_page_by_slug(&slug)
        .await
        .expect("get_page_by_slug");

    assert!(!page.title.is_empty(), "title must not be empty");
    assert_eq!(page.slug, slug, "slug must match");
    assert!(page.id > 0, "id must be > 0");
}
