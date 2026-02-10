# cgofuse → unifuse: Полное руководство по портированию

## Обзор задачи

Портирование cgofuse на Rust как unifuse — это **не** полная переписка кода cgofuse, а создание адаптационного слоя поверх существующих Rust-крейтов `fuser` и `winfsp-rs`. CGO-зависимость полностью устраняется.

---

## Часть 1: Архитектура cgofuse

### 1.1. Структура cgofuse (Go)

```
cgofuse/fuse/
├── fsop.go                 # Интерфейс FileSystemInterface (42 метода)
├── host.go                 # FileSystemHost — dispatch layer
├── host_cgo.go             # CGO backend для Unix (826 строк C + 420 Go)
├── host_nocgo_windows.go   # Pure Go backend для Windows (1021 строк)
├── fsop_cgo.go             # CGO type mapping (330 строк)
├── fsop_nocgo_windows.go   # Nocgo type mapping (188 строк)
└── errstr.go               # Errno → string (98 строк)

Итого: ~4613 строк (без тестов/примеров)
```

### 1.2. Три слоя cgofuse

```
┌────────────────────────────────────────────────┐
│  FileSystemInterface (42 метода)               │ ← Пользовательский код
│  Init, Destroy, Getattr, Open, Read, Write... │
├────────────────────────────────────────────────┤
│  FileSystemHost (dispatch)                     │ ← host.go
│  Mount, Unmount, Signal handling, Handle table│
├─────────────────┬──────────────────────────────┤
│  CGO Backend    │  Nocgo Windows Backend       │
│  (host_cgo.go)  │  (host_nocgo_windows.go)     │
│                 │                              │
│  dlopen libfuse │  LoadLibrary winfsp-x64.dll  │
│  fuse_operations│  syscall.Call                │
│  C trampolines  │  Go trampolines              │
└─────────────────┴──────────────────────────────┘
```

---

## Часть 2: CGO в host_cgo.go — что это и зачем

### 2.1. Что делает CGO-код

CGO-блок перед `import "C"` выполняет три функции:

#### A. Lazy-загрузка libfuse/WinFsp (200 строк C)

```c
// Объявление function pointers
static int (*pfn_fuse_main_real)(int argc, char *argv[], ...);
static struct fuse_context *(*pfn_fuse_get_context)(void);

// Runtime загрузка через dlopen (Unix) или LoadLibrary (Windows)
static void *cgofuse_init_fuse(void) {
    void *h = dlopen("/usr/local/lib/libfuse.2.dylib", RTLD_NOW);  // macOS
    // или dlopen("libfuse.so.2", RTLD_NOW); на Linux
    // или LoadLibraryW(L"winfsp-x64.dll"); на Windows
    
    pfn_fuse_main_real = dlsym(h, "fuse_main_real");
    pfn_fuse_get_context = dlsym(h, "fuse_get_context");
    // ...
    return h;
}
```

**Зачем**: библиотека не линкуется статически, а ищется в runtime. На macOS проверяются три пути (macFUSE, OSXFuse, FUSE-T).

**В Rust**: заменяется крейтом `libloading` или вообще не нужно — fuser работает напрямую с `/dev/fuse` на Linux без libfuse.

#### B. Конвертация C-структур ↔ Go-типы (250 строк C)

```c
// Заполняет struct stat из Go-аргументов
static inline void hostCstatFromFusestat(fuse_stat_t *stbuf,
    uint64_t dev, uint64_t ino, uint32_t mode, uint32_t nlink,
    uint32_t uid, uint32_t gid, uint64_t rdev, int64_t size,
    int64_t atimSec, int64_t atimNsec, ...) {
    
    memset(stbuf, 0, sizeof *stbuf);
    stbuf->st_dev = dev;
    stbuf->st_ino = ino;
    stbuf->st_mode = mode;
    // ...
    
#if defined(__APPLE__)
    stbuf->st_atimespec.tv_sec = atimSec;  // macOS
#else
    stbuf->st_atim.tv_sec = atimSec;       // Linux
#endif
}
```

**Зачем**: CGO не позволяет Go-коду писать в поля C-структур с platform-specific layout. Хелперы абстрагируют различия между `struct stat` на macOS/Linux/Windows.

