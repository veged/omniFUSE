# План реализации: GitSyncLifecycle

> **Для agentic workers:** выполняй план небольшими задачами. Не начинай общий core-refactor: этот план локален для `omnifuse-git`.

**Цель:** спрятать clone/startup sync/stage/commit/push retry/fetch/pull/classify/filter в один глубокий git-модуль с маленьким интерфейсом.

**Архитектура:** `GitBackend` остаётся адаптером под `omnifuse_core::Backend`. Новый `GitSyncLifecycle` становится владельцем `RepoSource`, `GitOps`, `GitignoreFilter`, политики повторов и remote refresh. Это локальный рефакторинг без изменения core trait на первом этапе.

**Технологии:** Rust, `tokio::process::Command`, существующие `GitEngine`, `GitOps`, `GitError`, локальные bare repositories в тестах.

---

## Файловая структура

- Создать: `crates/omnifuse-git/src/sync_lifecycle.rs` — глубокий фасад.
- Создать: `crates/omnifuse-git/src/tracking.rs` — правила отслеживания `.git/` и `.gitignore`.
- Изменить: `crates/omnifuse-git/src/lib.rs` — `GitBackend` делегирует в lifecycle.
- Изменить: `crates/omnifuse-git/src/ops.rs` — оставить низкоуровневые сценарии без семантики `Backend`.
- Изменить: `crates/omnifuse-git/src/repo_source.rs` — target path приходит от вызывающего кода.
- Изменить: `crates/omnifuse-git/src/error.rs` — classifier остаётся публичным.
- Test: `crates/omnifuse-git/tests/backend_integration.rs`.
- Test: unit tests в `sync_lifecycle.rs` и `tracking.rs`.

## Целевой интерфейс

```rust
pub struct GitSyncLifecycle {
  repo_path: PathBuf,
  ops: GitOps,
  tracking: GitTrackingRules,
  max_push_retries: u32
}

pub enum GitInit {
  UpToDate,
  Updated,
  Conflicts { files: Vec<PathBuf> },
  Offline
}

pub enum GitSync {
  Success { synced_files: usize },
  Conflict { files: Vec<PathBuf> },
  Offline
}

pub enum GitRefresh {
  NoChange,
  Applied { files: Vec<PathBuf>, merge: MergeSummary },
  Conflict { files: Vec<PathBuf> },
  Offline
}

impl GitSyncLifecycle {
  pub async fn open(config: GitConfig, local_dir: &Path) -> anyhow::Result<(Self, GitInit)>;
  pub async fn sync_local(&self, dirty: &[PathBuf]) -> anyhow::Result<GitSync>;
  pub async fn refresh_remote(&self) -> anyhow::Result<GitRefresh>;
  pub fn should_track(&self, path: &Path) -> bool;
  pub fn classify(&self, error: &anyhow::Error) -> omnifuse_core::ErrorKind;
}
```

## Задача 1: Вынести правила отслеживания

**Files:**
- Создать: `crates/omnifuse-git/src/tracking.rs`
- Изменить: `crates/omnifuse-git/src/lib.rs`

- [ ] **Шаг 1: Написать tests**

```rust
#[test]
fn tracking_rejects_git_directory() {
  let tmp = tempfile::tempdir().expect("tempdir");
  let rules = GitTrackingRules::new(tmp.path());

  assert!(!rules.accepts(Path::new(".git/config")));
  assert!(!rules.accepts(&tmp.path().join(".git/config")));
}

#[test]
fn tracking_uses_gitignore_after_init() {
  let tmp = tempfile::tempdir().expect("tempdir");
  std::fs::write(tmp.path().join(".gitignore"), "*.log\nbuild/\n").expect("gitignore");
  let rules = GitTrackingRules::new(tmp.path());

  assert!(!rules.accepts(&tmp.path().join("debug.log")));
  assert!(!rules.accepts(&tmp.path().join("build/output.bin")));
  assert!(rules.accepts(&tmp.path().join("src/lib.rs")));
}
```

Запуск:

```bash
cargo test -p omnifuse-git tracking
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `GitTrackingRules`**

```rust
pub struct GitTrackingRules {
  repo_path: PathBuf,
  filter: GitignoreFilter
}

