# OmniFuse: Архитектура (интегрированный дизайн)

## Видение проекта

**OmniFuse** — универсальное приложение для монтирования различных сетевых/доменных ресурсов (git-репозитории, wiki-кластеры, облачные хранилища) в качестве виртуальной файловой системы с возможностью редактирования как обычных файлов и автоматической синхронизацией изменений обратно в источник.

### Ключевые принципы (из опыта SimpleGitFS + YaWikiFS)

1. **Local directory как единая модель данных** — для всех backend'ов файлы хранятся на диске как обычные файлы. Git backend → git working tree. Wiki backend → `.md` файлы в локальной папке. Это упрощает FUSE-слой (просто проксирует fs-операции) и позволяет работать offline.

2. **Platform-agnostic VFS ядро** — вся бизнес-логика (буферы, sync, события) в `omnifuse-core` без зависимости от FUSE или WinFsp. Platform adapters — тонкие обёртки.

3. **Платформы**: Linux (FUSE), macOS (macFUSE), Windows (WinFsp).

4. **Один trait `Backend`** — для git и wiki (пока этого достаточно). Backend управляет sync самостоятельно (commit/push, API PUT), merge встроен в backend. Когда появятся S3/WebDAV — trait при необходимости обобщится.

5. **Merge strategy — ответственность backend'а**: git использует native git merge, wiki использует `diffy` 3-way merge. Core не навязывает стратегию.

6. **Sync Engine оркестрирует timing** — debounce + close trigger + periodic poll. НЕ занимается merge.

7. **Async-first архитектура** — rfuse3 (async, tokio-native), async trait `UniFuseFilesystem`, async `Backend`. Никаких blocking thread pools.

### Ключевые отличия от rclone

- **Доменная семантика**: понимание git-коммитов, wiki-страниц, версионирования — не только как blob-объектов
- **Bidirectional sync с conflict resolution**: умная синхронизация для текстовых/структурированных данных
- **Кроссплатформенность**: FUSE (Linux/macOS) + WinFsp (Windows) через единый async VfsHandler
- **GUI**: desktop-приложение на Tauri + React

### Принятые решения

| Вопрос | Решение |
|--------|---------|
| FUSE crate | **rfuse3** — async-native, tokio, уже работает в YaWikiFS на macOS |
| Backend trait | **Один `Backend` trait** — без отдельного ObjectBackend. Git + Wiki достаточно. S3/WebDAV — future |
| Sync/Async | **Async everywhere** — `UniFuseFilesystem` async, rfuse3 async, Backend async |
| CLI binary | **`of`** — коротко и удобно |
| unifuse crate | **Монорепозиторий**, публикуется на crates.io отдельно |

---

## Архитектура: Workspace структура

```
omnifuse/
├── Cargo.toml                    # [workspace] root
├── CLAUDE.md
├── rustfmt.toml
│
├── crates/
│   ├── unifuse/                  # Слой 1: Кроссплатформенная async FUSE-абстракция
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # UniFuseFilesystem async trait, UniFuseHost mount API
│   │       ├── types.rs          # FileAttr, DirEntry, FileHandle, OpenFlags, FsError
│   │       ├── rfuse3_adapter.rs # Rfuse3Adapter: inode-based rfuse3 → path-based async trait
│   │       ├── inode.rs          # InodeMap (из SimpleGitFS, FNV-1a hash)
│   │       ├── winfsp_adapter.rs # WinfspAdapter: FileSystemContext → path-based async trait
│   │       └── security.rs       # Security descriptors для Windows
│   │
│   ├── omnifuse-core/            # Слой 2: VFS ядро (platform-agnostic)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # Реэкспорт + run_mount()
│   │       ├── vfs_handler.rs    # OmniFuseVfs: async path-based бизнес-логика
│   │       ├── backend.rs        # Backend trait (git, wiki)
│   │       ├── backend_registry.rs # Plugin registry через inventory
│   │       ├── sync_engine.rs    # DirtyTracker + SyncEngine (debounce + close trigger)
│   │       ├── buffer.rs         # FileBuffer + FileBufferManager (из SimpleGitFS)
│   │       ├── conflict_log.rs   # ConflictEntry, log_conflict() (из YaWikiFS)
│   │       ├── offline_queue.rs  # SyncQueueManager (из SimpleGitFS)
│   │       ├── pacer.rs          # Rate limiting (из rclone)
│   │       ├── accounting.rs     # Статистика операций
│   │       ├── events.rs         # VfsEventHandler trait (из SimpleGitFS)
│   │       ├── config.rs         # Конфигурация
│   │       └── error.rs          # OmniFuseError enum
│   │
│   ├── omnifuse-git/             # Слой 3: Git backend
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # impl Backend for GitBackend
│   │       ├── engine.rs         # GitEngine (из SimpleGitFS, gix)
│   │       ├── ops.rs            # GitOps (commit, push_with_retry)
│   │       ├── merge.rs          # MergeEngine (git native merge)
│   │       ├── repo_source.rs    # RepoSource (local/remote)
│   │       └── filter.rs         # .gitignore filtering
│   │
│   ├── omnifuse-wiki/            # Слой 3: Wiki backend
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # impl Backend for WikiBackend
│   │       ├── client.rs         # Wiki HTTP API (из YaWikiFS, reqwest)
│   │       ├── models.rs         # Serde модели API
│   │       ├── tree.rs           # Fetch/sync дерева страниц
│   │       ├── meta.rs           # .vfs/meta/ management
│   │       └── merge.rs          # diffy three_way_merge (из YaWikiFS)
│   │
│   ├── omnifuse-cli/             # Слой 4: CLI (binary: `of`)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs           # of mount/unmount/status/check
│   │
│   └── omnifuse-gui/             # Слой 5: GUI (Tauri + React)
│       ├── src-tauri/
│       │   ├── Cargo.toml
│       │   └── src/
│       │       ├── main.rs
│       │       ├── commands.rs
│       │       └── events.rs     # TauriEventHandler
│       ├── src/                  # React frontend
│       └── ...
│
├── crates/omnifuse-test/         # Generic backend compliance tests
│   └── src/lib.rs
│
├── docs/                         # Документация
├── tests/                        # Интеграционные тесты
│   ├── common/
│   │   ├── fake_wiki_api.rs      # Axum mock server (из YaWikiFS)
│   │   └── git_helpers.rs        # create_bare_and_two_clones()
│   ├── git_backend_tests.rs
│   ├── wiki_backend_tests.rs
│   ├── sync_engine_tests.rs
│   ├── fuse_tests.rs
│   └── concurrent_editing_tests.rs
│
└── ui/web/                       # Web UI (future)
```

