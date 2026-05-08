# План реализации: RemoteRefresh contract

> **Для agentic workers:** это cross-crate refactor. Начинай только после локализации Git/Wiki lifecycle или будь готов менять оба backend crates одновременно.

**Цель:** заменить неоднозначный контракт `poll_remote() -> Vec<RemoteChange>` + `apply_remote(Vec<RemoteChange>)` на честный `refresh_remote`, где backend сам обнаруживает и безопасно применяет remote изменения.

**Архитектура:** `SyncEngine::poll_worker` больше не получает данные изменений. Он вызывает `Backend::refresh_remote(RemoteRefresh { protected_paths, mode })`; backend возвращает доменный результат. Для защиты dirty-файлов вводится общий `DirtyIndex`, который `dirty_worker` обновляет, а `poll_worker` читает.

**Технологии:** `omnifuse-core`, `dashmap`/`DashSet` или `tokio::sync::RwLock<HashSet<PathBuf>>`, текущие Git/Wiki lifecycle modules.

---

## Файловая структура

- Изменить: `crates/omnifuse-core/src/backend.rs` — новый контракт.
- Создать: `crates/omnifuse-core/src/dirty_index.rs` — общий набор dirty paths.
- Изменить: `crates/omnifuse-core/src/sync_engine.rs` — `dirty_worker` и `poll_worker` используют `DirtyIndex`.
- Изменить: `crates/omnifuse-core/src/test_utils.rs` — `MockBackend` поддерживает результаты refresh.
- Изменить: `crates/omnifuse-git/src/lib.rs` — реализовать refresh через `GitSyncLifecycle`.
- Изменить: `crates/omnifuse-wiki/src/lib.rs` — реализовать refresh через `WikiPageSyncSession`.
- Test: `crates/omnifuse-core/src/sync_engine.rs`.
- Test: `crates/omnifuse-core/tests/e2e_cross_backend_scenarios.rs`.
- Test: `crates/omnifuse-git/tests/backend_integration.rs`.
- Test: `crates/omnifuse-wiki/tests/backend_integration.rs`.

## Целевой интерфейс

```rust
pub trait Backend: Send + Sync + 'static {
  fn init(&self, local_dir: &Path) -> impl Future<Output = anyhow::Result<InitResult>> + Send;
  fn sync(&self, dirty_files: &[PathBuf]) -> impl Future<Output = anyhow::Result<SyncResult>> + Send;
  fn refresh_remote(&self, request: RemoteRefresh<'_>) -> impl Future<Output = anyhow::Result<RemoteRefreshResult>> + Send;
  fn should_track(&self, path: &Path) -> bool;
  fn poll_interval(&self) -> Duration;
  fn is_online(&self) -> impl Future<Output = bool> + Send;
  fn name(&self) -> &'static str;
  fn classify_error(&self, error: &anyhow::Error) -> ErrorKind;
}

pub struct RemoteRefresh<'a> {
  pub protected_paths: &'a dyn PathProtection,
  pub mode: RemoteApplyMode
}

pub trait PathProtection {
  fn is_protected(&self, path: &Path) -> bool;
}

pub enum RemoteApplyMode {
  ApplySafe,
  DetectOnly
}

pub enum RemoteRefreshResult {
  Unchanged,
  Applied { changed: Vec<PathBuf>, deleted: Vec<PathBuf> },
  Deferred { affected: Vec<PathBuf>, reason: RemoteDeferReason },
  Offline
}
```

## Задача 1: Добавить `DirtyIndex`

**Files:**
- Создать: `crates/omnifuse-core/src/dirty_index.rs`
- Изменить: `crates/omnifuse-core/src/lib.rs`

- [ ] **Шаг 1: Написать tests**

```rust
#[test]
fn dirty_index_tracks_and_untracks_paths() {
  let index = DirtyIndex::default();
  let path = PathBuf::from("docs/page.md");

  index.mark_dirty(path.clone());
  assert!(index.is_protected(&path));

  index.mark_clean(&path);
  assert!(!index.is_protected(&path));
}

#[test]
fn dirty_index_protects_descendants_when_directory_is_dirty() {
  let index = DirtyIndex::default();
  index.mark_dirty(PathBuf::from("docs"));

  assert!(index.is_protected(Path::new("docs/page.md")));
}
```

Запуск:

```bash
cargo test -p omnifuse-core dirty_index
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `DirtyIndex`**

```rust
#[derive(Default)]
pub struct DirtyIndex {
  paths: RwLock<HashSet<PathBuf>>
}

impl DirtyIndex {
  pub fn mark_dirty(&self, path: PathBuf);
  pub fn mark_clean(&self, path: &Path);
  pub fn snapshot(&self) -> Vec<PathBuf>;
}

