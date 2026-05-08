# План реализации: unifuse SessionPathFs

> **Для agentic workers:** это библиотечный API-рефактор. Делай его отдельной веткой после стабилизации core/backend refactors.

**Цель:** заменить несовместимый lifecycle `init/destroy(&mut self)` при `Arc<F>` на session-oriented path API, который лучше соответствует FUSE/WinFsp hot path.

**Архитектура:** добавить новый trait `SessionPathFs` рядом с текущим `UniFuseFilesystem`, затем постепенно перевести adapters. Старый trait оставить временно через compatibility adapter, чтобы не ломать весь workspace одним коммитом.

**Технологии:** `unifuse`, `rfuse3`, `winfsp`, `tokio`, existing `FileAttr`, `FileHandle`, `OpenFlags`, `FsError`.

---

## Файловая структура

- Создать: `crates/unifuse/src/session.rs` — `SessionPathFs`, `MountSpec`, `OpenedNode`, `DirPage`.
- Создать: `crates/unifuse/src/compat.rs` — adapter между старым `UniFuseFilesystem` и новым `SessionPathFs`.
- Изменить: `crates/unifuse/src/lib.rs` — экспорты и `run_mount`.
- Изменить: `crates/unifuse/src/rfuse3_adapter.rs` — adapter работает с `SessionPathFs`.
- Изменить: `crates/unifuse/src/winfsp_adapter.rs` — adapter работает с `SessionPathFs`.
- Изменить: `crates/unifuse/src/inode.rs` — добавить семантику subtree rename, если нужно.
- Изменить: `crates/omnifuse-core/src/vfs.rs` — реализовать `SessionPathFs` или использовать compat adapter.
- Test: `crates/unifuse/src/session.rs`.
- Test: `crates/unifuse/src/inode.rs`.
- Test: `crates/unifuse/src/rfuse3_adapter.rs` unit-level tests where possible.

## Целевой интерфейс

```rust
pub trait SessionPathFs: Send + Sync + 'static {
  type MountState: Send + Sync + 'static;

  fn start(&self, context: MountContext) -> impl Future<Output = Result<Self::MountState, FsError>> + Send;
  fn stop(&self, state: Self::MountState, reason: UnmountReason) -> impl Future<Output = Result<(), FsError>> + Send;

  fn lookup(&self, state: &Self::MountState, path: &Path) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;
  fn open(&self, state: &Self::MountState, path: &Path, intent: OpenIntent) -> impl Future<Output = Result<OpenedNode, FsError>> + Send;
  fn read_into(&self, file: &OpenedNode, offset: u64, out: &mut [u8]) -> impl Future<Output = Result<usize, FsError>> + Send;
  fn write_from(&self, file: &OpenedNode, offset: u64, data: &[u8]) -> impl Future<Output = Result<usize, FsError>> + Send;
  fn flush(&self, file: &OpenedNode, mode: FlushMode) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;
  fn close(&self, file: OpenedNode, reason: CloseReason) -> impl Future<Output = Result<(), FsError>> + Send;
  fn read_dir(&self, state: &Self::MountState, path: &Path, page: DirPageRequest) -> impl Future<Output = Result<DirPage, FsError>> + Send;
  fn mutate(&self, state: &Self::MountState, mutation: FsMutation<'_>) -> impl Future<Output = Result<NodeMeta, FsError>> + Send;
  fn statfs(&self, state: &Self::MountState) -> impl Future<Output = Result<StatFs, FsError>> + Send;
}
```

## Задача 1: Добавить session types без смены adapters

**Files:**
- Создать: `crates/unifuse/src/session.rs`
- Изменить: `crates/unifuse/src/lib.rs`

- [ ] **Шаг 1: Написать compile/unit tests**

```rust
#[tokio::test]
async fn recording_session_fs_start_and_stop_are_called_on_shared_ref() {
  let fs = RecordingSessionFs::default();
  let state = fs.start(MountContext::test("/mnt/test")).await.expect("start");

  assert_eq!(fs.started_count(), 1);

  fs.stop(state, UnmountReason::Unmounted).await.expect("stop");

  assert_eq!(fs.stopped_count(), 1);
}

#[test]
fn opened_node_carries_path_handle_kind_and_attr() {
  let attr = FileAttr::regular_file(5);
  let node = OpenedNode::new(PathBuf::from("doc.md"), FileHandle(7), FileType::RegularFile, attr.clone());

  assert_eq!(node.path.as_ref(), Path::new("doc.md"));
  assert_eq!(node.handle, FileHandle(7));
  assert_eq!(node.attr.size, 5);
}
```

Запуск:

```bash
cargo test -p unifuse session
```

Ожидание: FAIL.

- [ ] **Шаг 2: Реализовать types**

Минимальный набор:

- `MountSpec`.
- `MountContext`.
- `UnmountReason`.
- `NodeMeta`.
- `OpenedNode`.
- `OpenIntent`.
- `FlushMode`.
- `CloseReason`.
- `DirPageRequest`.
- `DirPage`.
- `DirEntryMeta`.
- `FsMutation`.

