# План реализации: VFS file mutation pipeline

> **Для agentic workers:** этот план затрагивает горячий путь VFS. Выполняй после backend lifecycle refactors, чтобы не смешивать причины изменений.

**Цель:** собрать жизненный цикл buffer, записи на диск, dirty-события и file-события в один модуль, чтобы `OmniFuseVfs` стал тонким path-based адаптером.

**Архитектура:** добавить `FileMutationPipeline` и `OpenFileMutation`. Горячий путь `open/create -> write -> flush -> close` становится session-based; редкие structural operations (`unlink`, `rename`, `symlink`, `mkdir`, `rmdir`) проходят через маленький command gateway.

**Технологии:** `omnifuse-core`, `FileBufferManager`, `tokio::fs`, `mpsc::Sender<FsEvent>`, `VfsEventHandler`, `ObservabilitySession`.

---

## Файловая структура

- Создать: `crates/omnifuse-core/src/file_mutations.rs` — pipeline, open-file session, structural commands.
- Изменить: `crates/omnifuse-core/src/lib.rs` — экспортировать модуль.
- Изменить: `crates/omnifuse-core/src/vfs.rs` — делегировать методы мутаций.
- Изменить: `crates/omnifuse-core/src/buffer.rs` — оставить только ответственность buffer.
- Изменить: `crates/omnifuse-core/src/test_utils.rs` — helpers для тестов мутаций.
- Test: `crates/omnifuse-core/src/file_mutations.rs`.
- Test: `crates/omnifuse-core/src/vfs.rs`.
- Test: `crates/omnifuse-core/tests/e2e_pipeline_tests.rs`.
- Test: `crates/omnifuse-core/tests/concurrent_editing_tests.rs`.

## Целевой интерфейс

```rust
pub struct FileMutationPipeline {
  local_dir: PathBuf,
  buffer_manager: Arc<FileBufferManager>,
  dirty_sink: DirtySink,
  events: Arc<dyn VfsEventHandler>,
  session: Arc<ObservabilitySession>
}

pub struct OpenFileMutation {
  path: PathBuf,
  full_path: PathBuf,
  buffer: Arc<FileBuffer>,
  pipeline: Arc<FileMutationPipeline>,
  dirty_notified: AtomicBool
}

impl FileMutationPipeline {
  pub async fn open_file(self: &Arc<Self>, path: &Path) -> Result<OpenFileMutation, FsError>;
  pub async fn create_file(self: &Arc<Self>, path: &Path, mode: u32) -> Result<(OpenFileMutation, FileAttr), FsError>;
  pub async fn unlink(&self, path: &Path) -> Result<(), FsError>;
  pub async fn rename(&self, from: &Path, to: &Path) -> Result<(), FsError>;
  pub async fn symlink(&self, target: &Path, link: &Path) -> Result<FileAttr, FsError>;
}

impl OpenFileMutation {
  pub async fn read(&self, offset: u64, size: u32) -> Result<Vec<u8>, FsError>;
  pub async fn write(&self, offset: u64, data: &[u8]) -> Result<u32, FsError>;
  pub async fn truncate(&self, size: u64) -> Result<FileAttr, FsError>;
  pub async fn flush(&self) -> Result<(), FsError>;
  pub async fn close(self) -> Result<(), FsError>;
}
```

## Задача 1: Ввести `DirtySink`

**Files:**
- Создать: `crates/omnifuse-core/src/file_mutations.rs`
- Изменить: `crates/omnifuse-core/src/lib.rs`

- [ ] **Шаг 1: Написать tests для dirty sink**

```rust
#[tokio::test]
async fn dirty_sink_sends_modified_and_closed_events() {
  let (tx, mut rx) = tokio::sync::mpsc::channel(8);
  let sink = DirtySink::new(tx);

  sink.mark_modified(PathBuf::from("a.md"));
  sink.mark_closed(PathBuf::from("a.md"));

  assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("a.md")));
  assert!(matches!(rx.recv().await, Some(FsEvent::FileClosed(path)) if path == PathBuf::from("a.md")));
}

#[tokio::test]
async fn dirty_sink_reports_full_queue_without_panic() {
  let (tx, _rx) = tokio::sync::mpsc::channel(1);
  let sink = DirtySink::new(tx);

  sink.mark_modified(PathBuf::from("a.md"));
  let result = sink.mark_modified(PathBuf::from("b.md"));

  assert!(matches!(result, DirtySendResult::Dropped));
}
```

Запуск:

```bash
cargo test -p omnifuse-core file_mutations::dirty
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать `DirtySink`**

```rust
pub struct DirtySink {
  tx: mpsc::Sender<FsEvent>
}

pub enum DirtySendResult {
  Sent,
  Dropped
}
```

`DirtySink` не должен логировать сам; предупреждение о dropped event делает pipeline через `VfsEventHandler`.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core file_mutations::dirty
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core/src/file_mutations.rs crates/omnifuse-core/src/lib.rs
git commit -m "добавить vfs dirty sink"
```

## Задача 2: Реализовать open-file session

**Files:**
- Изменить: `crates/omnifuse-core/src/file_mutations.rs`

- [ ] **Шаг 1: Добавить tests write/flush/close**

```rust
#[tokio::test]
async fn open_file_mutation_writes_flushes_and_closes() {
  let tmp = tempfile::tempdir().expect("tmp");
  let path = tmp.path().join("doc.md");
  tokio::fs::write(&path, "").await.expect("seed");
  let (pipeline, mut rx, events) = test_pipeline(tmp.path());

  let file = pipeline.open_file(Path::new("doc.md")).await.expect("open");
  assert_eq!(file.write(0, b"hello").await.expect("write"), 5);
  file.flush().await.expect("flush");
  file.close().await.expect("close");

  assert_eq!(tokio::fs::read_to_string(&path).await.expect("read"), "hello");
  assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("doc.md")));
  assert!(matches!(rx.recv().await, Some(FsEvent::FileClosed(path)) if path == PathBuf::from("doc.md")));
  assert!(events.written_calls.lock().expect("lock").iter().any(|(path, bytes)| path == Path::new("doc.md") && *bytes == 5));
}

#[tokio::test]
async fn close_flushes_dirty_buffer_once() {
  let tmp = tempfile::tempdir().expect("tmp");
  tokio::fs::write(tmp.path().join("doc.md"), "").await.expect("seed");
  let (pipeline, _rx, _events) = test_pipeline(tmp.path());

  let file = pipeline.open_file(Path::new("doc.md")).await.expect("open");
  file.write(0, b"hello").await.expect("write");
  file.close().await.expect("close");

  assert_eq!(tokio::fs::read_to_string(tmp.path().join("doc.md")).await.expect("read"), "hello");
}
```

- [ ] **Шаг 2: Реализовать `FileMutationPipeline::open_file`**

Алгоритм:

1. `full_path = local_dir.join(path)`.
2. `buffer_manager.get_or_load(full_path)`.
3. Создать `OpenFileMutation`.

- [ ] **Шаг 3: Реализовать `OpenFileMutation::{read, write, flush, close}`**

Правила:

- `write` пишет в buffer, шлёт `FileModified`, emits `on_file_written` и `FileMarkedDirty`.
- `flush` пишет buffer на disk, emits `FileFlushed`.
- `close` вызывает `flush`, затем шлёт `FileClosed`.
- Если `DirtySink` queue full, emits `UserVisibleWarning`.

- [ ] **Шаг 4: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core file_mutations
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-core/src/file_mutations.rs
git commit -m "добавить vfs open file mutation session"
```

## Задача 3: Добавить structural operations

**Files:**
- Изменить: `crates/omnifuse-core/src/file_mutations.rs`

- [ ] **Шаг 1: Добавить tests**

```rust
#[tokio::test]
async fn create_file_caches_empty_buffer_and_marks_dirty() {
  let tmp = tempfile::tempdir().expect("tmp");
  let (pipeline, mut rx, events) = test_pipeline(tmp.path());

  let (_file, attr) = pipeline.create_file(Path::new("new.md"), 0o644).await.expect("create");

  assert_eq!(attr.kind, FileType::RegularFile);
  assert!(tmp.path().join("new.md").exists());
  assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("new.md")));
  assert!(events.created_calls.lock().expect("lock").iter().any(|path| path == Path::new("new.md")));
}

