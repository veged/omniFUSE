//! E2E Wiki user scenarios: full pipeline tests simulating real user workflows.
//!
//! These tests exercise the complete WikiBackend → FakeWikiApi pipeline
//! from a user's perspective: mount wiki, edit pages, sync changes,
//! handle conflicts, and detect remote updates.
//!
//! Run: `cargo test -p omnifuse-wiki --test e2e_wiki_user_scenarios`

mod common;

use std::time::Duration;

use common::FakeWikiApi;
use omnifuse_core::{Backend, InitResult, SyncResult};
use omnifuse_wiki::{WikiBackend, WikiConfig};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Create a WikiBackend connected to the fake API with pre-loaded pages.
async fn setup_backend() -> (WikiBackend, std::sync::Arc<common::FakeState>, tempfile::TempDir) {
  let (base_url, state) = FakeWikiApi::spawn().await;
  let root_id = state
    .add_page("root", "Root", "# Root page", "2024-01-01T00:00:00Z", None)
    .await;
  state
    .add_page(
      "root/docs",
      "Docs",
      "# Documentation",
      "2024-01-01T00:00:01Z",
      Some(root_id)
    )
    .await;

  let config = WikiConfig {
    base_url: base_url.clone(),
    auth_token: "test-token".to_string(),
    org_id: None,
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500
  };
  let backend = WikiBackend::new(config).expect("backend");
  let tmp = tempfile::tempdir().expect("tempdir");
  (backend, state, tmp)
}

