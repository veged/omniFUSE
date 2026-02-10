//! WikiBackend integration tests via FakeWikiApi.

#![allow(clippy::expect_used)]

mod common;

use std::path::Path;

use common::FakeWikiApi;
use omnifuse_core::Backend;
use omnifuse_wiki::{WikiBackend, WikiConfig};

/// Timeout for async tests (30s — HTTP server operations).
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Create a WikiBackend connected to the fake API with pre-loaded pages.
async fn setup_backend() -> (
  WikiBackend,
  std::sync::Arc<common::FakeState>,
  tempfile::TempDir,
  String,
) {
  let (base_url, state) = FakeWikiApi::spawn().await;

  // Create root page and children
  let root_id = state
    .add_page("root", "Root", "# Root page", "2024-01-01T00:00:00Z", None)
    .await;
  state
    .add_page(
      "root/docs",
      "Docs",
      "# Documentation",
      "2024-01-01T00:00:01Z",
      Some(root_id),
    )
    .await;

  let config = WikiConfig {
    base_url: base_url.clone(),
    auth_token: "test-token".to_string(),
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500,
  };
  let backend = WikiBackend::new(config).expect("backend");

  let tmp = tempfile::tempdir().expect("tempdir");

  (backend, state, tmp, base_url)
}

#[tokio::test]
async fn test_should_track_md_only() {
  eprintln!("[TEST] test_should_track_md_only");
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  assert!(
    backend.should_track(Path::new("docs/page.md")),
    ".md file should be tracked"
  );
  assert!(
    !backend.should_track(Path::new("image.png")),
    ".png should not be tracked"
  );
  assert!(
    !backend.should_track(Path::new("data.json")),
    ".json should not be tracked"
  );
}

#[tokio::test]
async fn test_should_track_excludes_vfs() {
  eprintln!("[TEST] test_should_track_excludes_vfs");
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  assert!(
    !backend.should_track(Path::new(".vfs/meta/page.md")),
    ".vfs/ should be excluded"
  );
  assert!(
    !backend.should_track(Path::new(".vfs/base/docs.md")),
    ".vfs/base/ should be excluded"
  );
}

#[tokio::test]
async fn test_init_downloads_tree() {
  eprintln!("[TEST] test_init_downloads_tree");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    let result = backend.init(&local_dir).await.expect("init");
    assert!(
      matches!(result, omnifuse_core::InitResult::Updated),
      "init with data -> Updated: {result:?}"
    );

    // Verify files are downloaded
    assert!(
      local_dir.join("root.md").exists(),
      "root.md should be downloaded"
    );
    assert!(
      local_dir.join("root/docs.md").exists(),
      "root/docs.md should be downloaded"
    );

    // Verify content
    let content = std::fs::read_to_string(local_dir.join("root.md")).expect("read");
    assert_eq!(content, "# Root page");
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_init_creates_meta() {
  eprintln!("[TEST] test_init_creates_meta");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Verify metadata is created
    let meta_dir = local_dir.join(".vfs/meta");
    assert!(meta_dir.exists(), ".vfs/meta/ should exist");

    let meta_file = meta_dir.join("root.json");
    assert!(meta_file.exists(), "root.json meta should exist");

    // Verify base
    let base_dir = local_dir.join(".vfs/base");
    assert!(base_dir.exists(), ".vfs/base/ should exist");

    let base_file = base_dir.join("root.md");
    assert!(base_file.exists(), "root.md base should exist");
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_new_page() {
  eprintln!("[TEST] test_sync_new_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Create a new .md file
    let new_page_path = local_dir.join("new-page.md");
    std::fs::write(&new_page_path, "# New Page Content").expect("write");

    let result = backend.sync(&[new_page_path]).await.expect("sync");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { synced_files: 1 }),
      "sync of new page -> Success: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_update_page() {
  eprintln!("[TEST] test_sync_update_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Modify an existing file
    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Updated Root").expect("write");

    let result = backend.sync(&[page_path]).await.expect("sync");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { .. }),
      "sync of updated page -> Success: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_poll_remote_detects_changes() {
  eprintln!("[TEST] test_poll_remote_detects_changes");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Change modified_at on the server (simulate a remote change)
    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.modified_at = "2024-06-01T00:00:00Z".to_string();
          page.content = "# Changed remotely".to_string();
        }
      }
    }

    let changes = backend.poll_remote().await.expect("poll_remote");
    assert!(
      !changes.is_empty(),
      "remote changes should be detected"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_apply_remote_writes() {
  eprintln!("[TEST] test_apply_remote_writes");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Apply a remote change
    let change = omnifuse_core::RemoteChange::Modified {
      path: local_dir.join("root.md"),
      content: b"# Remote update".to_vec(),
    };

    backend
      .apply_remote(vec![change])
      .await
      .expect("apply_remote");

    // Verify the file is updated
    let content = std::fs::read_to_string(local_dir.join("root.md")).expect("read");
    assert_eq!(content, "# Remote update");
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_with_concurrent_remote_change() {
  eprintln!("[TEST] test_sync_with_concurrent_remote_change");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Modify the page on the server (simulate concurrent editing)
    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.content = "# Remote change".to_string();
          page.modified_at = "2024-12-01T00:00:00Z".to_string();
        }
      }
    }

    // Simultaneously modify the local file
    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Local change").expect("write");

    // Sync detects conflict between local and remote changes
    let result = backend.sync(&[page_path.clone()]).await.expect("sync");
    assert!(
      matches!(
        result,
        omnifuse_core::SyncResult::Conflict { ref conflict_files, .. }
        if !conflict_files.is_empty()
      ),
      "sync with concurrent changes should detect conflict: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_name_returns_wiki() {
  eprintln!("[TEST] test_name_returns_wiki");
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  // Verify backend name is "wiki"
  assert_eq!(backend.name(), "wiki", "backend.name() should return \"wiki\"");
}

