# План реализации: MountService и единая подготовка mount

> **Для agentic workers:** используй `subagent-driven-development` или `executing-plans`. Выполняй задачи по чекбоксам, с отдельным коммитом после каждой логически завершённой задачи.

**Цель:** вынести подготовку `MountConfig`, backend config и cache layout из CLI/GUI/backend source в один модуль прикладного слоя.

**Архитектура:** создать crate `omnifuse-app`, который зависит от `omnifuse-core`, `omnifuse-git` и `omnifuse-wiki`. Публичная поверхность для CLI/GUI — `MountService::{prepare_git, prepare_wiki, run_git, run_wiki}`; внутри — маленький `MountPreparer`, отвечающий за `MountLayout`, cache key и дефолты.

**Технологии:** Rust workspace, `anyhow`, `tokio`, существующие backend crates, без новых внешних зависимостей.

---

## Файловая структура

- Создать: `crates/omnifuse-app/Cargo.toml` — crate прикладного слоя для общей подготовки mount.
- Создать: `crates/omnifuse-app/src/lib.rs` — публичные экспорты.
- Создать: `crates/omnifuse-app/src/mount_service.rs` — `MountService`, `GitMountArgs`, `WikiMountArgs`, `PreparedMount`.
- Создать: `crates/omnifuse-app/src/mount_layout.rs` — `MountLayout`, cache root, cache key, canonicalization.
- Создать: `crates/omnifuse-app/src/environment.rs` — `MountEnvironment`, `StdMountEnvironment`, тестовый fake.
- Изменить: `Cargo.toml` — добавить `crates/omnifuse-app` в workspace.
- Изменить: `crates/omnifuse-cli/Cargo.toml` — заменить прямые зависимости на `omnifuse-app` там, где возможно.
- Изменить: `crates/omnifuse-cli/src/main.rs` — удалить локальный `cache_dir_for`, использовать `MountService`.
- Изменить: `crates/omnifuse-gui/tauri/Cargo.toml` — добавить `omnifuse-app`.
- Изменить: `crates/omnifuse-gui/tauri/src/commands.rs` — использовать `MountService`.
- Изменить: `crates/omnifuse-git/src/repo_source.rs` — убрать самостоятельный выбор cache для remote-path из runtime-path или сделать его только запасным путём.
- Test: `crates/omnifuse-app/src/mount_layout.rs`.
- Test: `crates/omnifuse-app/src/mount_service.rs`.
- Test: `crates/omnifuse-cli/tests/cli_tests.rs`.

## Целевой интерфейс

```rust
pub struct MountService<E = StdMountEnvironment> {
  env: E,
  defaults: MountDefaults
}

pub struct GitMountArgs {
  pub source: String,
  pub mount_point: PathBuf,
  pub branch: Option<String>,
  pub poll_interval_secs: Option<u64>,
  pub allow_other: bool,
  pub read_only: bool
}

pub struct WikiMountArgs {
  pub base_url: String,
  pub root_slug: String,
  pub auth_token: String,
  pub org_id: Option<String>,
  pub mount_point: PathBuf,
  pub poll_interval_secs: Option<u64>,
  pub allow_other: bool,
  pub read_only: bool
}

pub struct PreparedMount<B> {
  pub config: MountConfig,
  pub backend: B,
  pub layout: MountLayout
}

impl MountService {
  pub fn prepare_git(&self, args: GitMountArgs) -> anyhow::Result<PreparedMount<GitBackend>>;
  pub fn prepare_wiki(&self, args: WikiMountArgs) -> anyhow::Result<PreparedMount<WikiBackend>>;

  pub async fn run_git(&self, args: GitMountArgs, events: impl VfsEventHandler) -> anyhow::Result<()>;
  pub async fn run_wiki(&self, args: WikiMountArgs, events: impl VfsEventHandler) -> anyhow::Result<()>;
}
```

## Задача 1: Создать crate прикладного слоя и политику layout