/// Test 1: Init wiki backend → verify root.md and root/docs.md exist with correct content.
#[tokio::test]
async fn test_init_downloads_tree_files_on_disk() {
  eprintln!("[TEST] test_init_downloads_tree_files_on_disk");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    let result = backend.init(&local_dir).await.expect("init");
    assert!(
      matches!(result, InitResult::Updated),
      "init with data -> Updated: {result:?}"
    );

    assert!(local_dir.join("root.md").exists(), "root.md should be downloaded");
    assert!(
      local_dir.join("root/docs.md").exists(),
      "root/docs.md should be downloaded"
    );

    let root_content = std::fs::read_to_string(local_dir.join("root.md")).expect("read root");
    assert_eq!(root_content, "# Root page");

    let docs_content = std::fs::read_to_string(local_dir.join("root/docs.md")).expect("read docs");
    assert_eq!(docs_content, "# Documentation");
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 2: Init → modify root.md → sync → verify FakeState page content updated.
#[tokio::test]
async fn test_edit_page_sync_updates_server() {
  eprintln!("[TEST] test_edit_page_sync_updates_server");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Updated Root Content").expect("write");

    let result = backend.sync(&[page_path]).await.expect("sync");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "sync of edited page -> Success: {result:?}"
    );

    let slug_map = state.slug_to_id.read().await;
    let root_id = *slug_map.get("root").expect("root slug");
    drop(slug_map);

    let pages = state.pages.read().await;
    let root_page = pages.get(&root_id).expect("root page");
    assert_eq!(
      root_page.content, "# Updated Root Content",
      "server content should be updated"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 3: Init → create new-page.md → sync → verify page exists in FakeState.
#[tokio::test]
async fn test_create_new_page_sync_creates_on_server() {
  eprintln!("[TEST] test_create_new_page_sync_creates_on_server");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let new_page_path = local_dir.join("new-page.md");
    std::fs::write(&new_page_path, "# Brand New Page").expect("write");

    let slug_map = state.slug_to_id.read().await;
    assert!(
      !slug_map.contains_key("new-page"),
      "new-page should not exist before sync"
    );
    drop(slug_map);

    let result = backend.sync(&[new_page_path]).await.expect("sync");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "sync of new page -> Success: {result:?}"
    );

    let slug_map = state.slug_to_id.read().await;
    assert!(slug_map.contains_key("new-page"), "new-page should exist after sync");
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 4: Init → delete root/docs.md → sync → verify sync returns error (file not found).
#[tokio::test]
async fn test_delete_page_sync_removes_from_server() {
  eprintln!("[TEST] test_delete_page_sync_removes_from_server");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let page_path = local_dir.join("root/docs.md");
    assert!(page_path.exists(), "root/docs.md should exist after init");
    std::fs::remove_file(&page_path).expect("remove");
    assert!(!page_path.exists(), "root/docs.md deleted from disk");

    let result = backend.sync(&[page_path]).await;
    assert!(
      result.is_err(),
      "sync of deleted file should return an error: {result:?}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 5: Init → modify locally AND change server modified_at → sync → detect conflict.
#[tokio::test]
async fn test_concurrent_edit_local_and_remote_conflict() {
  eprintln!("[TEST] test_concurrent_edit_local_and_remote_conflict");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.content = "# Remote change".to_string();
          page.modified_at = "2024-12-01T00:00:00Z".to_string();
        }
      }
    }

    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Local change").expect("write");

    let result = backend.sync(&[page_path.clone()]).await.expect("sync");
    assert!(
      matches!(
        result,
        SyncResult::Conflict { ref conflict_files, .. }
        if !conflict_files.is_empty()
      ),
      "sync with concurrent changes should detect conflict: {result:?}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 6: Init → modify different lines locally and remotely → sync → auto-merge succeeds.
#[tokio::test]
async fn test_three_way_merge_resolves_different_lines() {
  eprintln!("[TEST] test_three_way_merge_resolves_different_lines");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (base_url, state) = FakeWikiApi::spawn().await;
    let root_id = state
      .add_page("root", "Root", "line1\nline2\nline3", "2024-01-01T00:00:00Z", None)
      .await;
    state
      .add_page(
        "root/docs",
        "Docs",
        "# Documentation",
        "2024-01-01T00:00:01Z",
        Some(root_id)
      )
      .await;

    let config = WikiConfig {
      base_url: base_url.clone(),
      auth_token: "test-token".to_string(),
      org_id: None,
      root_slug: "root".to_string(),
      poll_interval_secs: 60,
      max_depth: 10,
      max_pages: 500
    };
    let backend = WikiBackend::new(config).expect("backend");
    let tmp = tempfile::tempdir().expect("tempdir");
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "LOCAL\nline2\nline3").expect("write local");

    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.content = "line1\nline2\nREMOTE".to_string();
          page.modified_at = "2024-12-01T00:00:00Z".to_string();
        }
      }
    }

    let result = backend.sync(&[page_path.clone()]).await.expect("sync");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "sync with non-overlapping changes -> Success: {result:?}"
    );

    let content = std::fs::read_to_string(&page_path).expect("read merged");
    assert!(
      content.contains("LOCAL"),
      "merged content should contain LOCAL: {content}"
    );
    assert!(
      content.contains("REMOTE"),
      "merged content should contain REMOTE: {content}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 7: Init → change server page → poll_remote → apply_remote → verify file updated on disk.
#[tokio::test]
async fn test_poll_detects_remote_change_and_apply_updates_disk() {
  eprintln!("[TEST] test_poll_detects_remote_change_and_apply_updates_disk");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let root_path = local_dir.join("root.md");
    let initial_content = std::fs::read_to_string(&root_path).expect("read initial");
    assert_eq!(initial_content, "# Root page");

    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.content = "# Changed remotely".to_string();
          page.modified_at = "2024-06-01T00:00:00Z".to_string();
        }
      }
    }

    let changes = backend.poll_remote().await.expect("poll_remote");
    assert!(!changes.is_empty(), "poll_remote should detect changes: {changes:?}");

    backend.apply_remote(changes).await.expect("apply_remote");

    let updated_content = std::fs::read_to_string(&root_path).expect("read updated");
    assert_eq!(
      updated_content, "# Changed remotely",
      "file should be updated after apply_remote"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}

/// Test 8: Init → create root/docs/api.md → sync → verify nested page created on server.
#[tokio::test]
async fn test_nested_pages_create_edit_sync() {
  eprintln!("[TEST] test_nested_pages_create_edit_sync");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let nested_dir = local_dir.join("root/docs");
    std::fs::create_dir_all(&nested_dir).expect("mkdir");

    let nested_page_path = nested_dir.join("api.md");
    std::fs::write(&nested_page_path, "# API Reference").expect("write nested");

    let slug_map = state.slug_to_id.read().await;
    assert!(
      !slug_map.contains_key("root/docs/api"),
      "root/docs/api should not exist before sync"
    );
    drop(slug_map);

    let result = backend.sync(&[nested_page_path]).await.expect("sync");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "sync of nested page -> Success: {result:?}"
    );

    let slug_map = state.slug_to_id.read().await;
    assert!(
      slug_map.contains_key("root/docs/api"),
      "root/docs/api should exist after sync"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}