---

## Зависимости workspace

### Cargo.toml (корневой)

```toml
[workspace]
resolver = "2"
members = [
  "crates/unifuse",
  "crates/omnifuse-core",
  "crates/omnifuse-git",
  "crates/omnifuse-wiki",
  "crates/omnifuse-cli",
  "crates/omnifuse-gui/src-tauri",
  "crates/omnifuse-test",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"

[workspace.dependencies]
# Общие
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "2"
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dashmap = "6"
chrono = "0.4"
clap = { version = "4", features = ["derive"] }
libc = "0.2"
tempfile = "3"
inventory = "0.3"

# FUSE (Linux/macOS) — async, tokio-native
rfuse3 = { version = "0.0.5", features = ["tokio-runtime"] }

# WinFsp (Windows)
winfsp = "0.12"

# Merge
diffy = "0.4"

# Git
gix = { version = "0.72", default-features = false, features = [
  "blocking-network-client",
  "worktree-mutation"
] }
ignore = "0.4"

# Wiki/HTTP
reqwest = { version = "0.12", features = ["json"] }
url = "2"

# Dev
axum = "0.7"
tower = "0.5"
http = "1"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
correctness = { level = "deny", priority = -1 }
suspicious = { level = "warn", priority = -1 }
perf = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
panic = "deny"
unwrap_used = "deny"
expect_used = "deny"
```

---

## Слой 1: unifuse — Кроссплатформенная async FUSE-абстракция

### Назначение

Тонкий адаптационный слой (~1700-2600 строк) поверх существующих Rust-крейтов:

- Linux/macOS: **rfuse3** — async, tokio-native, работает напрямую с `/dev/fuse` без libfuse. Уже используется в YaWikiFS на macOS.
- Windows: **winfsp-rs** v0.12 — Native API (не FUSE compatibility layer)

**Ключевая идея из cgofuse-to-unifuse-porting.md**: CGO-зависимость полностью устраняется. В Go нужен C-трамплин для каждого FUSE callback'а (826 строк C), в Rust — нет, потому что rfuse3/winfsp-rs уже реализуют dispatch внутри себя.

**Публикация**: крейт `unifuse` публикуется на crates.io отдельно (аналог cgofuse для Rust), но исходники живут в монорепозитории omnifuse.

### types.rs — Кроссплатформенные типы

```rust
use std::path::PathBuf;
use std::time::SystemTime;

/// Тип файлового объекта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    RegularFile,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    NamedPipe,
    Socket,
}

/// Атрибуты файла (кроссплатформенные).
///
/// Общее подмножество POSIX stat и Windows FILE_INFO.
/// Platform adapters маппят из/в платформо-специфичные типы.
#[derive(Debug, Clone)]
pub struct FileAttr {
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,     // macOS/Windows creation time
    pub kind: FileType,
    pub perm: u16,              // Unix mode (0o644, 0o755). На Windows → readonly mapping
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub flags: u32,             // BSD flags (macOS) / FILE_ATTRIBUTE_* (Windows)
}

/// Хэндл открытого файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

/// Элемент директории.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileType,
}

/// Флаги открытия файла.
#[derive(Debug, Clone, Copy)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    pub create: bool,
    pub exclusive: bool,
}

/// Информация о файловой системе.
pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
}

/// Ошибки FUSE-операций.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
    #[error("already exists")]
    AlreadyExists,
    #[error("not a directory")]
    NotADirectory,
    #[error("is a directory")]
    IsADirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("not supported")]
    NotSupported,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl FsError {
    /// Преобразование в libc errno для FUSE.
    pub fn to_errno(&self) -> i32 {
        match self {
            Self::NotFound => libc::ENOENT,
            Self::PermissionDenied => libc::EACCES,
            Self::AlreadyExists => libc::EEXIST,
            Self::NotADirectory => libc::ENOTDIR,
            Self::IsADirectory => libc::EISDIR,
            Self::NotEmpty => libc::ENOTEMPTY,
            Self::NotSupported => libc::ENOSYS,
            Self::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            Self::Other(_) => libc::EIO,
        }
    }

    /// Преобразование в NTSTATUS для WinFsp.
    #[cfg(windows)]
    pub fn to_ntstatus(&self) -> i32 { /* ... */ }
}
```

### UniFuseFilesystem — async path-based trait

```rust
/// Кроссплатформенный async path-based filesystem trait.
///
/// Аналог cgofuse FileSystemInterface (42 метода),
/// но с Rust-идиоматичным async API: Result<T>, &Path, Vec<u8>.
///
/// Async-first: rfuse3 вызывает эти методы из tokio runtime.
/// WinFsp adapter использует tokio::runtime::Handle::block_on()
/// для вызова async методов из синхронного контекста.
///
/// Реализатор trait'а — omnifuse-core (OmniFuseVfs).
pub trait UniFuseFilesystem: Send + Sync + 'static {
    // --- Lifecycle ---
    async fn init(&mut self) -> Result<(), FsError> { Ok(()) }
    fn destroy(&mut self) {}

    // --- Метаданные ---
    async fn getattr(&self, path: &Path) -> Result<FileAttr, FsError>;
    async fn setattr(&self, path: &Path, size: Option<u64>, atime: Option<SystemTime>,
                     mtime: Option<SystemTime>, mode: Option<u32>)
        -> Result<FileAttr, FsError> {
        Err(FsError::NotSupported)
    }
    async fn lookup(&self, parent: &Path, name: &OsStr) -> Result<FileAttr, FsError>;

    // --- Файловые операции ---
    async fn open(&self, path: &Path, flags: OpenFlags) -> Result<FileHandle, FsError>;
    async fn create(&self, path: &Path, flags: OpenFlags, mode: u32)
        -> Result<(FileHandle, FileAttr), FsError>;
    async fn read(&self, path: &Path, fh: FileHandle, offset: u64, size: u32)
        -> Result<Vec<u8>, FsError>;
    async fn write(&self, path: &Path, fh: FileHandle, offset: u64, data: &[u8])
        -> Result<u32, FsError>;
    async fn flush(&self, path: &Path, fh: FileHandle) -> Result<(), FsError> { Ok(()) }
    async fn release(&self, path: &Path, fh: FileHandle) -> Result<(), FsError>;
    async fn fsync(&self, path: &Path, fh: FileHandle, datasync: bool)
        -> Result<(), FsError> { Ok(()) }

    // --- Директории ---
    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;
    async fn mkdir(&self, path: &Path, mode: u32) -> Result<FileAttr, FsError>;
    async fn rmdir(&self, path: &Path) -> Result<(), FsError>;

    // --- Операции над деревом ---
    async fn unlink(&self, path: &Path) -> Result<(), FsError>;
    async fn rename(&self, from: &Path, to: &Path, flags: u32) -> Result<(), FsError>;

    // --- Symbolic links ---
    async fn symlink(&self, target: &Path, link: &Path) -> Result<FileAttr, FsError> {
        Err(FsError::NotSupported)
    }
    async fn readlink(&self, path: &Path) -> Result<PathBuf, FsError> {
        Err(FsError::NotSupported)
    }

    // --- Extended attributes (опционально) ---
    async fn setxattr(&self, path: &Path, name: &OsStr, value: &[u8], flags: i32)
        -> Result<(), FsError> { Err(FsError::NotSupported) }
    async fn getxattr(&self, path: &Path, name: &OsStr)
        -> Result<Vec<u8>, FsError> { Err(FsError::NotSupported) }
    async fn listxattr(&self, path: &Path)
        -> Result<Vec<OsString>, FsError> { Err(FsError::NotSupported) }
    async fn removexattr(&self, path: &Path, name: &OsStr)
        -> Result<(), FsError> { Err(FsError::NotSupported) }

    // --- Filesystem info ---
    async fn statfs(&self, path: &Path) -> Result<StatFs, FsError>;
}
```