**Files:**
- Создать: `crates/omnifuse-app/Cargo.toml`
- Создать: `crates/omnifuse-app/src/lib.rs`
- Создать: `crates/omnifuse-app/src/environment.rs`
- Создать: `crates/omnifuse-app/src/mount_layout.rs`
- Изменить: `Cargo.toml`

- [ ] **Шаг 1: Добавить crate в workspace**

Изменить корневой `Cargo.toml`:

```toml
members = [
  "crates/unifuse",
  "crates/omnifuse-core",
  "crates/omnifuse-git",
  "crates/omnifuse-wiki",
  "crates/omnifuse-app",
  "crates/omnifuse-cli",
  "crates/omnifuse-gui/tauri"
]
```

- [ ] **Шаг 2: Создать `omnifuse-app/Cargo.toml`**

```toml
[package]
name = "omnifuse-app"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
rust-version.workspace = true
description = "Application-level mount preparation for OmniFuse"

[lints]
workspace = true

[dependencies]
omnifuse-core = { path = "../omnifuse-core" }
omnifuse-git = { path = "../omnifuse-git" }
omnifuse-wiki = { path = "../omnifuse-wiki" }
anyhow = { workspace = true }
tokio = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Шаг 3: Написать failing tests для layout**

В `crates/omnifuse-app/src/mount_layout.rs` добавить tests:

```rust
#[test]
fn mount_layout_keeps_mount_point_and_work_dir_separate() {
  let env = FakeMountEnvironment::new()
    .home("/home/user")
    .canonical("/mnt/wiki", "/abs/mnt/wiki");

  let layout = MountLayout::resolve(
    &env,
    Path::new("/mnt/wiki"),
    CacheKey::new("wiki", "https://api.example.test/root")
  ).expect("layout");

  assert_eq!(layout.mount_point, PathBuf::from("/abs/mnt/wiki"));
  assert_ne!(layout.mount_point, layout.work_dir);
  assert!(layout.work_dir.ends_with("omnifuse/wiki"));
}

#[test]
fn same_source_and_mount_produce_stable_work_dir() {
  let env = FakeMountEnvironment::new()
    .home("/home/user")
    .canonical("/mnt/repo", "/abs/mnt/repo");

  let first = MountLayout::resolve(&env, Path::new("/mnt/repo"), CacheKey::new("git", "source-a"))
    .expect("first");
  let second = MountLayout::resolve(&env, Path::new("/mnt/repo"), CacheKey::new("git", "source-a"))
    .expect("second");

  assert_eq!(first.work_dir, second.work_dir);
}
```

Запуск:

```bash
cargo test -p omnifuse-app mount_layout
```

Ожидание: FAIL, потому что crate и типы ещё не реализованы.

- [ ] **Шаг 4: Реализовать `MountEnvironment` и `MountLayout`**

Ключевые типы:

```rust
pub trait MountEnvironment {
  fn home_dir(&self) -> anyhow::Result<PathBuf>;
  fn cache_base_dir(&self) -> anyhow::Result<PathBuf>;
  fn canonicalize_mount_point(&self, mount_point: &Path) -> anyhow::Result<PathBuf>;
}

#[derive(Debug, Clone)]
pub struct MountLayout {
  pub mount_point: PathBuf,
  pub work_dir: PathBuf
}