**В Rust**: заменяется `#[repr(C)]` структурами + `cfg(target_os)`. Rust умеет работать с C-структурами напрямую через FFI.

#### C. struct fuse_operations + callback trampolines (376 строк C)

```c
// Объявления extern функций (вызываются из C, реализованы в Go)
extern int go_hostGetattr(char *path, fuse_stat_t *stbuf);
extern int go_hostRead(char *path, char *buf, size_t size, fuse_off_t off,
    struct fuse_file_info *fi);
// ... 40 других

// Заполнение fuse_operations
static struct fuse_operations fsop = {
    .getattr  = go_hostGetattr,
    .readlink = go_hostReadlink,
    .read     = go_hostRead,
    .write    = go_hostWrite,
    // ... 42 поля
};

// Вызов FUSE
fuse_main_real(argc, argv, &fsop, sizeof fsop, data);
```

Каждый `go_hostXxx` — это трамплин: FUSE kernel → C → Go.

**В Rust**: этот паттерн не нужен — fuser и winfsp-rs уже реализуют dispatch внутри себя. Пользователь имплементирует trait, крейты вызывают методы напрямую.

### 2.2. Почему CGO было нужно в Go

Go не может:

1. Вызывать function pointer'ы напрямую из pure Go (нужен C-трамплин)
2. Работать с C-структурами, где layout зависит от `#define` макросов и архитектуры
3. Линковаться с shared library динамически без CGO (cgo — это bridge)

Rust может всё это через FFI без дополнительных слоёв.

---

## Часть 3: Портирование на Rust — по слоям

### 3.1. Trait UniFuseFilesystem — аналог FileSystemInterface

**cgofuse (Go)**:

```go
type FileSystemInterface interface {
    Init()
    Destroy()
    Getattr(path string, stat *Stat_t, fh uint64) int
    Open(path string, flags int) (int, uint64)
    Read(path string, buff []byte, ofst int64, fh uint64) int
    Write(path string, buff []byte, ofst int64, fh uint64) int
    Readdir(path string, fill func(name string, stat *Stat_t, ofst int64) bool, ofst int64, fh uint64) int
    Release(path string, fh uint64) int
    // ... ещё 34 метода
}
```

**unifuse (Rust)**:

```rust
pub trait UniFuseFilesystem {
    fn init(&mut self, config: &mut KernelConfig) -> Result<()>;
    fn destroy(&mut self);
    
    fn getattr(&self, path: &Path) -> Result<FileAttr>;
    fn open(&self, path: &Path, flags: OpenFlags) -> Result<FileHandle>;
    fn read(&self, path: &Path, fh: FileHandle, offset: u64, size: u32) -> Result<Vec<u8>>;
    fn write(&self, path: &Path, fh: FileHandle, offset: u64, data: &[u8]) -> Result<u32>;
    fn readdir(&self, path: &Path) -> Result<Vec<DirEntry>>;
    fn release(&self, path: &Path, fh: FileHandle) -> Result<()>;
    
    // ... остальные 34 метода
}

// Типы
pub struct FileAttr {
    pub size: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,  // Windows/macOS
    pub kind: FileType,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
}

pub struct FileHandle(u64);

pub enum FileType {
    RegularFile,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    NamedPipe,
    Socket,
}
```

**Ключевые отличия**:

- Rust: `Result<T>` вместо `int` errno
- Path-based: `&Path` вместо `string` (более type-safe)
- Ownership: `Vec<u8>` вместо `[]byte` (явное владение)

### 3.2. Адаптер для fuser (Linux/macOS)

fuser имеет **inode-based API**, а unifuse — **path-based**. Нужна конвертация.

**Inode↔Path Mapping Table**:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub struct InodeTable {
    inode_to_path: HashMap<u64, PathBuf>,
    path_to_inode: HashMap<PathBuf, u64>,
    next_inode: u64,
}

impl InodeTable {
    pub fn lookup(&mut self, parent: u64, name: &OsStr) -> Option<u64> {
        let parent_path = self.inode_to_path.get(&parent)?;
        let path = parent_path.join(name);
        
        if let Some(&inode) = self.path_to_inode.get(&path) {
            return Some(inode);
        }
        
        // Allocate new inode
        let inode = self.next_inode;
        self.next_inode += 1;
        self.inode_to_path.insert(inode, path.clone());
        self.path_to_inode.insert(path, inode);
        Some(inode)
    }
    