### Адаптеры

#### rfuse3_adapter.rs (Linux/macOS) — ~500-800 строк

```rust
/// Адаптер: преобразует inode-based вызовы rfuse3 → path-based async UniFuseFilesystem.
///
/// Содержит InodeMap для маппинга inode ↔ path.
/// Маппинг типов:
///   rfuse3::Inode (u64) → Path (через InodeMap)
///   rfuse3::FileAttr → unifuse::FileAttr
///   unifuse::FsError → Errno
pub struct Rfuse3Adapter<F: UniFuseFilesystem> {
    inner: Arc<F>,
    inode_map: Arc<RwLock<InodeMap>>,
}

impl<F: UniFuseFilesystem> rfuse3::Filesystem for Rfuse3Adapter<F> {
    async fn lookup(&self, _req: Request, parent: u64, name: &OsStr)
        -> rfuse3::Result<ReplyEntry> {
        let table = self.inode_map.write().await;
        let parent_path = table.resolve(parent);

        match self.inner.lookup(&parent_path, name).await {
            Ok(attr) => {
                let path = parent_path.join(name);
                let ino = table.get_or_insert(&path);
                Ok(ReplyEntry {
                    ttl: TTL,
                    attr: convert_attr(attr, ino),
                    generation: 0,
                })
            }
            Err(e) => Err(e.to_errno().into()),
        }
    }

    async fn read(&self, _req: Request, ino: u64, fh: u64, offset: u64, size: u32)
        -> rfuse3::Result<ReplyData> {
        let table = self.inode_map.read().await;
        let path = table.resolve(ino);

        match self.inner.read(&path, FileHandle(fh), offset, size).await {
            Ok(data) => Ok(ReplyData { data: data.into() }),
            Err(e) => Err(e.to_errno().into()),
        }
    }

    // ... остальные ~48 методов — async inode→path маппинг
}
```

**Ключевое преимущество rfuse3**: adapter методы **async** — не нужен blocking thread pool. Весь pipeline от FUSE kernel до backend'а async.

#### inode.rs — InodeMap (из SimpleGitFS)

Из SimpleGitFS `core/src/util/inode_map.rs` (277 строк) **без изменений**.
FNV-1a хеш, `NodeKind`, `ROOT_INODE = 1`, bidirectional HashMap, reference counting.

WinFsp работает с путями напрямую — InodeMap ему **НЕ нужен**.

#### winfsp_adapter.rs (Windows) — ~400-600 строк

```rust
/// Адаптер WinFsp → async UniFuseFilesystem.
///
/// WinFsp API синхронный, поэтому используем
/// `tokio::runtime::Handle::block_on()` для вызова async методов.
///
/// Маппинг типов:
///   U16CStr (NT-path) → Path (UTF-16 → UTF-8)
///   unifuse::FileAttr → winfsp FileInfo (FILE_ATTRIBUTE_*, FILETIME)
///   unifuse::FsError → NTSTATUS
pub struct WinfspAdapter<F: UniFuseFilesystem> {
    inner: Arc<F>,
    rt: tokio::runtime::Handle,
}

pub struct WinfspFileContext {
    handle: FileHandle,
    path: PathBuf,
    is_directory: bool,
}

impl<F: UniFuseFilesystem> winfsp::filesystem::FileSystemContext for WinfspAdapter<F> {
    type FileContext = WinfspFileContext;

    fn open(&self, file_name: &U16CStr, create_options: u32,
            granted_access: FILE_ACCESS_RIGHTS, file_info: &mut OpenFileInfo)
        -> winfsp::Result<Self::FileContext> {
        let path = nt_path_to_path(file_name);
        let flags = convert_create_options(create_options, granted_access);
        // Sync bridge: block_on async method
        let fh = self.rt.block_on(self.inner.open(&path, flags))
            .map_err(ntstatus_from_error)?;
        Ok(WinfspFileContext { handle: fh, path, is_directory: false })
    }

    fn read(&self, context: &Self::FileContext, buffer: &mut [u8], offset: u64)
        -> winfsp::Result<u32> {
        let data = self.rt.block_on(
            self.inner.read(&context.path, context.handle, offset, buffer.len() as u32)
        ).map_err(ntstatus_from_error)?;
        buffer[..data.len()].copy_from_slice(&data);
        Ok(data.len() as u32)
    }

    fn close(&self, context: Self::FileContext) {
        let _ = self.rt.block_on(self.inner.release(&context.path, context.handle));
    }

    // ... остальные ~30 методов
}
```

**Различия FUSE vs WinFsp:**

| Аспект | FUSE (Linux/macOS) | WinFsp (Windows) |
|--------|-------------------|------------------|
| Идентификация | inode (u64) | NT-path (UTF-16) |
| Async | rfuse3 native async | sync (block_on bridge) |
| Права доступа | POSIX mode (0o644) | Security descriptors + ACL |
| Симлинки | Полная поддержка | Reparse points (нужны привилегии) |
| Extended attrs | xattr | Named streams / EA |
| Case sensitivity | Зависит от FS | case-insensitive по умолчанию |
| Удаление | `unlink()` сразу | `cleanup()` с DELETE_ON_CLOSE |