#[derive(Debug, Clone)]
pub struct CacheKey {
  backend: &'static str,
  identity: String
}
```

Правило cache key: `work_dir = cache_base/omnifuse/<backend>/<stable-hash-of-canonical-mount-and-source>`. Использовать `DefaultHasher`, как текущий CLI, чтобы не добавлять зависимость.

- [ ] **Шаг 5: Проверить layout tests**

Запуск:

```bash
cargo test -p omnifuse-app mount_layout
```

Ожидание: PASS.

- [ ] **Шаг 6: Коммит**

```bash
git add Cargo.toml crates/omnifuse-app
git commit -m "добавить crate для mount layout service"
```

## Задача 2: Реализовать `MountService`

**Files:**
- Создать: `crates/omnifuse-app/src/mount_service.rs`
- Изменить: `crates/omnifuse-app/src/lib.rs`

- [ ] **Шаг 1: Добавить tests на подготовку Git/Wiki**

```rust
#[test]
fn prepare_git_builds_consistent_mount_and_backend_config() {
  let service = MountService::new(FakeMountEnvironment::new()
    .home("/home/user")
    .canonical("/mnt/repo", "/abs/mnt/repo"));

  let prepared = service.prepare_git(GitMountArgs {
    source: "https://example.test/repo.git".to_string(),
    mount_point: PathBuf::from("/mnt/repo"),
    branch: Some("main".to_string()),
    poll_interval_secs: Some(15),
    allow_other: true,
    read_only: false
  }).expect("prepared");

  assert_eq!(prepared.config.mount_point, PathBuf::from("/abs/mnt/repo"));
  assert_eq!(prepared.config.local_dir, prepared.layout.work_dir);
  assert_eq!(prepared.config.mount_options.fs_name, "omnifuse-git");
  assert_eq!(prepared.config.mount_options.allow_other, true);
}

#[test]
fn prepare_wiki_uses_same_layout_rules_as_git() {
  let service = MountService::new(FakeMountEnvironment::new()
    .home("/home/user")
    .canonical("/mnt/wiki", "/abs/mnt/wiki"));

  let prepared = service.prepare_wiki(WikiMountArgs {
    base_url: "https://api.wiki.example.test".to_string(),
    root_slug: "root".to_string(),
    auth_token: "token".to_string(),
    org_id: Some("org".to_string()),
    mount_point: PathBuf::from("/mnt/wiki"),
    poll_interval_secs: Some(60),
    allow_other: false,
    read_only: false
  }).expect("prepared");

  assert_eq!(prepared.config.mount_point, PathBuf::from("/abs/mnt/wiki"));
  assert_eq!(prepared.config.local_dir, prepared.layout.work_dir);
  assert_eq!(prepared.config.mount_options.fs_name, "omnifuse-wiki");
}
```

Запуск:

```bash
cargo test -p omnifuse-app mount_service
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать service**

Ключевые правила:

- Git branch default: `"main"`.
- Git poll default: `30`.
- Wiki poll default: `60`.
- `MountConfig.sync`, `buffer`, `logging` используют текущие defaults.
- `GitConfig.local_dir` всегда равен `MountConfig.local_dir`.
- `WikiConfig` не знает `local_dir`; он передаётся в `Backend::init`.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-app
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-app
git commit -m "добавить общий mount service"
```

## Задача 3: Перевести CLI на `MountService`

**Files:**
- Изменить: `crates/omnifuse-cli/Cargo.toml`
- Изменить: `crates/omnifuse-cli/src/main.rs`
- Изменить: `crates/omnifuse-cli/tests/cli_tests.rs`

- [ ] **Шаг 1: Добавить зависимость `omnifuse-app`**

```toml
omnifuse-app = { path = "../omnifuse-app" }
```

- [ ] **Шаг 2: Удалить локальный `cache_dir_for`**

Заменить `cmd_mount_git` и `cmd_mount_wiki` на вызовы:

```rust
MountService::default()
  .run_git(GitMountArgs {
    source,
    mount_point: mountpoint,
    branch: Some(branch),
    poll_interval_secs: Some(poll_interval),
    allow_other,
    read_only
  }, omnifuse_core::NoopEventHandler)
  .await
  .context("mount error")?;
```

и:

```rust
MountService::default()
  .run_wiki(WikiMountArgs {
    base_url,
    root_slug,
    auth_token: auth,
    org_id,
    mount_point: mountpoint,
    poll_interval_secs: Some(poll_interval),
    allow_other,
    read_only
  }, omnifuse_core::NoopEventHandler)
  .await
  .context("mount error")?;