    pub fn forget(&mut self, inode: u64, nlookup: u64) {
        // Remove from table if refcount drops to zero
        // (simplified: real impl needs reference counting)
        if nlookup == 0 {
            if let Some(path) = self.inode_to_path.remove(&inode) {
                self.path_to_inode.remove(&path);
            }
        }
    }
}

pub struct FuserAdapter<F: UniFuseFilesystem> {
    inner: Arc<F>,
    inode_table: Arc<RwLock<InodeTable>>,
}

impl<F: UniFuseFilesystem> fuser::Filesystem for FuserAdapter<F> {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let mut table = self.inode_table.write().unwrap();
        
        if let Some(inode) = table.lookup(parent, name) {
            let path = table.inode_to_path.get(&inode).unwrap();
            
            match self.inner.getattr(path) {
                Ok(attr) => {
                    let ttl = Duration::from_secs(1);
                    reply.entry(&ttl, &convert_attr(attr, inode), 0);
                }
                Err(e) => reply.error(errno_from_error(e)),
            }
        } else {
            reply.error(libc::ENOENT);
        }
    }
    
    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let table = self.inode_table.read().unwrap();
        let path = table.inode_to_path.get(&ino).unwrap();
        
        match self.inner.getattr(path) {
            Ok(attr) => {
                let ttl = Duration::from_secs(1);
                reply.attr(&ttl, &convert_attr(attr, ino));
            }
            Err(e) => reply.error(errno_from_error(e)),
        }
    }
    
    fn read(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, size: u32, 
            _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        let table = self.inode_table.read().unwrap();
        let path = table.inode_to_path.get(&ino).unwrap();
        
        match self.inner.read(path, FileHandle(fh), offset as u64, size) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno_from_error(e)),
        }
    }
    
    // ... остальные методы fuser::Filesystem (всего ~48)
}

fn convert_attr(attr: FileAttr, ino: u64) -> fuser::FileAttr {
    fuser::FileAttr {
        ino,
        size: attr.size,
        blocks: (attr.size + 511) / 512,
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: attr.crtime,
        kind: match attr.kind {
            FileType::RegularFile => fuser::FileType::RegularFile,
            FileType::Directory => fuser::FileType::Directory,
            // ...
        },
        perm: attr.perm,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: 0,
        flags: 0,
        blksize: 4096,
    }
}
```

**Объём кода**: ~500–800 строк (inode table + все 48 методов fuser trait).

### 3.3. Адаптер для winfsp-rs (Windows)

winfsp-rs уже path-based, маппинг проще:

```rust
use winfsp::FileSystemContext;

pub struct WinfspAdapter<F: UniFuseFilesystem> {
    inner: Arc<F>,
}

impl<F: UniFuseFilesystem> FileSystemContext for WinfspAdapter<F> {
    type FileContext = FileHandle;
    
    fn get_security_by_name(&self, file_name: &U16CStr, ...) -> Result<()> {
        // Windows security → Unix permissions mapping (упрощённый)
        Ok(())
    }
    
    fn open(&self, file_name: &U16CStr, ...) -> Result<Self::FileContext> {
        let path = Path::new(&file_name.to_os_string());
        let flags = ...; // convert CreateOptions → OpenFlags
        self.inner.open(path, flags).map_err(ntstatus_from_error)
    }
    
    fn read(&self, file_context: &Self::FileContext, buffer: &mut [u8], 
            offset: u64) -> Result<u32> {
        // Проблема: winfsp-rs не передаёт path в read()!
        // Нужно либо:
        // 1. Хранить path в FileContext (FileHandle содержит PathBuf)
        // 2. Или изменить trait UniFuseFilesystem: read принимает только fh
        
        // Вариант 1: FileHandle = struct { id: u64, path: PathBuf }
        self.inner.read(&file_context.path, *file_context, offset, buffer.len() as u32)
            .and_then(|data| {
                buffer[..data.len()].copy_from_slice(&data);
                Ok(data.len() as u32)
            })
            .map_err(ntstatus_from_error)
    }
    