impl PathProtection for DirtyIndex {
  fn is_protected(&self, path: &Path) -> bool;
}
```

Если нужен async lock, методы сделать async; тогда `RemoteRefresh` получает snapshot guard, а не сам index.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core dirty_index
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core/src/dirty_index.rs crates/omnifuse-core/src/lib.rs
git commit -m "добавить общий dirty index"
```

## Задача 2: Изменить `Backend` contract

**Files:**
- Изменить: `crates/omnifuse-core/src/backend.rs`
- Изменить: `crates/omnifuse-core/src/test_utils.rs`

- [ ] **Шаг 1: Добавить types и временный compatibility method**

На один переходный коммит допустимо оставить устаревшие default-методы:

```rust
fn poll_remote(&self) -> impl Future<Output = anyhow::Result<Vec<RemoteChange>>> + Send {
  async { Ok(Vec::new()) }
}

fn apply_remote(&self, _changes: Vec<RemoteChange>) -> impl Future<Output = anyhow::Result<()>> + Send {
  async { Ok(()) }
}
```

Но `SyncEngine` в следующей задаче уже должен использовать `refresh_remote`.

- [ ] **Шаг 2: Обновить `MockBackend`**

Добавить:

```rust
pub refresh_calls: Arc<AtomicUsize>,
pub refresh_result: Arc<Mutex<RemoteRefreshResult>>,
pub refresh_error: Arc<Mutex<Option<String>>>
```

и helper:

```rust
pub fn set_refresh_result(&self, result: RemoteRefreshResult);
```

- [ ] **Шаг 3: Проверить compile**

Запуск:

```bash
cargo test -p omnifuse-core --lib backend
```

Ожидание: PASS или compile errors только в backend crates, которые будут исправлены далее.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core/src/backend.rs crates/omnifuse-core/src/test_utils.rs
git commit -m "добавить backend contract для remote refresh"
```

## Задача 3: Перевести `SyncEngine::poll_worker`

**Files:**
- Изменить: `crates/omnifuse-core/src/sync_engine.rs`

- [ ] **Шаг 1: Добавить тесты на результаты refresh**

```rust
#[tokio::test]
async fn poll_worker_calls_refresh_remote_and_reports_applied() {
  let backend = Arc::new(MockBackend {
    poll_interval_dur: Duration::from_millis(10),
    ..MockBackend::new()
  });
  backend.set_refresh_result(RemoteRefreshResult::Applied {
    changed: vec![PathBuf::from("remote.md")],
    deleted: Vec::new()
  });

  let events = Arc::new(TestEventHandler::new());
  let (engine, handle) = SyncEngine::start(SyncConfig::default(), backend.clone(), events.clone());

  tokio::time::sleep(Duration::from_millis(50)).await;
  engine.shutdown().await.expect("shutdown");
  handle.await.expect("join");

  assert!(backend.refresh_call_count() > 0);
  assert!(events.sync_calls.lock().expect("lock").iter().any(|v| v == "updated"));
}

#[tokio::test]
async fn poll_worker_defers_when_remote_touches_dirty_path() {
  let backend = Arc::new(MockBackend {
    poll_interval_dur: Duration::from_millis(10),
    ..MockBackend::new()
  });
  backend.set_refresh_result(RemoteRefreshResult::Deferred {
    affected: vec![PathBuf::from("dirty.md")],
    reason: RemoteDeferReason::ProtectedLocalChange
  });

  let events = Arc::new(TestEventHandler::new());
  let (engine, handle) = SyncEngine::start(SyncConfig::default(), backend, events.clone());

  tokio::time::sleep(Duration::from_millis(50)).await;
  engine.shutdown().await.expect("shutdown");
  handle.await.expect("join");

  assert!(events.log_count(LogLevel::Warn) >= 1);
}
```

- [ ] **Шаг 2: Интегрировать `DirtyIndex`**

`dirty_worker`:

- На `FileModified/FileClosed` вызывает `dirty_index.mark_dirty(path)`.
- После успешной синхронизации вызывает `dirty_index.mark_clean(path)` для изменённых файлов.
- При отсутствии сети или ошибке оставляет paths грязными.

`poll_worker`:

```rust
let request = RemoteRefresh {
  protected_paths: dirty_index.as_path_protection(),
  mode: RemoteApplyMode::ApplySafe
};
let result = backend.refresh_remote(request).await?;