### Mount API — UniFuseHost

```rust
pub struct UniFuseHost<F: UniFuseFilesystem> {
    fs: Arc<F>,
}

impl<F: UniFuseFilesystem> UniFuseHost<F> {
    pub fn new(fs: F) -> Self;

    /// Смонтировать и заблокировать до unmount.
    pub async fn mount(&self, mountpoint: &Path, options: MountOptions) -> Result<()> {
        #[cfg(unix)]
        {
            let adapter = Rfuse3Adapter::new(self.fs.clone());
            let mount_handle = rfuse3::MountHandle::mount(
                rfuse3::MountOptions::default()
                    .fs_name(&options.fs_name)
                    .allow_other(options.allow_other),
                mountpoint,
                adapter,
            ).await?;
            mount_handle.await?; // Блокирует до unmount
        }

        #[cfg(windows)]
        {
            let adapter = WinfspAdapter::new(self.fs.clone(), tokio::runtime::Handle::current());
            let mut host = winfsp::host::FileSystemHost::new(volume_params, adapter)?;
            host.mount(mountpoint)?;
            host.start()?;
            tokio::signal::ctrl_c().await?; // Ждём Ctrl+C
            host.stop();
        }

        Ok(())
    }

    /// Проверить доступность платформы.
    pub fn is_available() -> bool {
        #[cfg(target_os = "linux")]
        { Path::new("/dev/fuse").exists() }
        #[cfg(target_os = "macos")]
        { Path::new("/Library/Filesystems/macfuse.fs").exists() }
        #[cfg(windows)]
        { winfsp::winfsp_init().is_ok() }
    }
}
```

### Feature flags (unifuse/Cargo.toml)

```toml
[package]
name = "unifuse"
version.workspace = true
edition.workspace = true

[features]
default = []

[target.'cfg(unix)'.dependencies]
rfuse3 = { workspace = true }
libc = { workspace = true }

[target.'cfg(windows)'.dependencies]
winfsp = { workspace = true }
windows = { version = "0.58", features = [
  "Win32_Storage_FileSystem",
  "Win32_Security",
  "Win32_Foundation",
] }

[dependencies]
tokio = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
```

---

## Слой 2: omnifuse-core — VFS ядро

### Backend trait

Единый trait для всех backend'ов. Сейчас реализуется для git и wiki — когда понадобится S3/WebDAV, trait обобщится при необходимости.

```rust
/// Backend для синхронизируемого хранилища.
///
/// Управляет sync самостоятельно: VFS-ядро говорит ЧТО синхронизировать,
/// backend решает КАК.
pub trait Backend: Send + Sync + 'static {
    /// Инициализация: подготовить локальную директорию.
    /// Git: clone (если remote URL) или проверить существующий repo.
    /// Wiki: создать local dir, fetch дерево, записать файлы.
    async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult>;

    /// Синхронизировать локальные изменения с remote.
    /// Вызывается SyncEngine'ом после debounce/close trigger.
    /// Backend сам решает как мержить при конфликтах.
    /// Git: stage → commit → push_with_retry (pull + git merge при reject).
    /// Wiki: для каждого файла → check modified_at → diffy merge → HTTP PUT.
    async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult>;

    /// Проверить remote на изменения (периодический poll).
    /// Git: fetch → compare local HEAD vs remote HEAD.
    /// Wiki: fetch tree → compare modified_at.
    async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>>;

    /// Применить remote изменения к локальной директории.
    /// НЕ перезаписывает dirty файлы (SyncEngine проверяет).
    async fn apply_remote(&self, changes: Vec<RemoteChange>) -> anyhow::Result<()>;

    /// Должен ли файл синхронизироваться с remote?
    /// Git: проверка .gitignore, скрытие .git/.
    /// Wiki: только .md файлы.
    fn should_track(&self, path: &Path) -> bool;

    /// Интервал опроса remote на изменения.
    fn poll_interval(&self) -> Duration;

    /// Проверить доступность remote.
    async fn is_online(&self) -> bool;

    /// Название backend'а для логов и UI.
    fn name(&self) -> &str;
}

/// Результат синхронизации.
pub enum SyncResult {
    Success { synced_files: usize },
    Conflict { synced_files: usize, conflict_files: Vec<PathBuf> },
    Offline,
}

/// Изменение на remote.
pub enum RemoteChange {
    Modified { path: PathBuf, content: Vec<u8> },
    Deleted { path: PathBuf },
}

/// Результат инициализации.
pub enum InitResult {
    Fresh,
    UpToDate,
    Updated,
    Conflicts { files: Vec<PathBuf> },
    Offline,
}
```

### Backend Registry (из rclone-lessons-and-tests.md)

Plugin-based registration через `inventory`:

```rust
pub struct BackendDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub constructor: fn(&BackendConfig) -> anyhow::Result<Box<dyn Backend>>,
}

inventory::collect!(BackendDescriptor);

// В omnifuse-git/lib.rs:
inventory::submit! {
    BackendDescriptor {
        name: "git",
        description: "Git repositories",
        constructor: |config| Ok(Box::new(GitBackend::from_config(config)?)),
    }
}

// В omnifuse-core:
pub fn list_backends() -> Vec<&'static BackendDescriptor> {
    inventory::iter::<BackendDescriptor>().collect()
}

pub fn create_backend(name: &str, config: &BackendConfig) -> anyhow::Result<Box<dyn Backend>> {
    inventory::iter::<BackendDescriptor>()
        .find(|b| b.name == name)
        .map(|b| (b.constructor)(config))
        .ok_or_else(|| anyhow::anyhow!("backend '{name}' не найден"))?
}
```

### vfs_handler.rs — OmniFuseVfs

Конкретная реализация `UniFuseFilesystem`, содержащая всю бизнес-логику:

```rust
/// Ядро файловой системы OmniFuse.
///
/// Реализует async UniFuseFilesystem, делегируя:
/// - Файловые операции → local directory (tokio::fs) + BufferManager
/// - Sync уведомления → SyncEngine (через канал)
/// - Фильтрацию → Backend.should_track()
///
/// Маппинг операций (из реального кода SimpleGitFS + YaWikiFS):
///
/// | Метод           | Поведение                                    |
/// |-----------------|----------------------------------------------|
/// | lookup          | скрытие .git/.vfs, stat                      |
/// | getattr         | tokio::fs::metadata → FileAttr               |
/// | setattr         | truncate/chmod → FsEvent                     |
/// | read            | buffer_manager → read                        |
/// | write           | buffer.write → FsEvent::FileModified         |
/// | open            | load в buffer_manager                        |
/// | release         | flush → FsEvent::FileClosed                  |
/// | create          | tokio::fs::File::create → FsEvent            |
/// | unlink          | tokio::fs::remove_file → FsEvent             |
/// | mkdir           | tokio::fs::create_dir                        |
/// | rmdir           | tokio::fs::remove_dir                        |
/// | rename          | tokio::fs::rename → FsEvent для обоих путей  |
/// | readdir         | tokio::fs::read_dir + фильтрация             |
/// | readlink        | tokio::fs::read_link                         |
/// | symlink         | tokio::fs::symlink → FsEvent                 |
/// | statfs          | nix::sys::statvfs / platform-specific        |
pub struct OmniFuseVfs {
    local_dir: PathBuf,
    buffer_manager: Arc<FileBufferManager>,
    sync_tx: mpsc::Sender<FsEvent>,
    filter: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    events: Arc<dyn VfsEventHandler>,
}

impl UniFuseFilesystem for OmniFuseVfs {
    async fn getattr(&self, path: &Path) -> Result<FileAttr, FsError> {
        let full_path = self.local_dir.join(path);
        let meta = tokio::fs::metadata(&full_path).await?;
        Ok(metadata_to_attr(&meta))
    }

    async fn read(&self, _path: &Path, fh: FileHandle, offset: u64, size: u32)
        -> Result<Vec<u8>, FsError> {
        self.buffer_manager.read(fh, offset, size).await
    }

    async fn write(&self, path: &Path, fh: FileHandle, offset: u64, data: &[u8])
        -> Result<u32, FsError> {
        let written = self.buffer_manager.write(fh, offset, data).await?;
        let _ = self.sync_tx.try_send(FsEvent::FileModified(path.to_owned()));
        self.events.on_file_written(path, written as usize);
        Ok(written)
    }

    async fn release(&self, path: &Path, fh: FileHandle) -> Result<(), FsError> {
        self.buffer_manager.flush_and_close(fh).await?;
        let _ = self.sync_tx.try_send(FsEvent::FileClosed(path.to_owned()));
        Ok(())
    }

    async fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let full_path = self.local_dir.join(path);
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&full_path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            if self.is_hidden(&entry.file_name()) { continue; }
            let kind = if entry.file_type().await?.is_dir() {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        Ok(entries)
    }

    // ... остальные методы — аналогично, все async
}
```

### sync_engine.rs — SyncEngine (из unified-vfs-design)

```rust
/// Событие от FUSE слоя.
pub enum FsEvent {
    FileModified(PathBuf),
    FileClosed(PathBuf),
    Flush,
    Shutdown,
}

/// Конфигурация sync engine.
pub struct SyncConfig {
    pub debounce_timeout: Duration,    // default: 1s
    pub max_retry_attempts: u32,       // default: 10
    pub initial_backoff: Duration,     // default: 100ms
    pub max_backoff: Duration,         // default: 30s
}

/// Движок синхронизации.
///
/// Оркестрирует timing: собирает dirty файлы, батчит через debounce,
/// триггерит sync на close, обрабатывает periodic poll.
///
/// НЕ занимается merge — это ответственность Backend.
pub struct SyncEngine {
    config: SyncConfig,
    event_tx: mpsc::Sender<FsEvent>,
}

impl SyncEngine {
    /// Создать SyncEngine и запустить background workers:
    /// 1. dirty_worker — собирает FsEvent, ведёт DirtySet, триггерит sync
    /// 2. poll_worker — периодически вызывает backend.poll_remote()
    pub fn start<B: Backend>(
        config: SyncConfig,
        backend: Arc<B>,
        events: Arc<dyn VfsEventHandler>,
    ) -> (Self, tokio::task::JoinHandle<()>);

    pub fn sender(&self) -> mpsc::Sender<FsEvent>;
    pub async fn shutdown(&self) -> anyhow::Result<()>;
}
```

**Логика dirty_worker (проверена в SimpleGitFS):**

```
loop {
    select! {
        event = event_rx.recv() => {
            match event {
                FileModified(path) => {
                    dirty_set.insert(path);
                    debounce_deadline = Instant::now() + config.debounce_timeout;
                }
                FileClosed(path) => {
                    dirty_set.insert(path);
                    trigger_sync = true; // Немедленный sync (не ждём debounce)
                }
                Flush => trigger_sync = true,
                Shutdown => { final_sync(); break; }
            }
        }
        _ = sleep_until(debounce_deadline), if !dirty_set.is_empty() => {
            trigger_sync = true;
        }
    }

    if trigger_sync && !dirty_set.is_empty() {
        let files: Vec<PathBuf> = dirty_set.drain()
            .filter(|f| backend.should_track(f))
            .collect();

        if !files.is_empty() {
            match backend.sync(&files).await {
                Ok(SyncResult::Success { synced_files }) =>
                    events.on_push(synced_files),
                Ok(SyncResult::Conflict { conflict_files, .. }) =>
                    events.on_log(Warn, &format!("конфликты: {conflict_files:?}")),
                Ok(SyncResult::Offline) =>
                    offline_queue.enqueue_all(files),
                Err(e) =>
                    events.on_log(Error, &format!("ошибка sync: {e}")),
            }
        }
        trigger_sync = false;
    }
}
```

**Логика poll_worker:**

```
loop {
    sleep(backend.poll_interval()).await;

    match backend.poll_remote().await {
        Ok(changes) if !changes.is_empty() => {
            let safe = changes.into_iter()
                .filter(|c| !dirty_set.contains(c.path()))
                .collect();
            backend.apply_remote(safe).await;
            events.on_sync("updated");
        }
        Err(e) => events.on_log(Warn, &format!("poll failed: {e}")),
        _ => {}
    }
}
```

### buffer.rs — FileBuffer (из SimpleGitFS)

Из SimpleGitFS `core/src/vfs/buffer.rs` (416 строк). Адаптация: `sync → async` (RwLock → tokio::sync::RwLock).

### events.rs — VfsEventHandler (из SimpleGitFS)