    fn get_file_info(&self, file_context: &Self::FileContext, file_info: &mut FileInfo) 
        -> Result<()> {
        let attr = self.inner.getattr(&file_context.path)
            .map_err(ntstatus_from_error)?;
        
        file_info.set_file_size(attr.size);
        file_info.set_file_attributes(windows_attrs_from_perm(attr.perm));
        file_info.set_creation_time(system_time_to_filetime(attr.crtime));
        file_info.set_last_access_time(system_time_to_filetime(attr.atime));
        file_info.set_last_write_time(system_time_to_filetime(attr.mtime));
        file_info.set_change_time(system_time_to_filetime(attr.ctime));
        
        Ok(())
    }
    
    // ... остальные ~30 методов FileSystemContext
}

fn ntstatus_from_error(e: Error) -> NTSTATUS {
    match e.kind() {
        ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        ErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
        ErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
        // ... полная таблица errno → NTSTATUS
        _ => STATUS_UNSUCCESSFUL,
    }
}

fn windows_attrs_from_perm(perm: u16) -> u32 {
    let mut attrs = 0;
    if perm & 0o200 == 0 {
        attrs |= FILE_ATTRIBUTE_READONLY;
    }
    if perm & 0o40000 != 0 {  // S_IFDIR
        attrs |= FILE_ATTRIBUTE_DIRECTORY;
    }
    attrs
}

fn system_time_to_filetime(time: SystemTime) -> FILETIME {
    // Convert Unix epoch → Windows FILETIME (100ns intervals since 1601)
    // ...
}
```

**Объём кода**: ~400–600 строк (30 методов + type conversions).

### 3.4. Mount API

```rust
pub struct UniFuseHost<F: UniFuseFilesystem> {
    fs: Arc<F>,
    mountpoint: PathBuf,
    #[cfg(unix)]
    session: Option<fuser::Session>,
    #[cfg(windows)]
    host: Option<winfsp::FileSystemHost<WinfspAdapter<F>>>,
}

impl<F: UniFuseFilesystem + 'static> UniFuseHost<F> {
    pub fn new(fs: F) -> Self {
        Self {
            fs: Arc::new(fs),
            mountpoint: PathBuf::new(),
            #[cfg(unix)]
            session: None,
            #[cfg(windows)]
            host: None,
        }
    }
    
    pub fn mount(&mut self, mountpoint: PathBuf, options: &[&str]) -> Result<()> {
        self.mountpoint = mountpoint.clone();
        
        #[cfg(unix)]
        {
            let adapter = FuserAdapter {
                inner: self.fs.clone(),
                inode_table: Arc::new(RwLock::new(InodeTable::new())),
            };
            
            let session = fuser::Session::new(
                adapter,
                &mountpoint,
                options
            )?;
            
            self.session = Some(session);
        }
        
        #[cfg(windows)]
        {
            let adapter = WinfspAdapter {
                inner: self.fs.clone(),
            };
            
            let volume_params = winfsp::VolumeParams::default();
            let host = winfsp::FileSystemHost::new(volume_params, adapter)?;
            host.mount(&mountpoint)?;
            
            self.host = Some(host);
        }
        
        Ok(())
    }
    
    pub fn unmount(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            if let Some(session) = self.session.take() {
                session.unmount()?;
            }
        }
        
        #[cfg(windows)]
        {
            if let Some(host) = self.host.take() {
                host.unmount()?;
            }
        }
        
        Ok(())
    }
}
```

**Объём кода**: ~200–300 строк.

---

## Часть 4: Сводная таблица портирования

| cgofuse компонент | Строк | unifuse эквивалент | Строк | Замечания |
|-------------------|-------|---------------------|-------|-----------|
| `fsop.go` (trait) | 632 | `UniFuseFilesystem` trait | 500–700 | Path-based, Result\<T\> |
| `host.go` (dispatch) | 1098 | `UniFuseHost` | 200–300 | Упрощён (нет CGO bridge) |
| `host_cgo.go` (826 C + 420 Go) | 1246 | **НЕ НУЖЕН** | 0 | Заменено fuser internals |
| `host_nocgo_windows.go` | 1021 | **НЕ НУЖЕН** | 0 | Заменено winfsp-rs |
| `fsop_cgo.go` (type mapping) | 330 | `FuserAdapter` | 500–800 | Inode↔path + conversions |
| `fsop_nocgo_windows.go` | 188 | `WinfspAdapter` | 400–600 | NTSTATUS mapping |
| `errstr.go` | 98 | Error handling | 100–200 | thiserror + errno tables |
| **Итого** | **4613** | | **~1700–2600** | **Меньше в 2–3 раза** |

---

## Часть 5: Детали реализации

### 5.1. Errno ↔ Result\<T\> маппинг

**cgofuse** возвращает POSIX errno (int):

```go
func (fs *MyFs) Read(path string, buff []byte, ofst int64, fh uint64) int {
    // ...
    return -fuse.ENOENT  // Not found
}
```

**unifuse** использует `Result<T, Error>`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Not found")]
    NotFound,
    
    #[error("Permission denied")]
    PermissionDenied,
    
    #[error("Already exists")]
    AlreadyExists,
    
    #[error("Not a directory")]
    NotADirectory,
    
    #[error("Is a directory")]
    IsADirectory,
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn to_errno(&self) -> i32 {
        match self {
            Error::NotFound => libc::ENOENT,
            Error::PermissionDenied => libc::EACCES,
            Error::AlreadyExists => libc::EEXIST,
            Error::NotADirectory => libc::ENOTDIR,
            Error::IsADirectory => libc::EISDIR,
            Error::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            Error::Other(_) => libc::EIO,
        }
    }
    
    #[cfg(windows)]
    pub fn to_ntstatus(&self) -> NTSTATUS {
        match self {
            Error::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            Error::PermissionDenied => STATUS_ACCESS_DENIED,
            Error::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            Error::NotADirectory => STATUS_NOT_A_DIRECTORY,
            Error::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
            _ => STATUS_UNSUCCESSFUL,
        }
    }
}
```