Не добавлять generic driver pipeline.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p unifuse session
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/unifuse/src/session.rs crates/unifuse/src/lib.rs
git commit -m "добавить api session path filesystem"
```

## Задача 2: Добавить compatibility adapter

**Files:**
- Создать: `crates/unifuse/src/compat.rs`
- Изменить: `crates/unifuse/src/lib.rs`

- [ ] **Шаг 1: Написать tests**

```rust
#[tokio::test]
async fn compat_session_delegates_open_read_close_to_old_trait() {
  let old = RecordingOldFs::with_file("doc.md", b"hello");
  let compat = CompatSessionFs::new(old);
  let state = compat.start(MountContext::test("/mnt/test")).await.expect("start");

  let opened = compat.open(&state, Path::new("doc.md"), OpenIntent::read_only()).await.expect("open");
  let mut out = vec![0; 5];
  let read = compat.read_into(&opened, 0, &mut out).await.expect("read");
  compat.close(opened, CloseReason::Released).await.expect("close");

  assert_eq!(read, 5);
  assert_eq!(&out, b"hello");
}
```

- [ ] **Шаг 2: Реализовать `CompatSessionFs<F>`**

`CompatSessionFs` wraps старый `UniFuseFilesystem`:

- `start` вызывает no-op, потому что старый `init(&mut self)` несовместим с `Arc`.
- `lookup(path)` может использовать `getattr(path)` для full path.
- `open` вызывает old `open`, затем `getattr`.
- `read_into` вызывает старый `read` и копирует байты в буфер вызывающего кода.
- `write_from` вызывает old `write`.
- `close` вызывает old `release`.
- `mutate` dispatches to old `create/mkdir/unlink/rename/symlink`.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p unifuse compat
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/unifuse/src/compat.rs crates/unifuse/src/lib.rs
git commit -m "добавить compatibility adapter для session fs"
```

## Задача 3: Перевести rfuse3 adapter на `SessionPathFs`

**Files:**
- Изменить: `crates/unifuse/src/rfuse3_adapter.rs`
- Изменить: `crates/unifuse/src/lib.rs`
- Изменить: `crates/unifuse/src/inode.rs`

- [ ] **Шаг 1: Добавить unit tests для inode lifecycle**

```rust
#[test]
fn inode_map_renames_subtree() {
  let map = InodeMap::new(PathBuf::from("/"));
  let file = PathBuf::from("/old/a.md");
  let inode = map.get_or_insert(&file, NodeKind::File);

  map.rename_subtree(Path::new("/old"), Path::new("/new"));

  assert_eq!(map.get_path(inode), Some(PathBuf::from("/new/a.md")));
  assert_eq!(map.get_inode(Path::new("/new/a.md")), Some(inode));
  assert_eq!(map.get_inode(Path::new("/old/a.md")), None);
}
```

- [ ] **Шаг 2: Добавить adapter-level fake tests where possible**

Не пытаться mount'ить FUSE в unit test. Проверить conversion/helpers:

- open flags decode.
- attr conversion.
- helper-методы для publish/remove/rename inode на `lookup/create/unlink/rename`.

- [ ] **Шаг 3: Изменить `Rfuse3Adapter<F>` bound**

```rust
pub struct Rfuse3Adapter<F: SessionPathFs> {
  inner: Arc<F>,
  state: Arc<F::MountState>,
  inode_map: Arc<RwLock<InodeMap>>,
  open_nodes: DashMap<u64, OpenedNode>
}
```

`init` platform callback больше не вызывает app lifecycle; lifecycle вызывается host перед mount.

- [ ] **Шаг 4: Обновить callbacks**

Mapping:

- `lookup(parent, name)` -> path -> `inner.lookup(state, path)` -> publish inode.
- `open` -> `inner.open` -> store `OpenedNode` by fh.
- `read` -> `inner.read_into`.
- `write` -> `inner.write_from`.
- `flush` -> `inner.flush`.
- `release` -> remove opened node -> `inner.close`.
- `readdir` -> `inner.read_dir`.
- `rename` -> `inner.mutate(FsMutation::Rename)` -> `inode_map.rename_subtree`.

- [ ] **Шаг 5: Проверить Unix build**

Запуск:

```bash
cargo test -p unifuse
```

Ожидание: PASS on Unix.

- [ ] **Шаг 6: Коммит**

```bash
git add crates/unifuse/src/rfuse3_adapter.rs crates/unifuse/src/inode.rs crates/unifuse/src/lib.rs
git commit -m "провести rfuse3 adapter через session fs"
```

## Задача 4: Перевести mount host API

**Files:**
- Изменить: `crates/unifuse/src/lib.rs`

- [ ] **Шаг 1: Добавить `run_mount`**

```rust
pub async fn run_mount<F>(fs: F, spec: MountSpec) -> Result<MountExit, FsError>
where
  F: SessionPathFs;
```