```rust
/// Обработчик событий для UI (Tauri, CLI, логи).
pub trait VfsEventHandler: Send + Sync + 'static {
    fn on_mounted(&self, source: &str, mount_point: &Path);
    fn on_unmounted(&self);
    fn on_error(&self, message: &str);
    fn on_log(&self, level: LogLevel, message: &str);
    fn on_file_written(&self, path: &Path, bytes: usize) {}
    fn on_file_dirty(&self, path: &Path) {}
    fn on_file_created(&self, path: &Path) {}
    fn on_file_deleted(&self, path: &Path) {}
    fn on_file_renamed(&self, old_path: &Path, new_path: &Path) {}
    fn on_commit(&self, hash: &str, files_count: usize, message: &str) {}
    fn on_push(&self, items_count: usize) {}
    fn on_push_rejected(&self) {}
    fn on_sync(&self, result: &str) {}
}
```

### pacer.rs — Rate Limiting (из rclone)

Защита от rate limits при обращении к API backend'ам (Wiki):

```rust
pub struct Pacer {
    min_sleep: Duration,
    max_sleep: Duration,
    current_sleep: Duration,
    attack_constant: u32,
    decay_constant: u32,
}

impl Pacer {
    pub async fn call<F, Fut, T>(&mut self, f: F) -> anyhow::Result<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        loop {
            self.wait_if_needed().await;
            match f().await {
                Ok(result) => { self.success(); return Ok(result); }
                Err(e) if is_retryable(&e) => { self.retry(); continue; }
                Err(e) => return Err(e),
            }
        }
    }
}
```

### conflict_log.rs (из YaWikiFS)

Из YaWikiFS `src/sync/conflict_log.rs` (143 строки) **без изменений**.

### offline_queue.rs (из SimpleGitFS)

Из SimpleGitFS `core/src/vfs/sync_queue.rs` (281 строка). Упрощение: убрать `try_sync`, оставить `enqueue`, `dequeue`, `BackoffState`.

### lib.rs — точка входа

```rust
/// Главная точка входа: смонтировать OmniFuse.
///
/// 1. Инициализирует backend (clone/fetch)
/// 2. Запускает SyncEngine (dirty tracking + periodic poll)
/// 3. Создаёт OmniFuseVfs (implements async UniFuseFilesystem)
/// 4. Монтирует через UniFuseHost
/// 5. При завершении: flush + final sync
pub async fn run_mount<B: Backend>(
    config: MountConfig,
    backend: B,
    events: impl VfsEventHandler,
) -> anyhow::Result<()> {
    let events: Arc<dyn VfsEventHandler> = Arc::new(events);
    let backend = Arc::new(backend);

    if !UniFuseHost::<OmniFuseVfs>::is_available() {
        anyhow::bail!("FUSE/WinFsp не установлен");
    }

    let init_result = backend.init(&config.local_dir).await?;
    events.on_sync(&format!("{init_result:?}"));

    let (sync_engine, sync_handle) = SyncEngine::start(
        config.sync.clone(), Arc::clone(&backend), Arc::clone(&events),
    );

    let vfs = OmniFuseVfs::new(
        config.local_dir.clone(),
        sync_engine.sender(),
        backend.clone(),
        events.clone(),
    );

    let host = UniFuseHost::new(vfs);
    events.on_mounted(backend.name(), &config.mount_point);
    host.mount(&config.mount_point, config.mount_options).await?;

    events.on_unmounted();
    sync_engine.shutdown().await?;
    sync_handle.await?;

    Ok(())
}
```

### config.rs

```rust
pub struct MountConfig {
    pub mount_point: PathBuf,
    pub local_dir: PathBuf,
    pub sync: SyncConfig,
    pub buffer: BufferConfig,
    pub mount_options: MountOptions,
    pub logging: LoggingConfig,
}

pub struct MountOptions {
    pub attr_cache_timeout_secs: u64,  // default: 60
    pub allow_other: bool,
    pub fs_name: String,
    pub readonly: bool,
}

pub struct SyncConfig {
    pub debounce_timeout_secs: u64,    // default: 1
    pub max_retry_attempts: u32,       // default: 10
    pub initial_backoff_ms: u64,       // default: 100
    pub max_backoff_secs: u64,         // default: 30
}

pub struct BufferConfig {
    pub max_memory_mb: usize,          // default: 512
    pub lru_eviction_enabled: bool,    // default: true
}
```

---

## Слой 3: Backend Implementations

### omnifuse-git (из SimpleGitFS)

```rust
pub struct GitBackend {
    engine: GitEngine,
    ops: GitOps,
    filter: GitignoreFilter,
}

impl Backend for GitBackend {
    async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
        // RepoSource::parse(source) → clone if remote
        // GitOps::startup_sync() → fetch + pull
    }

    async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
        // 1. git_ops.auto_commit(dirty_files)
        // 2. git_ops.push_with_retry(max_retries)
        //    Внутри: push → rejected → pull (git native merge!) → retry
    }

    async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
        // engine.fetch() → compare local HEAD vs remote HEAD
    }

    async fn apply_remote(&self, _changes: Vec<RemoteChange>) -> anyhow::Result<()> {
        // engine.pull() (fast-forward)
    }

    fn should_track(&self, path: &Path) -> bool {
        !self.filter.is_ignored(path) && !path.starts_with(".git")
    }

    fn poll_interval(&self) -> Duration { Duration::from_secs(30) }
    fn name(&self) -> &str { "git" }
}
```

**Файлы из SimpleGitFS (переносятся с минимальными изменениями):**

| Файл SimpleGitFS | → omnifuse-git/ | Изменения |
|---|---|---|
| `core/src/git/engine.rs` (493 строки) | `engine.rs` | `SimpleGitFsError` → `anyhow::Error` |
| `core/src/git/ops.rs` (282 строки) | `ops.rs` | Убрать `sync_commit()` |
| `core/src/git/merge.rs` (197 строк) | `merge.rs` | Без изменений |
| `core/src/git/repo_source.rs` (387 строк) | `repo_source.rs` | Без изменений |
| Новый | `filter.rs` (~50 строк) | Обёртка над `ignore::gitignore::Gitignore` |

**Настройки:**

```toml
[backends.my-repo]
type = "git"
remote = "https://github.com/user/repo"
branch = "main"
auto_commit = true
commit_on = "close"  # flush | close | periodic
sync_interval = "30s"
```

### omnifuse-wiki (из YaWikiFS)

