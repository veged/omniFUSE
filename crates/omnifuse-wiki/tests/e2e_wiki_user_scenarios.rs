//! E2E Wiki user scenarios: full pipeline tests simulating real user workflows.
//!
//! These tests exercise the complete WikiBackend → FakeWikiApi pipeline
//! from a user's perspective: mount wiki, edit pages, sync changes,
//! handle conflicts, and detect remote updates.
//!
//! Run: `cargo test -p omnifuse-wiki --test e2e_wiki_user_scenarios`

// User-scenario tests keep long flows and explicit fake-state assertions readable.
#![allow(
  clippy::cloned_ref_to_slice_refs,
  clippy::doc_markdown,
  clippy::expect_used,
  clippy::significant_drop_tightening,
  clippy::too_many_lines
)]

mod common;

use std::{
  path::{Path, PathBuf},
  time::Duration
};

use common::FakeWikiApi;
use omnifuse_core::{
  Backend, InitResult, PathProtection, RemoteApplyMode, RemoteRefresh, RemoteRefreshResult, SyncResult
};
use omnifuse_wiki::{WikiBackend, WikiConfig};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

struct NoPathProtection;

impl PathProtection for NoPathProtection {
  fn is_protected(&self, _path: &Path) -> bool {
    false
  }
}

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

async fn setup_editorial_state() -> (String, std::sync::Arc<common::FakeState>) {
  let (base_url, state) = FakeWikiApi::spawn().await;
  let root_id = state
    .add_page("root", "Root", "# Редакционный проект\n", "2024-01-01T00:00:00Z", None)
    .await;
  let media_id = state
    .add_page(
      "root/media",
      "Посевы",
      "# Посевы\n",
      "2024-01-01T00:00:01Z",
      Some(root_id)
    )
    .await;

  state
    .add_page(
      "root/plan",
      "План",
      "# План публикации\n\n- Тема: запуск OmniFuse\n- Тезис: быстрый черновик с ачепяткой\n- Проверка фактов: ожидается\n",
      "2024-01-01T00:00:02Z",
      Some(root_id)
    )
    .await;
  state
    .add_page(
      "root/article",
      "Статья",
      "# Статья\n\nЧерновик ожидается.\n",
      "2024-01-01T00:00:03Z",
      Some(root_id)
    )
    .await;
  state
    .add_page(
      "root/media/telegram",
      "Telegram",
      "# Telegram\n\nЧерновик ожидается.\n",
      "2024-01-01T00:00:04Z",
      Some(media_id)
    )
    .await;
  state
    .add_page(
      "root/media/vk",
      "VK",
      "# VK\n\nЧерновик ожидается.\n",
      "2024-01-01T00:00:05Z",
      Some(media_id)
    )
    .await;

  (base_url, state)
}

async fn init_editorial_workspace(base_url: &str) -> (WikiBackend, tempfile::TempDir, PathBuf) {
  let backend = WikiBackend::new(WikiConfig {
    base_url: base_url.to_string(),
    auth_token: "test-token".to_string(),
    org_id: None,
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500
  })
  .expect("backend");
  let tmp = tempfile::tempdir().expect("tempdir");
  let local_dir = tmp.path().to_path_buf();

  backend.init(&local_dir).await.expect("init editorial workspace");
  (backend, tmp, local_dir)
}

async fn refresh_workspace(backend: &WikiBackend) {
  let protection = NoPathProtection;
  let result = backend
    .refresh_remote(RemoteRefresh {
      protected_paths: &protection,
      mode: RemoteApplyMode::ApplySafe
    })
    .await
    .expect("refresh workspace");

  assert!(
    matches!(
      result,
      RemoteRefreshResult::Applied { .. } | RemoteRefreshResult::Unchanged
    ),
    "refresh should either apply remote changes or report no changes: {result:?}"
  );
}

async fn bump_modified_at(state: &common::FakeState, slug: &str, modified_at: &str) {
  let page = state.find_slug(slug).await.expect("page exists");
  state.update_content_by_slug(slug, &page.content, modified_at).await;
}

fn read_text(path: impl AsRef<Path>) -> String {
  std::fs::read_to_string(path).expect("read text file")
}

fn assert_contains_all(content: &str, markers: &[&str]) {
  for marker in markers {
    assert!(
      content.contains(marker),
      "content should contain {marker:?}:\n{content}"
    );
  }
}

