# План реализации: WikiPageSyncSession

> **Для agentic workers:** этот план локален для `omnifuse-wiki`. Не начинай с общего `Backend` trait refactor.

**Цель:** собрать invariant `slug <-> path <-> .vfs/meta <-> .vfs/base <-> remote page` в один session-модуль.

**Архитектура:** `WikiBackend` остаётся адаптером под `omnifuse_core::Backend`. Новый `WikiPageSyncSession` владеет `Client`, `MetaStore`, локальным каталогом, индексом страниц, materialization дерева, локальной dirty-синхронизацией и пакетным применением remote. `Client` остаётся конкретной зависимостью; `FakeWikiApi` покрывает границу.

**Технологии:** Rust, `reqwest` через текущий `Client`, `diffy`, `serde_json`, temp dirs, `FakeWikiApi`.

---

## Файловая структура

- Создать: `crates/omnifuse-wiki/src/page.rs` — `Slug`, `PageRef`, `PageSnapshot`, кодек path.
- Создать: `crates/omnifuse-wiki/src/page_index.rs` — двунаправленный индекс slug/path.
- Создать: `crates/omnifuse-wiki/src/session.rs` — `WikiPageSyncSession`.
- Изменить: `crates/omnifuse-wiki/src/lib.rs` — `WikiBackend` делегирует в session.
- Изменить: `crates/omnifuse-wiki/src/meta.rs` — оставить store, но убрать решения о path из backend.
- Изменить: `crates/omnifuse-wiki/src/merge.rs` — вернуть доменное решение merge.
- Test: `crates/omnifuse-wiki/tests/backend_integration.rs`.
- Test: `crates/omnifuse-wiki/tests/e2e_wiki_user_scenarios.rs`.
- Test: unit tests в новых модулях.

## Целевой интерфейс

```rust
pub struct WikiPageSyncSession {
  config: WikiConfig,
  client: Arc<Client>,
  local_dir: PathBuf,
  meta_store: MetaStore,
  page_index: tokio::sync::RwLock<PageIndex>,
  pending_remote: tokio::sync::Mutex<Option<RemoteBatch>>
}

pub struct DirtyBatch<'a> {
  pub paths: &'a [PathBuf]
}

pub struct RemoteBatch {
  pub changes: Vec<RemoteChange>,
  pub snapshots: Vec<(PageRef, PageSnapshot)>
}

impl WikiPageSyncSession {
  pub async fn attach(config: WikiConfig, client: Arc<Client>, local_dir: &Path) -> anyhow::Result<Self>;
  pub async fn initialize(&self) -> anyhow::Result<InitResult>;
  pub async fn sync_dirty(&self, dirty: DirtyBatch<'_>) -> anyhow::Result<SyncResult>;
  pub async fn poll_remote(&self) -> anyhow::Result<RemoteBatch>;
  pub async fn apply_remote(&self, batch: RemoteBatch) -> anyhow::Result<ApplyReport>;
  pub fn should_track(&self, path: &Path) -> bool;
}
```

## Задача 1: Ввести page domain types

**Files:**
- Создать: `crates/omnifuse-wiki/src/page.rs`
- Создать: `crates/omnifuse-wiki/src/page_index.rs`
- Изменить: `crates/omnifuse-wiki/src/lib.rs`

- [ ] **Шаг 1: Написать tests для path/slug codec**

```rust
#[test]
fn page_ref_maps_slug_to_markdown_path() {
  let local_dir = Path::new("/mnt/wiki");
  let page = PageRef::from_slug(local_dir, Slug::new("root/docs")).expect("page");

  assert_eq!(page.slug.as_str(), "root/docs");
  assert_eq!(page.path, PathBuf::from("/mnt/wiki/root/docs.md"));
}

#[test]
fn page_ref_rejects_vfs_metadata_paths() {
  let local_dir = Path::new("/mnt/wiki");

  assert!(PageRef::from_path(local_dir, Path::new("/mnt/wiki/.vfs/meta/root.json")).is_none());
  assert!(PageRef::from_path(local_dir, Path::new("/mnt/wiki/root/docs.md")).is_some());
}

#[test]
fn page_index_resolves_by_slug_and_path() {
  let local_dir = Path::new("/mnt/wiki");
  let page = PageRef::from_slug(local_dir, Slug::new("root/docs")).expect("page");
  let mut index = PageIndex::default();

  index.insert(page.clone(), PageSnapshot {
    id: 7,
    title: "Docs".to_string(),
    modified_at: "2024-01-01T00:00:00Z".to_string()
  });

  assert_eq!(index.by_slug(page.slug.as_str()).map(|p| p.ref_.path.clone()), Some(page.path.clone()));
  assert_eq!(index.by_path(&page.path).map(|p| p.ref_.slug.as_str()), Some("root/docs"));
}
```