`run_mount`:

1. Создаёт `MountContext`.
2. Вызывает `fs.start(context).await`.
3. Создаёт platform adapter со state.
4. Mounts and waits.
5. На выходе вызывает `fs.stop(state, reason).await`.

- [ ] **Шаг 2: Сохранить old `UniFuseHost` через compat**

`UniFuseHost<F: UniFuseFilesystem>` может внутри вызывать `run_mount(CompatSessionFs::new(fs), spec)`.

- [ ] **Шаг 3: Проверить core compile**

Запуск:

```bash
cargo test -p unifuse
cargo test -p omnifuse-core --lib
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/unifuse/src/lib.rs crates/unifuse/src/compat.rs
git commit -m "добавить session-based unifuse mount host"
```

## Задача 5: Реализовать `SessionPathFs` для `OmniFuseVfs`

**Files:**
- Изменить: `crates/omnifuse-core/src/vfs.rs`
- Изменить: `crates/omnifuse-core/src/lib.rs`

- [ ] **Шаг 1: Добавить compile-focused tests**

```rust
#[tokio::test]
async fn omnifuse_vfs_session_api_open_write_close_roundtrip() {
  let tmp = tempfile::tempdir().expect("tmp");
  let (vfs, _rx) = create_test_vfs(tmp.path());
  let state = vfs.start(MountContext::test(tmp.path())).await.expect("start");

  let meta = vfs.mutate(&state, FsMutation::Create {
    path: Path::new("doc.md"),
    flags: OpenFlags::read_write(),
    mode: 0o644
  }).await.expect("create");
  assert_eq!(meta.attr.kind, FileType::RegularFile);

  let opened = vfs.open(&state, Path::new("doc.md"), OpenIntent::read_write()).await.expect("open");
  vfs.write_from(&opened, 0, b"hello").await.expect("write");
  vfs.close(opened, CloseReason::Released).await.expect("close");
  vfs.stop(state, UnmountReason::Unmounted).await.expect("stop");

  assert_eq!(std::fs::read_to_string(tmp.path().join("doc.md")).expect("read"), "hello");
}
```

- [ ] **Шаг 2: Реализовать**

Сопоставить существующие методы VFS:

- `start` -> `Ok(())`.
- `stop` -> flush if needed only if VFS owns mutation pipeline; otherwise no-op.
- `lookup` -> `getattr`.
- `open` -> old `open` + `getattr` -> `OpenedNode`.
- `read_into` -> old `read`.
- `write_from` -> old `write`.
- `flush` -> old `flush` + `getattr`.
- `close` -> old `release`.
- `read_dir` -> old `readdir`.
- `mutate` -> old create/mkdir/unlink/rename/symlink.

- [ ] **Шаг 3: Проверить**

Запуск:

```bash
cargo test -p omnifuse-core vfs
cargo test -p unifuse
```

Ожидание: PASS.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/omnifuse-core/src/vfs.rs crates/omnifuse-core/src/lib.rs
git commit -m "реализовать session path fs для omnifuse vfs"
```

## Задача 6: Перевести WinFsp adapter

**Files:**
- Изменить: `crates/unifuse/src/winfsp_adapter.rs`

- [ ] **Шаг 1: Обновить context**

`WinfspFileContext` должен хранить `OpenedNode`, а не `path + FileHandle` отдельно:

```rust
pub struct WinfspFileContext {
  opened: Option<OpenedNode>,
  path: PathBuf,
  is_directory: bool,
  delete_on_close: bool
}
```

- [ ] **Шаг 2: Сопоставить callbacks**

- `open/create` -> `SessionPathFs::open/mutate`.
- `read/write` -> `read_into/write_from`.
- `flush/close` -> `flush/close`.
- `cleanup` delete-on-close -> `mutate(Delete)`.

- [ ] **Шаг 3: Проверить compile где возможно**

На Unix Windows module может не компилироваться. Проверка:

```bash
cargo test -p unifuse
```

Ожидание: Unix tests PASS. Windows compile проверить в CI или отдельной Windows среде.

- [ ] **Шаг 4: Коммит**

```bash
git add crates/unifuse/src/winfsp_adapter.rs
git commit -m "провести winfsp adapter через session fs"
```

## Риски / Предположения

- Это API-level refactor; делать только после зелёного core/backend состояния.
- `read_into` меняет allocation profile. Проверить short-read и EOF cases.
- Windows adapter нельзя полноценно проверить на macOS/Linux; сохранять compile-gated осторожность.
- Не вводить generic driver/codec pipeline, пока нет второго platform driver.

## Критерий завершения

- Новый `SessionPathFs` работает через `&self` lifecycle.
- `run_mount` вызывает `start/stop` ровно один раз.
- rfuse3 adapter не вызывает app lifecycle из raw callback.
- Старый `UniFuseFilesystem` ещё поддерживается через compat adapter.
- Тесты:

```bash
cargo test -p unifuse
cargo test -p omnifuse-core --lib
```