fn assert_no_conflict_markers(content: &str) {
  assert!(
    !content.contains("<<<<<<<") && !content.contains("=======") && !content.contains(">>>>>>>"),
    "content should not contain conflict markers:\n{content}"
  );
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

/// Test 7: Init → change server page → refresh_remote → verify file updated on disk.
#[tokio::test]
async fn test_refresh_detects_remote_change_and_updates_disk() {
  eprintln!("[TEST] test_refresh_detects_remote_change_and_updates_disk");
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

    let protection = NoPathProtection;
    let result = backend
      .refresh_remote(RemoteRefresh {
        protected_paths: &protection,
        mode: RemoteApplyMode::ApplySafe
      })
      .await
      .expect("refresh");
    assert!(matches!(result, RemoteRefreshResult::Applied { .. }));

    let updated_content = std::fs::read_to_string(&root_path).expect("read updated");
    assert_eq!(
      updated_content, "# Changed remotely",
      "file should be updated after refresh_remote"
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

#[tokio::test]
async fn test_editorial_team_collaboration_converges_across_workspaces() {
  eprintln!("[TEST] test_editorial_team_collaboration_converges_across_workspaces");
  tokio::time::timeout(TEST_TIMEOUT, async {
    let (base_url, state) = setup_editorial_state().await;
    let (planner_backend, _planner_tmp, planner_dir) = init_editorial_workspace(&base_url).await;
    let (corrector_backend, _corrector_tmp, corrector_dir) = init_editorial_workspace(&base_url).await;
    let (producer_backend, _producer_tmp, producer_dir) = init_editorial_workspace(&base_url).await;

    let corrector_plan_path = corrector_dir.join("root/plan.md");
    let corrected_plan = read_text(&corrector_plan_path).replace("ачепяткой", "опечаткой");
    std::fs::write(&corrector_plan_path, corrected_plan).expect("write corrected plan");

    let remote_plan = format!(
      "{}\n- Каналы посева: Telegram, VK и email.\n- Ритм публикации: большая статья, затем короткие форматы.\n",
      state.find_slug("root/plan").await.expect("plan").content
    );
    state
      .update_content_by_slug("root/plan", &remote_plan, "2024-02-01T10:00:00Z")
      .await;

    let result = corrector_backend
      .sync(&[corrector_plan_path.clone()])
      .await
      .expect("sync merged plan");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "non-overlapping plan edits should merge: {result:?}"
    );
    bump_modified_at(&state, "root/plan", "2024-02-01T10:00:01Z").await;

    refresh_workspace(&planner_backend).await;
    refresh_workspace(&producer_backend).await;

    let plan_markers = [
      "Тезис: быстрый черновик с опечаткой",
      "Каналы посева: Telegram, VK и email",
      "Ритм публикации"
    ];
    for local_dir in [&planner_dir, &corrector_dir, &producer_dir] {
      let plan = read_text(local_dir.join("root/plan.md"));
      assert_contains_all(&plan, &plan_markers);
      assert!(!plan.contains("ачепяткой"), "typo should be gone from plan:\n{plan}");
      assert_no_conflict_markers(&plan);
    }

    let article = r"# Большая статья

## Лид
Материал объясняет запуск OmniFuse для публикацыи в нескольких медиа.

## Основной текст
Редакция сначала согласует план, потом готовит большую статью и короткие посевы.
Важный инвариант: исправления и новые пункты не теряются при параллельной работе.

## Финал
Эталонный текст должен сойтись у всех участников.
";
    let planner_article_path = planner_dir.join("root/article.md");
    std::fs::write(&planner_article_path, article).expect("write article");

    let result = planner_backend
      .sync(&[planner_article_path])
      .await
      .expect("sync article");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "article sync should succeed: {result:?}"
    );
    bump_modified_at(&state, "root/article", "2024-02-01T10:01:00Z").await;

    refresh_workspace(&producer_backend).await;

    let telegram_path = producer_dir.join("root/media/telegram.md");
    let vk_path = producer_dir.join("root/media/vk.md");
    std::fs::write(
      &telegram_path,
      "# Telegram\n\nКороткий посев: OmniFuse помогает редакции не терять правки.\n\nCTA: читать полную статью.\n"
    )
    .expect("write telegram");
    std::fs::write(
      &vk_path,
      "# VK\n\nСокращённая версия: план, статья и посевы синхронизируются между участниками.\n"
    )
    .expect("write vk");

    let result = producer_backend
      .sync(&[telegram_path, vk_path])
      .await
      .expect("sync media seeds");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "media seed sync should succeed: {result:?}"
    );
    bump_modified_at(&state, "root/media/telegram", "2024-02-01T10:02:00Z").await;
    bump_modified_at(&state, "root/media/vk", "2024-02-01T10:02:01Z").await;

    refresh_workspace(&corrector_backend).await;

    let corrector_article_path = corrector_dir.join("root/article.md");
    let corrected_article = read_text(&corrector_article_path).replace("публикацыи", "публикации");
    std::fs::write(&corrector_article_path, corrected_article).expect("write corrected article");

    let result = corrector_backend
      .sync(&[corrector_article_path])
      .await
      .expect("sync corrected article");
    assert!(
      matches!(result, SyncResult::Success { .. }),
      "corrected article sync should succeed: {result:?}"
    );
    bump_modified_at(&state, "root/article", "2024-02-01T10:03:00Z").await;

    refresh_workspace(&planner_backend).await;
    refresh_workspace(&producer_backend).await;

    for local_dir in [&planner_dir, &corrector_dir, &producer_dir] {
      let plan = read_text(local_dir.join("root/plan.md"));
      let article = read_text(local_dir.join("root/article.md"));
      let telegram = read_text(local_dir.join("root/media/telegram.md"));
      let vk = read_text(local_dir.join("root/media/vk.md"));

      assert_contains_all(&plan, &plan_markers);
      assert_contains_all(
        &article,
        &[
          "## Лид",
          "## Основной текст",
          "## Финал",
          "публикации в нескольких медиа",
          "Эталонный текст"
        ]
      );
      assert!(
        !article.contains("публикацыи"),
        "article typo should be fixed:\n{article}"
      );
      assert_contains_all(&telegram, &["Короткий посев", "читать полную статью"]);
      assert_contains_all(&vk, &["Сокращённая версия", "синхронизируются между участниками"]);

      for content in [&plan, &article, &telegram, &vk] {
        assert_no_conflict_markers(content);
      }
    }

    let server_article = state.find_slug("root/article").await.expect("server article").content;
    assert_contains_all(&server_article, &["публикации в нескольких медиа", "Эталонный текст"]);
    assert!(
      !server_article.contains("публикацыи"),
      "server article typo should be fixed:\n{server_article}"
    );
  })
  .await
  .expect("test timed out — possible deadlock");
}