```rust
pub struct WikiBackend {
    client: Arc<Client>,
    root_slug: String,
    meta_dir: PathBuf,
    pacer: Pacer,
}

impl Backend for WikiBackend {
    async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult> {
        // 1. Создать local_dir/.vfs/meta/ и .vfs/base/
        // 2. Fetch дерево: client.get_page_tree(root_slug)
        // 3. Для каждой страницы: записать .md + meta + base
    }

    async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
        // Для каждого файла:
        // 1. path → slug
        // 2. .vfs/meta/{slug}.json → base_modified_at
        // 3. pacer.call(|| client.get_page_by_slug(slug)) → remote
        // 4. Если remote_modified_at == base → просто PUT
        // 5. Иначе → diffy::three_way_merge(base, local, remote)
        // 6. client.update_page(id, merged_content)
        // 7. Обновить .vfs/meta/ и .vfs/base/
    }

    async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>> {
        // pacer.call(|| client.get_page_tree(root_slug))
        // Сравнить modified_at с .vfs/meta/{slug}.json
    }

    fn should_track(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e == "md")
            && !path.starts_with(".vfs")
    }

    fn poll_interval(&self) -> Duration { Duration::from_secs(60) }
    fn name(&self) -> &str { "wiki" }
}
```

**Маппинг:**
- Wiki page → `/Page_Title.md`
- Attachments → `/Page_Title.attachments/file.png`
- Metadata → `.vfs/meta/{slug}.json`
- Base для merge → `.vfs/base/{slug}.md`

**Файлы из YaWikiFS (переносятся):**