Запуск:

```bash
cargo test -p omnifuse-wiki page
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `Slug`, `PageRef`, `PageSnapshot`, `PageIndex`**

Правила:

- slug хранится без `.md`.
- `.vfs` никогда не становится page.
- path всегда absolute под `local_dir`.
- Windows separator нормализуется в `/` для slug.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki page
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-wiki/src/page.rs crates/omnifuse-wiki/src/page_index.rs crates/omnifuse-wiki/src/lib.rs
git commit -m "добавить доменную модель wiki page"
```

## Задача 2: Реализовать `attach` и `initialize`

**Files:**
- Создать: `crates/omnifuse-wiki/src/session.rs`
- Изменить: `crates/omnifuse-wiki/src/lib.rs`
- Изменить: `crates/omnifuse-wiki/src/meta.rs`

- [ ] **Шаг 1: Добавить integration test**

```rust
#[tokio::test]
async fn session_initialize_downloads_tree_and_indexes_pages() {
  let (base_url, state) = FakeWikiApi::spawn().await;
  let root_id = state.add_page("root", "Root", "# Root", "2024-01-01T00:00:00Z", None).await;
  state.add_page("root/docs", "Docs", "# Docs", "2024-01-01T00:00:01Z", Some(root_id)).await;

  let config = WikiConfig {
    base_url,
    auth_token: "token".to_string(),
    org_id: None,
    root_slug: "root".to_string(),
    poll_interval_secs: 60,
    max_depth: 10,
    max_pages: 500
  };
  let client = Arc::new(Client::new(&config.base_url, &config.auth_token, None).expect("client"));
  let tmp = tempfile::tempdir().expect("tempdir");
  let session = WikiPageSyncSession::attach(config, client, tmp.path()).await.expect("attach");

  let result = session.initialize().await.expect("init");

  assert!(matches!(result, InitResult::Updated));
  assert_eq!(std::fs::read_to_string(tmp.path().join("root.md")).expect("root"), "# Root");
  assert!(tmp.path().join(".vfs/meta/root.json").exists());
  assert!(tmp.path().join(".vfs/base/root.md").exists());
}
```

Запуск:

```bash
cargo test -p omnifuse-wiki session_initialize
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `attach`**

`attach` создаёт:

- `MetaStore::new(local_dir)`.
- `PageIndex::default()`.
- `pending_remote = None`.
- Не делает network I/O.

- [ ] **Шаг 3: Реализовать `initialize`**

Алгоритм:

1. `create_dir_all(local_dir)`.
2. `client.get_page_tree(root_slug, max_pages, max_depth)`.
3. Для каждого node скачать content через `get_page_by_slug`.
4. Создать `PageRef` и `PageSnapshot`.
5. Записать local `.md`, meta, base.
6. Обновить `PageIndex`.
7. Вернуть `Updated`, если изменился хотя бы один local file, иначе `UpToDate`.
8. При offline: если `MetaStore::all_slugs()` пустой — error, иначе `Offline`.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki session_initialize
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-wiki/src/session.rs crates/omnifuse-wiki/src/lib.rs crates/omnifuse-wiki/src/meta.rs
git commit -m "добавить инициализацию wiki page sync session"
```

## Задача 3: Перенести локальную dirty-синхронизацию

**Files:**
- Изменить: `crates/omnifuse-wiki/src/session.rs`
- Изменить: `crates/omnifuse-wiki/src/merge.rs`
- Изменить: `crates/omnifuse-wiki/src/lib.rs`

- [ ] **Шаг 1: Добавить tests для create/update/merge**

```rust
#[tokio::test]
async fn sync_dirty_creates_new_page_and_updates_snapshot() {
  let (session, state, tmp) = setup_session_with_root().await;
  session.initialize().await.expect("init");
  let path = tmp.path().join("root/new-page.md");
  std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
  std::fs::write(&path, "# New").expect("write");

  let result = session.sync_dirty(DirtyBatch { paths: &[path.clone()] }).await.expect("sync");

  assert!(matches!(result, SyncResult::Success { synced_files: 1 }));
  assert!(state.find_slug("root/new-page").await.is_some());
}

#[tokio::test]
async fn sync_dirty_auto_merges_non_overlapping_remote_change() {
  let (session, state, tmp) = setup_session_with_root_content("line1\nline2\nline3").await;
  session.initialize().await.expect("init");
  let path = tmp.path().join("root.md");
  std::fs::write(&path, "LOCAL\nline2\nline3").expect("local");
  state.update_content_by_slug("root", "line1\nline2\nREMOTE", "2024-02-01T00:00:00Z").await;

  let result = session.sync_dirty(DirtyBatch { paths: &[path.clone()] }).await.expect("sync");

  assert!(matches!(result, SyncResult::Success { synced_files: 1 }));
  let content = std::fs::read_to_string(&path).expect("merged");
  assert!(content.contains("LOCAL"));
  assert!(content.contains("REMOTE"));
}
```