```

- [ ] **Шаг 3: Перенести tests cache dir**

Тесты `cache_dir_for_*` удалить из CLI и перенести смысл в `omnifuse-app` tests. CLI должен проверять только parsing/help/errors, не layout internals.

- [ ] **Шаг 4: Проверить CLI**

Запуск:

```bash
cargo test -p omnifuse-cli
cargo test -p omnifuse-app
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-cli crates/omnifuse-app
git commit -m "использовать общий mount service в cli"
```

## Задача 4: Перевести GUI/Tauri на `MountService`

**Files:**
- Изменить: `crates/omnifuse-gui/tauri/Cargo.toml`
- Изменить: `crates/omnifuse-gui/tauri/src/commands.rs`

- [ ] **Шаг 1: Добавить зависимость**

```toml
omnifuse-app = { path = "../../omnifuse-app" }
```

- [ ] **Шаг 2: Заменить ручную сборку config**

В `mount_git` собрать `GitMountArgs` и вызвать `MountService::default().run_git(args)`. В `mount_wiki` аналогично вызвать `run_wiki(args)`.

Критичный invariant: GUI больше не использует `mount_point` как `local_dir`; `local_dir` приходит из `MountService`.

- [ ] **Шаг 3: Проверить GUI crate**

Запуск:

```bash
cargo test -p omnifuse-gui
```

Ожидание: PASS или compile-only PASS, если тестов мало.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-gui/tauri crates/omnifuse-app
git commit -m "использовать общий mount service в gui"
```

## Задача 5: Убрать второй источник cache у Git remote

**Files:**
- Изменить: `crates/omnifuse-git/src/lib.rs`
- Изменить: `crates/omnifuse-git/src/repo_source.rs`
- Test: `crates/omnifuse-git/tests/backend_integration.rs`

- [ ] **Шаг 1: Добавить regression test**

Сценарий: `GitConfig.local_dir` задаёт work dir для remote source.

```rust
#[tokio::test]
async fn remote_git_init_uses_configured_local_dir() {
  let (_tmp, bare, _clone1, _clone2) = create_bare_and_two_clones().await;
  let work = tempfile::tempdir().expect("work dir");

  let backend = GitBackend::new(GitConfig {
    source: bare.to_string_lossy().into_owned(),
    branch: "main".to_string(),
    max_push_retries: 1,
    poll_interval_secs: 30,
    local_dir: work.path().join("configured")
  });

  backend.init(&work.path().join("configured")).await.expect("init");

  assert!(work.path().join("configured/.git").exists());
}
```

- [ ] **Шаг 2: Реализовать правило**

`RepoSource::Remote` может хранить URL без собственного `cache_path`; clone target приходит из `GitBackend::init(local_dir)`.

Если нужен запасной путь для прямого использования `RepoSource::ensure_available`, оставить отдельный метод `ensure_available_at(branch, target)`.

- [ ] **Шаг 3: Проверить Git**

Запуск:

```bash
cargo test -p omnifuse-git backend_integration
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-git
git commit -m "использовать подготовленный work dir для git remote source"
```

## Риски / Предположения

- Новый crate увеличивает workspace, но не добавляет внешних зависимостей.
- Если GUI должен хранить user-selected cache отдельно от mountpoint, это отдельный product decision; дефолтный plan делает cache системным.
- `RepoSource` может использоваться тестами напрямую. Менять его нужно с совместимым helper или ясным test update.

## Критерий завершения

- CLI и GUI используют один `MountService`.
- `GitConfig.local_dir == MountConfig.local_dir` для CLI/GUI.
- Remote Git не выбирает второй cache path внутри `RepoSource`.
- Тесты:

```bash
cargo test -p omnifuse-app
cargo test -p omnifuse-cli
cargo test -p omnifuse-gui
cargo test -p omnifuse-git backend_integration
```