impl GitTrackingRules {
  pub fn new(repo_path: &Path) -> Self;
  pub fn accepts(&self, path: &Path) -> bool;
}
```

Правило: `.git` всегда запрещён, даже если `.gitignore` разрешает.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-git tracking
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-git/src/tracking.rs crates/omnifuse-git/src/lib.rs
git commit -m "вынести правила отслеживания git"
```

## Задача 2: Создать `GitSyncLifecycle::open`

**Files:**
- Создать: `crates/omnifuse-git/src/sync_lifecycle.rs`
- Изменить: `crates/omnifuse-git/src/lib.rs`
- Изменить: `crates/omnifuse-git/src/repo_source.rs`

- [ ] **Шаг 1: Добавить lifecycle init tests**

```rust
#[tokio::test]
async fn open_local_repo_runs_startup_sync_and_tracking() {
  let (_tmp, repo_path) = crate::engine::tests::create_test_repo().await;
  let config = GitConfig {
    source: repo_path.to_string_lossy().into_owned(),
    branch: "main".to_string(),
    max_push_retries: 3,
    poll_interval_secs: 30,
    local_dir: repo_path.clone()
  };

  let (git, init) = GitSyncLifecycle::open(config, &repo_path).await.expect("open");

  assert!(matches!(init, GitInit::UpToDate | GitInit::Updated));
  assert!(git.should_track(Path::new("README.md")));
  assert!(!git.should_track(Path::new(".git/config")));
}
```

Запуск:

```bash
cargo test -p omnifuse-git sync_lifecycle
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `open`**

`open` делает:

1. `RepoSource::parse(&config.source)`.
2. `source.ensure_available_at(&config.branch, local_dir).await`.
3. `GitOps::new(repo_path, config.branch)`.
4. `ops.startup_sync().await`.
5. `GitTrackingRules::new(repo_path)`.
6. Возвращает `(GitSyncLifecycle, GitInit)`.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-git sync_lifecycle
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-git/src/sync_lifecycle.rs crates/omnifuse-git/src/repo_source.rs crates/omnifuse-git/src/lib.rs
git commit -m "добавить open для git sync lifecycle"
```

## Задача 3: Перенести `sync_local`

**Files:**
- Изменить: `crates/omnifuse-git/src/sync_lifecycle.rs`
- Изменить: `crates/omnifuse-git/src/lib.rs`
- Test: `crates/omnifuse-git/tests/backend_integration.rs`

- [ ] **Шаг 1: Добавить boundary tests**

```rust
#[tokio::test]
async fn sync_local_commits_and_pushes_dirty_files() {
  let (_tmp, _bare, clone1, _clone2) = create_bare_and_two_clones().await;
  let config = GitConfig {
    source: clone1.to_string_lossy().into_owned(),
    branch: "main".to_string(),
    max_push_retries: 3,
    poll_interval_secs: 30,
    local_dir: clone1.clone()
  };
  let (git, _) = GitSyncLifecycle::open(config, &clone1).await.expect("open");
  let file = clone1.join("new.txt");
  tokio::fs::write(&file, "new").await.expect("write");

  let result = git.sync_local(&[file]).await.expect("sync");

  assert!(matches!(result, GitSync::Success { synced_files: 1 }));
}

#[tokio::test]
async fn sync_local_reports_noop_as_success() {
  let (_tmp, repo_path) = crate::engine::tests::create_test_repo().await;
  let config = GitConfig {
    source: repo_path.to_string_lossy().into_owned(),
    branch: "main".to_string(),
    max_push_retries: 1,
    poll_interval_secs: 30,
    local_dir: repo_path.clone()
  };
  let (git, _) = GitSyncLifecycle::open(config, &repo_path).await.expect("open");

  let result = git.sync_local(&[repo_path.join("unchanged.txt")]).await.expect("sync");

  assert!(matches!(result, GitSync::Success { synced_files: 1 }));
}
```

- [ ] **Шаг 2: Реализовать `sync_local`**

Перенести логику из `GitBackend::sync`:

- `ops.auto_commit(dirty)`.
- `is_nothing_to_commit` трактовать как no-op success.
- `ops.push_with_retry(max_push_retries)`.
- `PushRejected/Conflict` -> `GitSync::Conflict`.
- Offline -> `GitSync::Offline`.

- [ ] **Шаг 3: Упростить `GitBackend::sync`**

`GitBackend` должен только:

```rust
match self.lifecycle()?.sync_local(dirty_files).await? {
  GitSync::Success { synced_files } => Ok(SyncResult::Success { synced_files }),
  GitSync::Conflict { files } => Ok(SyncResult::Conflict { synced_files: 0, conflict_files: files }),
  GitSync::Offline => Ok(SyncResult::Offline)
}
```

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-git backend_integration
cargo test -p omnifuse-git sync_lifecycle
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-git
git commit -m "перенести git local sync в lifecycle"
```

## Задача 4: Перенести remote refresh

**Files:**
- Изменить: `crates/omnifuse-git/src/sync_lifecycle.rs`
- Изменить: `crates/omnifuse-git/src/lib.rs`

- [ ] **Шаг 1: Добавить tests remote refresh**

```rust
#[tokio::test]
async fn refresh_remote_applies_new_remote_commit() {
  let (_tmp, _bare, clone1, clone2) = create_bare_and_two_clones().await;

  tokio::fs::write(clone2.join("remote.txt"), "remote").await.expect("write");
  let engine2 = GitEngine::new(clone2.clone(), "main".to_string()).expect("engine2");
  engine2.stage(&[clone2.join("remote.txt")]).await.expect("stage");
  engine2.commit("remote change").await.expect("commit");
  engine2.push().await.expect("push");

  let config = GitConfig {
    source: clone1.to_string_lossy().into_owned(),
    branch: "main".to_string(),
    max_push_retries: 3,
    poll_interval_secs: 30,
    local_dir: clone1.clone()
  };
  let (git, _) = GitSyncLifecycle::open(config, &clone1).await.expect("open");

  let result = git.refresh_remote().await.expect("refresh");

  assert!(matches!(result, GitRefresh::Applied { .. }));
  assert!(clone1.join("remote.txt").exists());
}
```

- [ ] **Шаг 2: Реализовать `refresh_remote`**

Алгоритм:

1. Запомнить local head.
2. `fetch`.
3. Получить remote head.
4. Если head равны — `NoChange`.
5. Получить changed files через `git diff --name-only`.
6. Выполнить `pull`.
7. `MergeResult::Conflict` -> `GitRefresh::Conflict`.
8. Иначе `Applied`.

- [ ] **Шаг 3: Обновить `GitBackend::poll_remote/apply_remote`**

До общего `RemoteRefresh` контракта сохранить совместимость:

- `poll_remote` может вызывать lightweight inspect или старый diff.
- `apply_remote` делегирует `refresh_remote`.

После плана `remote-refresh-contract` эти методы будут удалены.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-git
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-git
git commit -m "перенести git remote refresh в lifecycle"
```

## Задача 5: Удалить дублирование из `GitBackend`

**Files:**
- Изменить: `crates/omnifuse-git/src/lib.rs`

- [ ] **Шаг 1: Заменить `OnceLock<GitOps>` и `OnceLock<GitignoreFilter>`**

Использовать один `OnceLock<GitSyncLifecycle>`:

```rust
pub struct GitBackend {
  config: GitConfig,
  lifecycle: OnceLock<GitSyncLifecycle>
}
```

- [ ] **Шаг 2: Удалить `diff_remote_files` из `GitBackend`**

Эта функция должна жить только в lifecycle или низкоуровневом helper, не в адаптере.

- [ ] **Шаг 3: Проверить workspace slice**

Запуск:

```bash
cargo test -p omnifuse-git
cargo test -p omnifuse-core e2e_pipeline_tests
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-git
git commit -m "упростить git backend adapter"
```

## Риски / Предположения

- Тесты git могут быть медленными; не заменять реальные local bare repo тесты моками, потому что семантика Git CLI — существенная часть поведения.
- Не добавлять generic `GitRepositoryPort`, пока нет второго git engine.
- `GitSyncLifecycle` не должен владеть core `SyncEngine` dirty set. Его ответственность — git workflow, не timing.

## Критерий завершения

- `GitBackend` стал тонким адаптером “core trait -> lifecycle result”.
- Вся sync/push/pull/error policy живёт в `sync_lifecycle.rs`.
- `.gitignore` и `.git/` filtering живут в `tracking.rs`.
- Тесты:

```bash
cargo test -p omnifuse-git
cargo test -p omnifuse-core e2e_pipeline_tests
```