match result {
  RemoteRefreshResult::Unchanged => {}
  RemoteRefreshResult::Applied(summary) => emit_remote_applied(summary).await?,
  RemoteRefreshResult::Deferred(reason) => emit_remote_deferred(reason).await?,
  RemoteRefreshResult::Offline(reason) => log_remote_offline(reason),
}
```

- [ ] **Шаг 3: Удалить `poll_remote/apply_remote` из `poll_worker`**

События:

- `Unchanged` — ничего.
- `Applied` — `events.on_sync("updated")` и typed event.
- `Deferred` — предупреждение, без применения.
- `Offline` — предупреждение или debug-лог, без ошибки.
- `Err` — `RemotePollFailed`.

- [ ] **Шаг 4: Проверить core**

Запуск:

```bash
cargo test -p omnifuse-core sync_engine
cargo test -p omnifuse-core e2e_cross_backend_scenarios
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-core/src/sync_engine.rs crates/omnifuse-core/src/dirty_index.rs
git commit -m "использовать remote refresh в sync engine"
```

## Задача 4: Реализовать refresh в GitBackend

**Files:**
- Изменить: `crates/omnifuse-git/src/lib.rs`
- Изменить: `crates/omnifuse-git/src/sync_lifecycle.rs`

- [ ] **Шаг 1: Добавить conflict/protected tests**

```rust
#[tokio::test]
async fn git_refresh_defers_when_remote_change_hits_protected_path() {
  let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;
  create_remote_commit(&clone2, "shared.txt", "remote").await;

  let backend = initialized_git_backend_for(&clone1).await;
  let protected = StaticPathProtection::new(vec![clone1.join("shared.txt")]);

  let result = backend.refresh_remote(RemoteRefresh {
    protected_paths: &protected,
    mode: RemoteApplyMode::ApplySafe
  }).await.expect("refresh");

  assert!(matches!(result, RemoteRefreshResult::Deferred { .. }));
}
```

- [ ] **Шаг 2: Реализовать через lifecycle**

Алгоритм:

1. Inspect changed remote files.
2. Если affected пересекается с protected paths — `Deferred`.
3. Иначе `refresh_remote`.
4. Map `GitRefresh` в `RemoteRefreshResult`.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-git
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-git
git commit -m "реализовать remote refresh для git backend"
```

## Задача 5: Реализовать refresh в WikiBackend

**Files:**
- Изменить: `crates/omnifuse-wiki/src/lib.rs`
- Изменить: `crates/omnifuse-wiki/src/session.rs`

- [ ] **Шаг 1: Добавить protected test**

```rust
#[tokio::test]
async fn wiki_refresh_defers_protected_remote_page() {
  let (backend, state, tmp, _url) = setup_backend().await;
  let local_dir = tmp.path().to_path_buf();
  backend.init(&local_dir).await.expect("init");
  state.update_content_by_slug("root", "# Remote", "2024-02-01T00:00:00Z").await;

  let protected = StaticPathProtection::new(vec![local_dir.join("root.md")]);
  let result = backend.refresh_remote(RemoteRefresh {
    protected_paths: &protected,
    mode: RemoteApplyMode::ApplySafe
  }).await.expect("refresh");

  assert!(matches!(result, RemoteRefreshResult::Deferred { .. }));
}
```

- [ ] **Шаг 2: Реализовать**

`WikiPageSyncSession`:

- `poll_remote` строит batch.
- Перед `apply_remote` проверить `protected_paths`.
- Если пересечение есть — `Deferred`, batch не применять.
- Иначе apply и вернуть changed/deleted paths.

- [ ] **Шаг 3: Удалить compatibility pending cache**

Если `Backend::poll_remote/apply_remote` больше не используются, удалить `pending_remote`.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-wiki
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-wiki
git commit -m "реализовать remote refresh для wiki backend"
```

## Задача 6: Удалить старый `RemoteChange` contract

**Files:**
- Изменить: `crates/omnifuse-core/src/backend.rs`
- Изменить: tests referencing `poll_remote/apply_remote`

- [ ] **Шаг 1: Удалить `poll_remote` и `apply_remote` из `Backend`**

Оставить `RemoteChange` только если его ещё использует Wiki внутренняя модель. Если внешний core больше не использует, перенести тип в `omnifuse-wiki`.

- [ ] **Шаг 2: Обновить tests**

Тесты должны проверять:

- `refresh_remote` called.
- `Applied` emits update event.
- `Deferred` не перетирает локальный dirty-файл.
- Offline не падает.

- [ ] **Шаг 3: Проверить workspace slice**

Запуск:

```bash
cargo test -p omnifuse-core
cargo test -p omnifuse-git
cargo test -p omnifuse-wiki
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core crates/omnifuse-git crates/omnifuse-wiki
git commit -m "заменить remote change contract на refresh"
```

## Риски / Предположения

- `DirtyIndex` меняет семантику конкурентности. Проверять race-тесты обязательно.
- Git remote apply может конфликтовать на уровне merge даже без protected path overlap; это остаётся `Conflict`.
- `DetectOnly` можно реализовать минимально или оставить используемым только в тестах до появления UI preview.

## Критерий завершения

- `SyncEngine::poll_worker` не знает о `RemoteChange`.
- Git и Wiki реализуют `refresh_remote`.
- Dirty local paths защищены от silent remote overwrite.
- Тесты:

```bash
cargo test -p omnifuse-core
cargo test -p omnifuse-git
cargo test -p omnifuse-wiki
```