| Файл YaWikiFS | → omnifuse-wiki/ | Изменения |
|---|---|---|
| `src/wiki/client.rs` (317 строк) | `client.rs` | `WikiErr` → `anyhow::Error` |
| `src/wiki/models.rs` (228 строк) | `models.rs` | Без изменений |
| `src/sync/merge.rs` (64 строки) | `merge.rs` | Без изменений |
| Новый | `tree.rs` (~150 строк) | Логика fetch дерева |
| Новый | `meta.rs` (~100 строк) | .vfs/meta/*.json |

---

## Слой 4: CLI — `of`

```bash
# Монтирование
of mount git https://github.com/user/repo /mnt/repo --branch=main
of mount git /path/to/local/repo /mnt/repo
of mount wiki "https://wiki.example.com/root/slug/" /mnt/wiki --auth TOKEN

# Windows
of mount git C:\repos\myrepo C:\Users\Name\mnt\repo
of mount git C:\repos\myrepo X:

# Управление
of status <mountpoint>
of unmount <mountpoint>
of sync <mountpoint>          # Принудительная синхронизация
of list                       # Список активных маунтов

# Конфигурация
of config create <name> <backend> [--interactive]
of config list
of config edit <name>

# Диагностика
of check                      # Проверить наличие FUSE/WinFsp
of gen-config                 # Сгенерировать пример конфига
```

**Конфигурация**: `~/.config/omnifuse/config.toml`

```toml
[backends.my-git-repo]
type = "git"
remote = "https://github.com/user/repo"
branch = "main"
auto_commit = true

[backends.company-wiki]
type = "wiki"
provider = "yandex"
url = "https://wiki.yandex-team.ru"
root_slug = "my/project"
token = "secret"

[mounts."/home/user/mnt/repo"]
backend = "my-git-repo"
sync_interval = "30s"
debounce = "1s"

[mounts."/home/user/mnt/wiki"]
backend = "company-wiki"
sync_interval = "60s"
```

### CLI Cargo.toml

```toml
[package]
name = "omnifuse-cli"

[[bin]]
name = "of"
path = "src/main.rs"

[dependencies]
omnifuse-core = { path = "../omnifuse-core" }
omnifuse-git = { path = "../omnifuse-git" }
omnifuse-wiki = { path = "../omnifuse-wiki" }
unifuse = { path = "../unifuse" }

clap.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
anyhow.workspace = true
serde.workspace = true
toml = "0.8"
```

---

## Слой 5: GUI — omnifuse-gui (Tauri + React)

### Технологический стек

- **Backend**: Tauri 2.0 (Rust) — интеграция с `omnifuse-core`
- **Frontend**: React 18 + TypeScript
- **Bundler**: Vite 5
- **UI Framework**: shadcn/ui (Radix UI + Tailwind CSS)
- **State Management**: Zustand
- **Icons**: Lucide React

### Tauri Commands

```rust
#[tauri::command]
async fn mount_backend(
    backend_name: String, mountpoint: PathBuf,
    options: MountOptions, state: tauri::State<'_, AppState>
) -> Result<MountId, String>;

#[tauri::command]
async fn unmount(mount_id: MountId, state: tauri::State<'_, AppState>)
    -> Result<(), String>;

#[tauri::command]
async fn list_mounts(state: tauri::State<'_, AppState>)
    -> Result<Vec<MountInfo>, String>;

#[tauri::command]
async fn force_sync(mount_id: MountId, state: tauri::State<'_, AppState>)
    -> Result<(), String>;

#[tauri::command]
async fn check_platform() -> Result<PlatformInfo, String>;
```

### UI Features

1. **Dashboard**: список маунтов, статус sync (idle/syncing/conflict)
2. **Backend Configuration**: визуальный редактор конфигов, тест соединения
3. **Conflict Resolution UI**: diff viewer, выбор версии (local/remote/merge)
4. **Platform check**: проверка FUSE/macFUSE/WinFsp с инструкцией по установке
5. **Logs & Diagnostics**: real-time логи, debugging panel

---

## Тестовая стратегия (интегрированная)

### Уровни тестирования

#### 1. Unit тесты (каждый крейт)

| Крейт | Что тестируем | Источник |
|-------|---------------|----------|
| unifuse | InodeMap, type conversions, FsError→errno | SimpleGitFS inode_map tests |
| omnifuse-core | BufferManager, SyncEngine (debounce/close), offline queue | SimpleGitFS core tests |
| omnifuse-git | push/pull/merge через bare repos | SimpleGitFS concurrent_editing_tests |
| omnifuse-wiki | client через FakeApi, diffy merge | YaWikiFS integration tests |

#### 2. Backend Compliance Tests (из rclone-lessons-and-tests.md)

Generic test suite в `omnifuse-test`:

```rust
/// Запускает ~50 стандартных тестов для любого Backend.
pub async fn run_backend_compliance_tests<B: Backend>(backend: &B, test_dir: &Path) {
    test_init_creates_local_dir(backend, test_dir).await;
    test_sync_empty_no_error(backend, test_dir).await;
    test_write_and_sync(backend, test_dir).await;
    test_poll_remote_detects_changes(backend, test_dir).await;
    test_should_track_filtering(backend, test_dir).await;
    test_concurrent_sync(backend, test_dir).await;
    // ... ~50 тестов
}
```

#### 3. VfsHandler тесты (без mount — кроссплатформенные, async)

Тестирование `OmniFuseVfs` через прямые вызовы async path-based методов.
Работают на **всех платформах** (включая CI без FUSE):

```rust
#[tokio::test]
async fn test_write_read_roundtrip() {
    let temp_dir = tempdir().unwrap();
    let vfs = create_test_vfs(temp_dir.path()).await;

    let (handle, _attr) = vfs.create(
        Path::new("test.txt"), OpenFlags::write(), 0o644
    ).await.unwrap();
    vfs.write(Path::new("test.txt"), handle, 0, b"hello world").await.unwrap();

    let data = vfs.read(Path::new("test.txt"), handle, 0, 1024).await.unwrap();
    assert_eq!(&data, b"hello world");

    vfs.release(Path::new("test.txt"), handle).await.unwrap();
}
```

#### 4. FUSE mount тесты (Linux/macOS only)

```rust
#[tokio::test]
#[ignore] // Требует FUSE
async fn test_real_mount_write_read() {
    let mountpoint = setup_mount().await;
    tokio::fs::write(mountpoint.join("test.txt"), b"hello").await.unwrap();
    let content = tokio::fs::read_to_string(mountpoint.join("test.txt")).await.unwrap();
    assert_eq!(content, "hello");
    teardown_mount().await;
}
```

32 теста из SimpleGitFS fuse_mount_tests: directories, atomic save, mtime, rename, symlinks, truncate, consistency.

#### 5. Интеграционные тесты (end-to-end)

| Тест | Описание |
|------|----------|
| `git_backend_tests.rs` | push/pull/merge через bare repos |
| `wiki_backend_tests.rs` | CRUD через FakeApi (Axum mock server) |
| `sync_engine_tests.rs` | debounce, close trigger, offline queue, concurrent |
| `fuse_tests.rs` | FUSE mount + standard fs operations |
| `concurrent_editing_tests.rs` | Два клиента, конфликты, merge |

### Тестовая инфраструктура

```rust
// Bare git repos для тестов push/pull
pub async fn create_bare_and_two_clones()
    -> (TempDir, PathBuf, PathBuf, PathBuf);

// Axum mock Wiki API
pub struct FakeApi {
    pub base_url: String,
    state: Arc<DashMap<String, Page>>,
}

// Mock Backend для unit тестов
pub struct MockBackend {
    sync_calls: Arc<Mutex<Vec<Vec<PathBuf>>>>,
    should_track_fn: Box<dyn Fn(&Path) -> bool + Send + Sync>,
}
```

### CI: GitHub Actions

```yaml
jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - name: Install FUSE (Linux)
        if: runner.os == 'Linux'
        run: sudo apt-get install -y libfuse3-dev fuse3
      - name: Install macFUSE (macOS)
        if: runner.os == 'macOS'
        run: brew install macfuse
      - name: Install WinFsp (Windows)
        if: runner.os == 'Windows'
        run: choco install winfsp -y
      - run: cargo test --workspace
```

---

## Roadmap

### Phase 1: unifuse (2-3 недели)

- [ ] Async `UniFuseFilesystem` trait + types + error
- [ ] `Rfuse3Adapter` (Linux/macOS) + `InodeMap`
- [ ] `WinfspAdapter` (Windows)
- [ ] `UniFuseHost` async mount API
- [ ] Примеры: memfs, passthrough
- [ ] Unit тесты + CI на 3 платформах

### Phase 2: omnifuse-core (3-4 недели)

- [ ] `Backend` trait
- [ ] `SyncEngine` (dirty_worker + poll_worker)
- [ ] `OmniFuseVfs` (implements async `UniFuseFilesystem`)
- [ ] `FileBufferManager` (async, перенос из SimpleGitFS)
- [ ] `VfsEventHandler`, `ConflictLog`, `OfflineQueue`
- [ ] `BackendRegistry` (inventory)
- [ ] Unit тесты с MockBackend

### Phase 3: omnifuse-git (2-3 недели)

- [ ] Перенос `engine.rs`, `ops.rs`, `merge.rs`, `repo_source.rs` из SimpleGitFS
- [ ] `impl Backend for GitBackend`
- [ ] `filter.rs` (.gitignore)
- [ ] Тесты через bare repos
- [ ] Backend compliance tests

### Phase 4: omnifuse-wiki (2-3 недели)

- [ ] Перенос `client.rs`, `models.rs`, `merge.rs` из YaWikiFS
- [ ] `tree.rs`, `meta.rs` (новый код)
- [ ] `impl Backend for WikiBackend`
- [ ] `Pacer` для API calls
- [ ] Тесты через FakeApi (Axum mock)

### Phase 5: CLI `of` (1-2 недели)

- [ ] mount/unmount/status/list commands
- [ ] TOML config management
- [ ] `of check` (проверка FUSE/WinFsp)
- [ ] Ручное тестирование на каждой платформе

### Phase 6: GUI (3-4 недели)

- [ ] Tauri skeleton + commands
- [ ] React UI (dashboard, config editor, conflict resolution)
- [ ] Platform check UI

---

## Технические требования

- **Rust**: nightly, edition 2024
- **Node.js**: 18+ (для GUI frontend)
- **Platform dependencies**:
  - Linux: `sudo apt install fuse3 libfuse3-dev`
  - macOS: `brew install macfuse`
  - Windows: `choco install winfsp` или `winget install WinFsp.WinFsp`

---

## Риски и митигация

| Риск | Вероятность | Митигация |
|------|-------------|-----------|
| Баги rfuse3 на macOS | Средняя | YaWikiFS уже работает на macOS с rfuse3 |
| Потеря git merge quality | Низкая | Git backend вызывает `git pull` напрямую |
| Regression в sync timing | Средняя | Тесты: debounce + close trigger + concurrent writes из SimpleGitFS |
| Wiki API несовместимость | Низкая | FakeApi тесты покрывают все операции |
| winfsp-rs незрелость | Средняя | Crate v0.12, Snowflake development. Fallback: winfsp-sys FFI |
| Различия FS-семантики Win/Unix | Высокая | OmniFuseVfs абстрагирует: symlink → NotSupported на Windows |
| Windows CI | Средняя | GitHub Actions windows-latest + `choco install winfsp` |
| Path encoding (UTF-16 vs UTF-8) | Средняя | WinFsp adapter: `U16CStr ↔ OsString ↔ Path` |
| async trait overhead | Низкая | rfuse3 native async — нет overhead от blocking bridge на Unix |