### 5.2. FileHandle management

cgofuse использует `uint64` как opaque handle. unifuse может быть type-safe:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct FileHandle(u64);

static NEXT_FH: AtomicU64 = AtomicU64::new(1);

impl FileHandle {
    pub fn new() -> Self {
        Self(NEXT_FH.fetch_add(1, Ordering::Relaxed))
    }
    
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}
```

Если winfsp-rs требует хранить path в FileContext:

```rust
pub struct FileHandleWithPath {
    id: u64,
    path: PathBuf,
}
```

### 5.3. Platform-specific attributes

macOS/Windows имеют дополнительные атрибуты (crtime, flags, reparse points). Хранить в `FileAttr`:

```rust
pub struct FileAttr {
    // POSIX
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub kind: FileType,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    
    // Extended (macOS/Windows)
    pub crtime: SystemTime,       // Creation time
    pub flags: u32,                // BSD flags (macOS) or FILE_ATTRIBUTE_* (Windows)
    pub reparse_tag: Option<u32>,  // Windows reparse points
}
```

---

## Часть 6: Тестирование

### 6.1. Unit tests

Минимальный in-memory FS для тестов адаптеров:

```rust
struct MemFs {
    files: HashMap<PathBuf, Vec<u8>>,
}

impl UniFuseFilesystem for MemFs {
    fn getattr(&self, path: &Path) -> Result<FileAttr> {
        if path == Path::new("/") {
            return Ok(FileAttr {
                size: 0,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                // ...
            });
        }
        
        if self.files.contains_key(path) {
            let data = &self.files[path];
            Ok(FileAttr {
                size: data.len() as u64,
                kind: FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                // ...
            })
        } else {
            Err(Error::NotFound)
        }
    }
    
    fn read(&self, path: &Path, _fh: FileHandle, offset: u64, size: u32) 
        -> Result<Vec<u8>> {
        let data = self.files.get(path).ok_or(Error::NotFound)?;
        let start = offset as usize;
        let end = (start + size as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }
    
    fn write(&self, path: &Path, _fh: FileHandle, offset: u64, data: &[u8]) 
        -> Result<u32> {
        // ...
    }
    
    // ... остальные методы (минимальная реализация)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fuser_adapter() {
        let memfs = MemFs::new();
        let adapter = FuserAdapter::new(Arc::new(memfs));
        
        // Тестировать lookup, getattr, read через fuser API
    }
    