#[tokio::test]
async fn test_poll_interval_from_config() {
  eprintln!("[TEST] test_poll_interval_from_config");
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    poll_interval_secs: 60,
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  // Verify poll_interval matches the config value
  assert_eq!(
    backend.poll_interval(),
    std::time::Duration::from_secs(60),
    "poll_interval() should return Duration from config (60s)"
  );
}

#[tokio::test]
async fn test_init_idempotent() {
  eprintln!("[TEST] test_init_idempotent");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Repeated init returns UpToDate if data hasn't changed
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    // First init -> Updated
    let result1 = backend.init(&local_dir).await.expect("init 1");
    assert!(
      matches!(result1, omnifuse_core::InitResult::Updated),
      "first init -> Updated: {result1:?}"
    );

    // Second init -> UpToDate (data already downloaded)
    let result2 = backend.init(&local_dir).await.expect("init 2");
    assert!(
      matches!(result2, omnifuse_core::InitResult::UpToDate),
      "repeated init -> UpToDate: {result2:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_should_track_nested_md() {
  eprintln!("[TEST] test_should_track_nested_md");
  // Nested .md files are also tracked
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  assert!(
    backend.should_track(Path::new("docs/architecture/overview.md")),
    "nested .md file should be tracked"
  );
  assert!(
    backend.should_track(Path::new("a/b/c/page.md")),
    "deeply nested .md should be tracked"
  );
}

#[tokio::test]
async fn test_apply_remote_updates_base() {
  eprintln!("[TEST] test_apply_remote_updates_base");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // apply_remote updates the base version for three-way merge
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Apply a remote change
    let change = omnifuse_core::RemoteChange::Modified {
      path: local_dir.join("root.md"),
      content: b"# Updated by remote".to_vec(),
    };

    backend
      .apply_remote(vec![change])
      .await
      .expect("apply_remote");

    // Verify base is updated
    let base_path = local_dir.join(".vfs/base/root.md");
    assert!(base_path.exists(), "base file should exist");

    let base_content = std::fs::read_to_string(&base_path).expect("read base");
    assert_eq!(
      base_content, "# Updated by remote",
      "base should contain updated content"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_delete_page() {
  eprintln!("[TEST] test_sync_delete_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Deleting .md file from disk -> sync returns an error (file not found)
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Delete the file from disk
    let page_path = local_dir.join("root.md");
    assert!(page_path.exists(), "root.md should exist after init");
    std::fs::remove_file(&page_path).expect("remove");
    assert!(!page_path.exists(), "root.md deleted from disk");

    // sync for deleted file -> read error
    let result = backend.sync(&[page_path]).await;
    assert!(
      result.is_err(),
      "sync of deleted file should return an error: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_poll_remote_no_changes() {
  eprintln!("[TEST] test_poll_remote_no_changes");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // poll_remote right after init (no server changes) -> empty list
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Poll immediately - no changes
    let changes = backend.poll_remote().await.expect("poll_remote");
    assert!(
      changes.is_empty(),
      "poll_remote with no server changes -> empty list: {} items",
      changes.len()
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_init_with_deep_nesting() {
  eprintln!("[TEST] test_init_with_deep_nesting");
  tokio::time::timeout(TEST_TIMEOUT, async {
  // Pages with 3 nesting levels: root -> docs -> api
  let (base_url, state) = FakeWikiApi::spawn().await;

  let root_id = state
    .add_page("root", "Root", "# Root", "2024-01-01T00:00:00Z", None)
    .await;
  let docs_id = state
    .add_page(
      "root/docs",
      "Docs",
      "# Docs",
      "2024-01-01T00:00:01Z",
      Some(root_id),
    )
    .await;
  state
    .add_page(
      "root/docs/api",
      "API",
      "# API Reference",
      "2024-01-01T00:00:02Z",
      Some(docs_id),
    )
    .await;

  let config = WikiConfig {
    base_url,
    auth_token: "test-token".to_string(),
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500,
  };
  let backend = WikiBackend::new(config).expect("backend");
  let tmp = tempfile::tempdir().expect("tempdir");
  let local_dir = tmp.path().to_path_buf();

  let result = backend.init(&local_dir).await.expect("init");
  assert!(
    matches!(result, omnifuse_core::InitResult::Updated),
    "init with deep tree -> Updated: {result:?}"
  );

  // Verify all 3 nesting levels
  assert!(
    local_dir.join("root.md").exists(),
    "root.md should be downloaded"
  );
  assert!(
    local_dir.join("root/docs.md").exists(),
    "root/docs.md should be downloaded"
  );
  assert!(
    local_dir.join("root/docs/api.md").exists(),
    "root/docs/api.md should be downloaded (3rd level)"
  );

  // Verify the deepest file content
  let content =
    std::fs::read_to_string(local_dir.join("root/docs/api.md")).expect("read api.md");
  assert_eq!(content, "# API Reference");
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_non_md_file_ignored() {
  eprintln!("[TEST] test_sync_non_md_file_ignored");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Non-.md file: should_track returns false
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Create a .txt file (not .md)
    let txt_path = local_dir.join("file.txt");
    std::fs::write(&txt_path, "plain text").expect("write");

    // should_track for .txt -> false
    assert!(
      !backend.should_track(Path::new("file.txt")),
      ".txt file should not be tracked (should_track = false)"
    );

    // sync will still attempt to process the file,
    // since filtering is the responsibility of VFS core, not the backend
    let result = backend.sync(&[txt_path]).await.expect("sync");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { .. }),
      "sync of non-md file handled by backend: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_apply_remote_new_page() {
  eprintln!("[TEST] test_apply_remote_new_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // apply_remote with RemoteChange::Modified for a new file -> file created on disk
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Apply a remote addition (new file)
    let new_path = local_dir.join("brand-new.md");
    assert!(!new_path.exists(), "file should not exist before apply");

    let change = omnifuse_core::RemoteChange::Modified {
      path: new_path.clone(),
      content: b"# Brand New Page".to_vec(),
    };

    backend
      .apply_remote(vec![change])
      .await
      .expect("apply_remote");

    // Verify the file is created
    assert!(new_path.exists(), "new file should be created");

    let content = std::fs::read_to_string(&new_path).expect("read");
    assert_eq!(content, "# Brand New Page");
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_multiple_pages() {
  eprintln!("[TEST] test_sync_multiple_pages");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Create 3 new .md files -> sync all three -> synced_files: 3
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Create 3 new .md files
    let paths: Vec<_> = (1..=3)
      .map(|i| {
        let p = local_dir.join(format!("page-{i}.md"));
        std::fs::write(&p, format!("# Page {i}")).expect("write");
        p
      })
      .collect();

    let result = backend.sync(&paths).await.expect("sync");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { synced_files: 3 }),
      "sync of three new pages -> Success {{ synced_files: 3 }}: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_poll_remote_after_server_update() {
  eprintln!("[TEST] test_poll_remote_after_server_update");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // mtime updates when content is changed remotely
    let (backend, state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Change content AND modified_at of the page on the server
    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root/docs" {
          page.content = "# Updated documentation".to_string();
          page.modified_at = "2025-03-15T12:00:00Z".to_string();
        }
      }
    }

    let changes = backend.poll_remote().await.expect("poll_remote");

    // A change for root/docs should be detected
    let has_modified = changes.iter().any(|c| {
      matches!(c, omnifuse_core::RemoteChange::Modified { path, .. }
        if path.ends_with("root/docs.md"))
    });
    assert!(
      has_modified,
      "poll_remote should detect Modified for root/docs.md: {changes:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_apply_remote_deleted_page() {
  eprintln!("[TEST] test_apply_remote_deleted_page");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // A page deleted on the server is removed from disk via apply_remote
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let root_path = local_dir.join("root.md");
    assert!(root_path.exists(), "root.md should exist after init");

    // Apply a remote deletion
    let change = omnifuse_core::RemoteChange::Deleted {
      path: root_path.clone(),
    };
    backend
      .apply_remote(vec![change])
      .await
      .expect("apply_remote");

    // File should be deleted from disk
    assert!(
      !root_path.exists(),
      "root.md should be deleted after apply_remote Deleted"
    );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_init_empty_tree() {
  eprintln!("[TEST] test_init_empty_tree");
  tokio::time::timeout(TEST_TIMEOUT, async {
  // Server with a single root page, no children.
  // Repeated init -> UpToDate (count=0, files already in place).
  let (base_url, state) = FakeWikiApi::spawn().await;

  state
    .add_page("root", "Root", "# Empty root", "2024-01-01T00:00:00Z", None)
    .await;

  let config = WikiConfig {
    base_url,
    auth_token: "test-token".to_string(),
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500,
  };
  let backend = WikiBackend::new(config).expect("backend");
  let tmp = tempfile::tempdir().expect("tempdir");
  let local_dir = tmp.path().to_path_buf();

  // First init - download the single page
  let result1 = backend.init(&local_dir).await.expect("init 1");
  assert!(
    matches!(result1, omnifuse_core::InitResult::Updated),
    "first init -> Updated: {result1:?}"
  );

  // Repeated init - data already in place, count=0
  let result2 = backend.init(&local_dir).await.expect("init 2");
  assert!(
    matches!(result2, omnifuse_core::InitResult::UpToDate),
    "repeated init of empty tree -> UpToDate (count=0): {result2:?}"
  );
  }).await.expect("test timed out — possible deadlock");
}

#[tokio::test]
async fn test_sync_with_server_error() {
  eprintln!("[TEST] test_sync_with_server_error");
  tokio::time::timeout(TEST_TIMEOUT, async {
    // Modify FakeState: remove the page from pages but keep it in
    // slug_to_id so the handler panics -> server returns 500.
    // sync should return Err (error is not "page not found").
    let (backend, state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Modify the local file
    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Local change").expect("write");

    // Remove the page from pages but keep the slug in slug_to_id.
    // This creates a desync: handle_get_by_slug finds the id, but pages[&id]
    // panics -> axum returns 500 Internal Server Error.
    {
      let slug_map = state.slug_to_id.read().await;
      let root_id = *slug_map.get("root").expect("root slug");
      drop(slug_map);
      state.pages.write().await.remove(&root_id);
    }

    let result = backend.sync(&[page_path]).await;
    assert!(
      result.is_err(),
      "sync with server error 500 should return Err: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

/// Root page with 3 children: init downloads all 4 .md files.
#[tokio::test]
async fn test_read_page_with_subpages() {
  eprintln!("[TEST] test_read_page_with_subpages");
  tokio::time::timeout(TEST_TIMEOUT, async {
  let (base_url, state) = FakeWikiApi::spawn().await;

  let root_id = state
    .add_page("root", "Root", "# Root page", "2024-01-01T00:00:00Z", None)
    .await;
  state
    .add_page(
      "root/child1",
      "Child 1",
      "# Child 1",
      "2024-01-01T00:00:01Z",
      Some(root_id),
    )
    .await;
  state
    .add_page(
      "root/child2",
      "Child 2",
      "# Child 2",
      "2024-01-01T00:00:02Z",
      Some(root_id),
    )
    .await;
  state
    .add_page(
      "root/child3",
      "Child 3",
      "# Child 3",
      "2024-01-01T00:00:03Z",
      Some(root_id),
    )
    .await;

  let config = WikiConfig {
    base_url,
    auth_token: "test-token".to_string(),
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500,
  };
  let backend = WikiBackend::new(config).expect("backend");
  let tmp = tempfile::tempdir().expect("tempdir");
  let local_dir = tmp.path().to_path_buf();

  let result = backend.init(&local_dir).await.expect("init");
  assert!(
    matches!(result, omnifuse_core::InitResult::Updated),
    "init with root + 3 children -> Updated: {result:?}"
  );

  // Verify all 4 files
  assert!(
    local_dir.join("root.md").exists(),
    "root.md should be downloaded"
  );
  assert!(
    local_dir.join("root/child1.md").exists(),
    "root/child1.md should be downloaded"
  );
  assert!(
    local_dir.join("root/child2.md").exists(),
    "root/child2.md should be downloaded"
  );
  assert!(
    local_dir.join("root/child3.md").exists(),
    "root/child3.md should be downloaded"
  );
  }).await.expect("test timed out — possible deadlock");
}

/// Deleting a parent page via apply_remote(Deleted): file removed from disk.
#[tokio::test]
async fn test_delete_page_with_subpages() {
  eprintln!("[TEST] test_delete_page_with_subpages");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    let root_path = local_dir.join("root.md");
    assert!(root_path.exists(), "root.md should exist after init");

    // Apply remote deletion for the parent page
    let change = omnifuse_core::RemoteChange::Deleted {
      path: root_path.clone(),
    };
    backend
      .apply_remote(vec![change])
      .await
      .expect("apply_remote");

    // Parent file should be deleted
    assert!(
      !root_path.exists(),
      "root.md should be deleted after apply_remote Deleted"
    );
  }).await.expect("test timed out — possible deadlock");
}

/// Write file, sync, write again -- second change is not lost.
#[tokio::test]
async fn test_write_during_sync_not_lost() {
  eprintln!("[TEST] test_write_during_sync_not_lost");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, _state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // First modification
    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# First change").expect("write 1");

    // Sync the first change
    let result = backend.sync(&[page_path.clone()]).await.expect("sync 1");
    assert!(
      matches!(result, omnifuse_core::SyncResult::Success { .. }),
      "sync 1 → Success: {result:?}"
    );

    // Immediately write a second change
    std::fs::write(&page_path, "# Second change").expect("write 2");

    // Verify disk content is the second change
    let content = std::fs::read_to_string(&page_path).expect("read");
    assert_eq!(
      content, "# Second change",
      "second change should not be lost after sync"
    );
  }).await.expect("test timed out — possible deadlock");
}

/// Concurrent local + remote change -> sync detects conflict.
#[tokio::test]
async fn test_merge_conflict_detection() {
  eprintln!("[TEST] test_merge_conflict_detection");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (backend, state, tmp, _url) = setup_backend().await;
    let local_dir = tmp.path().to_path_buf();

    backend.init(&local_dir).await.expect("init");

    // Modify the page on the server (remote change)
    {
      let mut pages = state.pages.write().await;
      for page in pages.values_mut() {
        if page.slug == "root" {
          page.content = "# Remote change (conflict)".to_string();
          page.modified_at = "2024-12-01T00:00:00Z".to_string();
        }
      }
    }

    // Simultaneously modify locally
    let page_path = local_dir.join("root.md");
    std::fs::write(&page_path, "# Local change (conflict)").expect("write");

    // sync should detect a conflict
    let result = backend.sync(&[page_path]).await.expect("sync");
    assert!(
      matches!(
        result,
        omnifuse_core::SyncResult::Conflict { ref conflict_files, .. }
        if !conflict_files.is_empty()
      ),
      "sync with concurrent changes should detect conflict: {result:?}"
    );
  }).await.expect("test timed out — possible deadlock");
}

/// Non-.md files (.txt, .json) are not tracked: should_track -> false.
#[tokio::test]
async fn test_non_md_files_are_local_only() {
  eprintln!("[TEST] test_non_md_files_are_local_only");
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  assert!(
    !backend.should_track(Path::new("notes.txt")),
    ".txt file should not be tracked"
  );
  assert!(
    !backend.should_track(Path::new("config.json")),
    ".json file should not be tracked"
  );
  assert!(
    !backend.should_track(Path::new("data.yaml")),
    ".yaml file should not be tracked"
  );
  assert!(
    !backend.should_track(Path::new("script.py")),
    ".py file should not be tracked"
  );
  assert!(
    !backend.should_track(Path::new("archive.tar.gz")),
    ".tar.gz file should not be tracked"
  );
  // But .md is tracked
  assert!(
    backend.should_track(Path::new("page.md")),
    ".md file should be tracked"
  );
}

#[tokio::test]
async fn test_should_track_dot_files_excluded() {
  eprintln!("[TEST] test_should_track_dot_files_excluded");
  // Files starting with a dot are not .md -> should_track = false
  let config = WikiConfig {
    base_url: "http://unused".to_string(),
    auth_token: "token".to_string(),
    ..WikiConfig::default()
  };
  let backend = WikiBackend::new(config).expect("backend");

  assert!(
    !backend.should_track(Path::new(".hidden")),
    ".hidden should not be tracked (no .md extension)"
  );
  assert!(
    !backend.should_track(Path::new(".DS_Store")),
    ".DS_Store should not be tracked (no .md extension)"
  );
  assert!(
    !backend.should_track(Path::new(".gitignore")),
    ".gitignore should not be tracked (no .md extension)"
  );
}