- [ ] **Шаг 2: Ввести доменный `MergeDecision`**

В `merge.rs` оставить алгоритм, но результат поднять выше:

```rust
pub enum MergeDecision {
  UploadLocal,
  UploadMerged(String),
  AcceptRemote(String),
  AlreadySynced,
  Conflict
}
```

`sync_dirty` не должен интерпретировать `MergeResult::NoConflict` как “что-то само понятно”; действие должно быть явным.

- [ ] **Шаг 3: Реализовать `sync_dirty`**

Для каждого tracked path:

1. `PageRef::from_path`.
2. Read local content.
3. Если нет snapshot — create remote page, save meta/base/index.
4. Если snapshot есть — download remote.
5. Если `modified_at` совпадает — update remote local content.
6. Если remote changed — merge decision.
7. При merged content — update remote, write local merged, save base/meta/index.
8. При conflict — local wins только если это текущая продуктовая политика; вернуть `SyncResult::Conflict`.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki backend_integration
cargo test -p omnifuse-wiki e2e_wiki_user_scenarios
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-wiki
git commit -m "перенести wiki dirty sync в page session"
```

## Задача 4: Перенести remote batch poll/apply

**Files:**
- Изменить: `crates/omnifuse-wiki/src/session.rs`
- Изменить: `crates/omnifuse-wiki/src/lib.rs`

- [ ] **Шаг 1: Добавить tests**

```rust
#[tokio::test]
async fn poll_remote_returns_batch_with_content_and_snapshot() {
  let (session, state, tmp) = setup_session_with_root().await;
  session.initialize().await.expect("init");
  state.update_content_by_slug("root", "# Remote", "2024-02-01T00:00:00Z").await;

  let batch = session.poll_remote().await.expect("poll");

  assert_eq!(batch.changes.len(), 1);
  assert!(batch.snapshots.iter().any(|(page, _)| page.slug.as_str() == "root"));
}

#[tokio::test]
async fn apply_remote_updates_file_base_meta_and_index_together() {
  let (session, state, tmp) = setup_session_with_root().await;
  session.initialize().await.expect("init");
  state.update_content_by_slug("root", "# Remote", "2024-02-01T00:00:00Z").await;
  let batch = session.poll_remote().await.expect("poll");

  session.apply_remote(batch).await.expect("apply");

  assert_eq!(std::fs::read_to_string(tmp.path().join("root.md")).expect("file"), "# Remote");
  assert_eq!(std::fs::read_to_string(tmp.path().join(".vfs/base/root.md")).expect("base"), "# Remote");
}
```

- [ ] **Шаг 2: Реализовать `RemoteBatch`**

`RemoteBatch` должен хранить enough data для atomic-ish apply:

- `RemoteChange` для core adapter.
- `PageSnapshot` для meta.
- `PageRef` для path/slug.

- [ ] **Шаг 3: Обновить `WikiBackend`**

До общего `RemoteRefresh` контракта:

- `poll_remote` вызывает `session.poll_remote()`, сохраняет `RemoteBatch` в `pending_remote`, наружу отдаёт `batch.changes`.
- `apply_remote` берёт `pending_remote`, сверяет paths, применяет batch.
- Если `pending_remote` отсутствует, reconstruct через changes только для backward compatibility.

После плана `remote-refresh-contract` этот compatibility cache можно удалить.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-wiki
git commit -m "перенести wiki remote sync в page session"
```

## Задача 5: Упростить `WikiBackend`

**Files:**
- Изменить: `crates/omnifuse-wiki/src/lib.rs`

- [ ] **Шаг 1: Заменить поля backend**

```rust
pub struct WikiBackend {
  config: WikiConfig,
  client: Arc<Client>,
  session: OnceLock<WikiPageSyncSession>
}
```

- [ ] **Шаг 2: Удалить `write_tree`, `sync_file`, `collect_changes` из `lib.rs`**

Эти функции должны быть приватными методами session или небольшими helpers рядом с session.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-wiki
git commit -m "упростить wiki backend adapter"
```

## Риски / Предположения

- Удаление remote pages сейчас не является полноценным локальным sync-поведением; не расширять семантику без отдельного требования.
- `pending_remote` — временный compatibility слой. Его нужно удалить после перехода на `RemoteRefresh`.
- Не выносить `Client` в trait до реальной необходимости: `FakeWikiApi` уже даёт качественные boundary-тесты.

## Критерий завершения

- `WikiBackend` делегирует `init/sync/poll/apply/should_track` в session.
- Все решения по slug/path/meta/base находятся в `page`, `page_index`, `session`.
- Тесты:

```bash
cargo test -p omnifuse-wiki
```