    #[test]
    #[cfg(windows)]
    fn test_winfsp_adapter() {
        let memfs = MemFs::new();
        let adapter = WinfspAdapter::new(Arc::new(memfs));
        
        // Тестировать open, read, get_file_info через winfsp API
    }
}
```

### 6.2. Integration tests

Монтирование реального FS и проверка через системные вызовы:

```rust
#[test]
#[ignore] // Требует root/admin
fn test_real_mount() {
    let tempdir = TempDir::new().unwrap();
    let mountpoint = tempdir.path().join("mount");
    std::fs::create_dir(&mountpoint).unwrap();
    
    let memfs = MemFs::new();
    let mut host = UniFuseHost::new(memfs);
    host.mount(mountpoint.clone(), &[]).unwrap();
    
    // Читать/писать через std::fs
    std::fs::write(mountpoint.join("test.txt"), b"hello").unwrap();
    let content = std::fs::read_to_string(mountpoint.join("test.txt")).unwrap();
    assert_eq!(content, "hello");
    
    host.unmount().unwrap();
}
```

### 6.3. Кроссплатформенные тесты (CI)

GitHub Actions:

```yaml
name: unifuse CI

on: [push, pull_request]

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      
      - name: Install FUSE (Linux)
        if: runner.os == 'Linux'
        run: sudo apt-get install -y libfuse3-dev fuse3
      
      - name: Install macFUSE (macOS)
        if: runner.os == 'macOS'
        run: brew install macfuse
      
      - name: Install WinFsp (Windows)
        if: runner.os == 'Windows'
        run: choco install winfsp -y
      
      - name: Run tests
        run: cargo test --all-features
```

---

## Часть 7: Roadmap реализации unifuse

### Week 1: Trait + Types

- [ ] Определить `UniFuseFilesystem` trait (42 метода)
- [ ] Типы: `FileAttr`, `FileHandle`, `DirEntry`, `OpenFlags`, `Error`
- [ ] Feature flags: `fuse2`, `fuse3`, `winfsp`, `dokan`

### Week 2: FuserAdapter (Linux/macOS)

- [ ] `InodeTable` (inode↔path mapping)
- [ ] Реализовать все 48 методов `fuser::Filesystem`
- [ ] Unit tests с MemFs

### Week 3: WinfspAdapter (Windows)

- [ ] Реализовать все 30 методов `FileSystemContext`
- [ ] Errno ↔ NTSTATUS таблица
- [ ] Windows attributes ↔ Unix permissions mapping
- [ ] Unit tests на Windows

### Week 4: Mount API + Examples

- [ ] `UniFuseHost` (mount/unmount)
- [ ] Signal handling (graceful shutdown)
- [ ] Примеры: memfs, passthrough
- [ ] Integration tests (реальное монтирование)

### Week 5: CI + Documentation

- [ ] GitHub Actions для трёх ОС
- [ ] API docs (rustdoc)
- [ ] README с примерами
- [ ] Публикация на crates.io

---

## Часть 8: Промпты для ИИ-ассистента

### Промпт 1: Генерация trait

```
Создай Rust trait `UniFuseFilesystem` на основе cgofuse FileSystemInterface.
Интерфейс должен быть path-based (не inode-based) и использовать Result<T, Error>.

Методы (42 штуки):
- init, destroy
- getattr, setattr
- open, read, write, flush, release
- readdir, mkdir, rmdir
- create, unlink, rename
- symlink, readlink
- setxattr, getxattr, listxattr, removexattr
- statfs
- (и остальные из cgofuse/fuse/fsop.go)

Типы:
- FileAttr: size, atime, mtime, ctime, crtime, kind, perm, nlink, uid, gid, flags
- FileHandle: opaque u64 wrapper
- DirEntry: name, kind
- OpenFlags: битовые флаги (READ, WRITE, CREATE, TRUNCATE, APPEND)

Error enum: NotFound, PermissionDenied, AlreadyExists, IsADirectory, NotADirectory, Io(std::io::Error), Other(String)

Реализуй также default implementations для некоторых методов (возвращающих Error::NotSupported).
```

### Промпт 2: InodeTable

```
Реализуй InodeTable для маппинга inode ↔ path в Rust.