#[tokio::test]
async fn rename_removes_old_buffer_and_marks_both_paths_dirty() {
  let tmp = tempfile::tempdir().expect("tmp");
  tokio::fs::write(tmp.path().join("old.md"), "content").await.expect("seed");
  let (pipeline, mut rx, events) = test_pipeline(tmp.path());
  let _file = pipeline.open_file(Path::new("old.md")).await.expect("open");

  pipeline.rename(Path::new("old.md"), Path::new("new.md")).await.expect("rename");

  assert!(!tmp.path().join("old.md").exists());
  assert!(tmp.path().join("new.md").exists());
  assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("old.md")));
  assert!(matches!(rx.recv().await, Some(FsEvent::FileModified(path)) if path == PathBuf::from("new.md")));
  assert_eq!(events.renamed_calls.lock().expect("lock").len(), 1);
}
```

- [ ] **Шаг 2: Реализовать `create_file`, `unlink`, `rename`, `symlink`**

Сохранять текущую семантику `vfs.rs`:

- `create_file` создаёт пустой файл, mode на Unix, cache empty buffer, dirty event, created event.
- `unlink` removes buffer, removes file, dirty event, deleted event.
- `rename` disk rename, remove old buffer, dirty events for old and new, renamed event.
- `symlink` Unix only, dirty event for link path.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core file_mutations
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core/src/file_mutations.rs
git commit -m "добавить структурные операции vfs mutation"
```

## Задача 4: Перевести `OmniFuseVfs` на pipeline

**Files:**
- Изменить: `crates/omnifuse-core/src/vfs.rs`

- [ ] **Шаг 1: Добавить open file table**

В `OmniFuseVfs`:

```rust
mutation_pipeline: Arc<FileMutationPipeline>,
open_files: DashMap<FileHandle, OpenFileMutation>
```

`open` и `create` кладут session в `open_files`; `read/write/flush/release` достают по `fh`.

- [ ] **Шаг 2: Сохранить запасной путь по path**

Если `fh` не найден, для совместимости текущих тестов можно открыть временную session:

```rust
let file = match self.open_files.get(&fh) {
    Some(file) => file.clone(),
    None => self.mutation_pipeline.open_file(path).await?,
};
```

Если `OpenFileMutation` не должен быть `Clone`, хранить в `open_files` `Arc<OpenFileMutation>`.

После обновления тестов запасной путь можно удалить, если он скрывает баги.

- [ ] **Шаг 3: Делегировать methods**

`create`, `open`, `read`, `write`, `flush`, `release`, `unlink`, `rename`, `symlink`, `setattr(size)` должны использовать pipeline. `getattr`, `lookup`, `readdir`, `mkdir`, `rmdir`, xattr можно оставить в `vfs.rs`.

- [ ] **Шаг 4: Проверить VFS unit tests**

Запуск:

```bash
cargo test -p omnifuse-core vfs
```

Ожидание: PASS.

- [ ] **Шаг 5: Коммит**

```bash
git add crates/omnifuse-core/src/vfs.rs crates/omnifuse-core/src/file_mutations.rs
git commit -m "провести vfs file mutations через pipeline"
```

## Задача 5: Перевести boundary tests с internals на workflow

**Files:**
- Изменить: `crates/omnifuse-core/tests/e2e_pipeline_tests.rs`
- Изменить: `crates/omnifuse-core/tests/concurrent_editing_tests.rs`
- Изменить: `crates/omnifuse-core/src/buffer.rs`

- [ ] **Шаг 1: Оставить unit-тесты buffer только для семантики buffer**

Сохранять:

- read/write offset.
- truncate.
- LRU.
- stale reload.
- dirty buffer preserved.

Не проверять через buffer tests то, что теперь является VFS workflow: “write sends sync event”, “flush emits event”.

- [ ] **Шаг 2: Добавить workflow tests**

Проверять через `OmniFuseVfs`:

- create/write/flush/release -> disk content + sync event + user event.
- rename -> old/new dirty events + disk state.
- unlink -> buffer removed + disk state + dirty event.
- concurrent writes -> no data loss.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core e2e_pipeline_tests
cargo test -p omnifuse-core concurrent_editing_tests
cargo test -p omnifuse-core buffer
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core
git commit -m "проверить vfs mutations на границе workflow"
```

## Риски / Предположения

- Stateful `open_files` меняет lifecycle. Особенно проверить rename/unlink открытого файла.
- Не делать in-memory filesystem port: текущая local FS через tempdir уже local-substitutable.
- Если tests ожидают event на каждый `write`, сохранять это поведение до отдельного решения о coalescing.

## Критерий завершения

- `vfs.rs` больше не вызывает `FileBufferManager` напрямую в методах файловых мутаций.
- `FileMutationPipeline` владеет buffer/disk/dirty/event ordering.
- Тесты:

```bash
cargo test -p omnifuse-core vfs
cargo test -p omnifuse-core file_mutations
cargo test -p omnifuse-core e2e_pipeline_tests
cargo test -p omnifuse-core concurrent_editing_tests
```