Требования:
- HashMap<u64, PathBuf> для inode → path
- HashMap<PathBuf, u64> для path → inode (bidirectional)
- Методы: lookup(parent_inode, name) → Option<inode>, forget(inode, nlookup), get_path(inode) → Option<&Path>
- Thread-safe (Arc<RwLock<InodeTable>>)
- Автоинкремент next_inode (начинается с 2, так как 1 — это root)

Также реализуй reference counting для inode (чтобы знать, когда можно удалить из таблицы).
```

### Промпт 3: FuserAdapter

```
Реализуй FuserAdapter<F: UniFuseFilesystem> который имплементирует fuser::Filesystem.

Адаптер должен:
1. Конвертировать inode-based вызовы fuser → path-based вызовы UniFuseFilesystem
2. Использовать InodeTable для маппинга
3. Конвертировать типы: fuser::FileAttr ↔ unifuse::FileAttr, fuser::FileType ↔ unifuse::FileType
4. Конвертировать ошибки: unifuse::Error → libc errno

Реализуй методы:
- lookup, forget
- getattr, setattr
- open, read, write, flush, release
- readdir
- mkdir, rmdir, unlink, rename
- create
- symlink, readlink
(остальные можно заглушить через reply.error(libc::ENOSYS))

Используй правильные Reply типы из fuser (ReplyEntry, ReplyAttr, ReplyData, ReplyEmpty, и т.д.).
```

### Промпт 4: WinfspAdapter

```
Реализуй WinfspAdapter<F: UniFuseFilesystem> который имплементирует winfsp::FileSystemContext.

Адаптер должен:
1. Конвертировать winfsp callbacks → unifuse методы
2. FileContext = struct с (id, path) — так как winfsp не передаёт path в read/write
3. Конвертировать типы: unifuse::FileAttr → winfsp::FileInfo (FILE_ATTRIBUTE_*, FILETIME)
4. Конвертировать ошибки: unifuse::Error → NTSTATUS

Методы:
- get_security_by_name (заглушка: всегда успех)
- open → unifuse::open, сохранить path в FileContext
- close → unifuse::release
- read, write (используя path из FileContext)
- get_file_info → unifuse::getattr
- set_basic_info → unifuse::setattr (mtime/atime)
- read_directory → unifuse::readdir
- create, cleanup, set_delete
- rename

Также реализуй helper функции:
- system_time_to_filetime(SystemTime) → FILETIME
- filetime_to_system_time(FILETIME) → SystemTime
- windows_attrs_from_perm(u16) → FILE_ATTRIBUTE_*
- ntstatus_from_error(Error) → NTSTATUS (полная таблица)
```

### Промпт 5: UniFuseHost

```
Реализуй UniFuseHost<F: UniFuseFilesystem> — API для монтирования.

Структура:
- fs: Arc<F>
- mountpoint: PathBuf
- #[cfg(unix)] session: Option<fuser::Session>
- #[cfg(windows)] host: Option<winfsp::FileSystemHost<WinfspAdapter<F>>>

Методы:
- new(fs: F) -> Self
- mount(&mut self, mountpoint: PathBuf, options: &[&str]) -> Result<()>
  - На Unix: создать FuserAdapter, запустить fuser::Session
  - На Windows: создать WinfspAdapter, winfsp::FileSystemHost, вызвать mount()
- unmount(&mut self) -> Result<()>
  - Вызвать unmount на session/host, дождаться завершения
- is_mounted(&self) -> bool

Опции монтирования:
- allow_other, allow_root, ro, rw, auto_unmount, fsname=..., subtype=...

Используй #[cfg(unix)] и #[cfg(windows)] для platform-specific кода.
```

---

## Заключение

Портирование cgofuse → unifuse — это **не** полная переписка, а создание тонкого адаптационного слоя (~1700–2600 строк) поверх fuser и winfsp-rs, которые делают всю тяжёлую работу.

CGO-зависимость (826 строк C) полностью устраняется благодаря тому, что:
1. fuser работает напрямую с `/dev/fuse` без libfuse на Linux
2. winfsp-rs использует `libloading` для WinFsp DLL
3. Rust FFI позволяет работать с C-структурами через `#[repr(C)]`

Основная сложность — маппинг inode↔path для fuser-адаптера и errno↔NTSTATUS для winfsp-адаптера, но это решаемые задачи с понятной архитектурой.
