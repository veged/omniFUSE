# rclone: Архитектурные уроки и тестовые сценарии для omnifuse

## Обзор документа

Этот документ извлекает ключевые архитектурные решения из rclone, которые стоит использовать в omnifuse, и описывает фундаментальные тест-кейсы со ссылками на код rclone.

---

## Часть 1: Архитектурные приёмы rclone

### 1.1. Plugin-based Backend Registry

**Источник**: `backend/all/all.go`, `fs/registry.go`

**Паттерн**: Каждый backend регистрирует себя через `init()` в пакете `backend/all`:

```go
// backend/s3/s3.go
func init() {
    fs.Register(&fs.RegInfo{
        Name:        "s3",
        Description: "Amazon S3 Compliant Storage Providers",
        NewFs:       NewFs,  // Конструктор fs.Fs
        Options:     options,
    })
}

// backend/all/all.go
import _ "github.com/rclone/rclone/backend/s3"
import _ "github.com/rclone/rclone/backend/drive"
// ... 70+ импортов
```

**Для omnifuse (Rust)**:

Использовать `inventory` или `linkme` для auto-регистрации:

```rust
use inventory;

pub struct BackendDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub constructor: fn() -> Box<dyn OmniFsBackend>,
}

inventory::collect!(BackendDescriptor);

// В omnifuse-git/lib.rs
inventory::submit! {
    BackendDescriptor {
        name: "git",
        description: "Git repositories",
        constructor: || Box::new(GitBackend::new()),
    }
}

// В omnifuse-core
pub fn list_backends() -> Vec<&'static BackendDescriptor> {
    inventory::iter::<BackendDescriptor>().collect()
}

pub fn create_backend(name: &str) -> Result<Box<dyn OmniFsBackend>> {
    inventory::iter::<BackendDescriptor>()
        .find(|b| b.name == name)
        .map(|b| (b.constructor)())
        .ok_or_else(|| Error::BackendNotFound(name.to_string()))
}
```

**Тест-кейс**: Verify all registered backends are listed

```rust
#[test]
fn test_backend_registry() {
    let backends = list_backends();
    assert!(backends.iter().any(|b| b.name == "git"));
    assert!(backends.iter().any(|b| b.name == "s3"));
    // ...
}
```

---

### 1.2. Backend Features Table

**Источник**: `fs/features.go`, `backend/s3/s3.go` (метод `Features()`)

**Паттерн**: Backend декларирует свои возможности через структуру `Features`:

```go
// fs/features.go
type Features struct {
    CaseInsensitive         bool
    DuplicateFiles          bool
    ReadMimeType            bool
    WriteMimeType           bool
    CanHaveEmptyDirectories bool
    BucketBased             bool
    ServerSideMove          bool
    ServerSideCopy          bool
    // ... ещё ~30 полей
}

// backend/s3/s3.go
func (f *Fs) Features() *fs.Features {
    return &fs.Features{
        BucketBased:             true,
        ServerSideCopy:          true,
        ServerSideMove:          true,
        CanHaveEmptyDirectories: true,
        // ...
    }
}
```

**Зачем**: rclone оптимизирует операции на основе Features. Например, если backend поддерживает `ServerSideCopy`, то `rclone copy` делает server-side copy, а не download+upload.

**Для omnifuse (Rust)**:

```rust
pub struct BackendFeatures {
    pub case_insensitive: bool,
    pub case_preserving: bool,
    pub can_have_empty_dirs: bool,
    pub server_side_copy: bool,
    pub server_side_move: bool,
    pub partial_read: bool,  // Поддержка Range requests
    pub chunked_upload: bool,
    pub metadata: MetadataSupport,
    pub versioning: bool,  // Для Git/wiki backends
    pub conflict_detection: bool,
}

pub struct MetadataSupport {
    pub mtime: bool,
    pub atime: bool,
    pub permissions: bool,
    pub owner: bool,
    pub extended_attributes: bool,
}

pub trait OmniFsBackend {
    fn name(&self) -> &str;
    fn features(&self) -> BackendFeatures;
    // ...
}
```

**Применение**:

```rust
// В omnifuse-core/sync_engine.rs
impl SyncEngine {
    fn sync_file(&self, backend: &dyn OmniFsBackend, from: &Path, to: &Path) -> Result<()> {
        if backend.features().server_side_copy {
            // Оптимизация: server-side copy
            backend.copy(from, to)
        } else {
            // Fallback: download + upload
            let data = backend.read_object(from, None)?;
            backend.write_object(to, data)
        }
    }
}
```

**Тест-кейс**: Verify feature-based optimization

```rust
#[test]
fn test_server_side_copy_optimization() {
    struct MockBackend {
        server_side_copy_called: Arc<AtomicBool>,
    }
    
    impl OmniFsBackend for MockBackend {
        fn features(&self) -> BackendFeatures {
            BackendFeatures {
                server_side_copy: true,
                ..Default::default()
            }
        }
        
        fn copy(&self, from: &Path, to: &Path) -> Result<()> {
            self.server_side_copy_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    
    let backend = MockBackend { /* ... */ };
    let engine = SyncEngine::new();
    engine.sync_file(&backend, Path::new("/a"), Path::new("/b")).unwrap();
    
    assert!(backend.server_side_copy_called.load(Ordering::SeqCst));
}
```

---

### 1.3. VFS Cache Layer (4 режима)

**Источник**: `vfs/vfs.go`, `vfs/cache.go`, `vfs/file.go`

**Архитектура VFS**:

```
┌─────────────────────────────────────────┐
│        FUSE (через cgofuse)             │
├─────────────────────────────────────────┤
│              VFS Layer                  │
│  ┌─────────────────────────────────┐   │
│  │  Dir Cache (directory listings) │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Attr Cache (file metadata)     │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  File Cache (content, sparse)   │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Writeback Queue                │   │
│  └─────────────────────────────────┘   │
├─────────────────────────────────────────┤
│         Backend (fs.Fs)                 │
└─────────────────────────────────────────┘
```

**Режимы кеширования** (`--vfs-cache-mode`):

| Режим | Dir cache | Attr cache | Content cache | Writeback |
|-------|-----------|------------|---------------|-----------|
| `off` | ✗ | ✗ | ✗ | ✗ |
| `minimal` | ✓ | ✓ | ✗ | ✗ |
| `writes` | ✓ | ✓ | ✗ (только writeback buffer) | ✓ |
| `full` | ✓ | ✓ | ✓ (sparse files на диске) | ✓ |

**Ссылки на код**:

- `vfs/vfs.go:44-86` — структура VFS, конфиг
- `vfs/dir.go` — directory cache
- `vfs/file.go:100-250` — file cache + read-ahead
- `vfs/write.go` — writeback queue

**Для omnifuse (Rust)**:

```rust
// omnifuse-core/vfs_cache.rs
pub enum CacheMode {
    Off,
    Minimal,
    Writes,
    Full,
}

pub struct VfsCache {
    mode: CacheMode,
    dir_cache: DirCache,
    attr_cache: AttrCache,
    file_cache: Option<FileCache>,  // Some только для Full mode
    writeback: Option<WritebackQueue>,  // Some для Writes/Full
}

pub struct DirCache {
    entries: HashMap<PathBuf, (Vec<DirEntry>, Instant)>,
    ttl: Duration,  // --dir-cache-time
}

pub struct AttrCache {
    attrs: HashMap<PathBuf, (FileAttr, Instant)>,
    ttl: Duration,  // --attr-cache-time
}

pub struct FileCache {
    cache_dir: PathBuf,  // ~/.cache/omnifuse/
    chunks: HashMap<PathBuf, SparseFile>,
    max_size: u64,  // --vfs-cache-max-size
    eviction: EvictionPolicy,  // LRU/LFU/ARC
}

pub struct SparseFile {
    path: PathBuf,
    chunks: BTreeMap<u64, Chunk>,  // offset → chunk
    total_size: u64,
}

pub struct WritebackQueue {
    pending: Vec<WriteOp>,
    delay: Duration,  // --vfs-write-back (default: 5s)
}
```

**Конфигурация** (аналог rclone flags):

```rust
pub struct VfsConfig {
    pub cache_mode: CacheMode,
    pub dir_cache_time: Duration,      // default: 5m
    pub attr_cache_time: Duration,     // default: 1s
    pub vfs_read_chunk_size: u64,      // default: 128MB
    pub vfs_read_chunk_streams: usize, // default: 4 (parallel read-ahead)
    pub vfs_write_back: Duration,      // default: 5s
    pub vfs_cache_max_size: u64,       // default: unlimited
    pub vfs_cache_max_age: Duration,   // default: 1h
}
```

**Реализация Read-ahead** (по мотивам `vfs/read.go`):

```rust
impl FileCache {
    pub async fn read_with_readahead(
        &self,
        backend: &dyn OmniFsBackend,
        path: &Path,
        offset: u64,
        size: u32,
        config: &VfsConfig,
    ) -> Result<Vec<u8>> {
        let chunk_size = config.vfs_read_chunk_size;
        let num_streams = config.vfs_read_chunk_streams;
        
        // 1. Проверить локальный кеш
        if let Some(cached) = self.get_from_cache(path, offset, size) {
            return Ok(cached);
        }
        
        // 2. Запустить параллельные read-ahead'ы
        let mut handles = vec![];
        for i in 0..num_streams {
            let chunk_offset = offset + (i as u64 * chunk_size);
            let handle = tokio::spawn({
                let backend = backend.clone();
                let path = path.to_owned();
                async move {
                    backend.read_object(&path, Some(chunk_offset..chunk_offset + chunk_size)).await
                }
            });
            handles.push((chunk_offset, handle));
        }
        
        // 3. Дождаться первого чанка, остальные в фоне
        let (first_offset, first_handle) = handles.remove(0);
        let first_data = first_handle.await??;
        
        // 4. Кешировать оставшиеся чанки по мере готовности
        tokio::spawn(async move {
            for (offset, handle) in handles {
                if let Ok(Ok(data)) = handle.await {
                    self.store_chunk(path, offset, data).await;
                }
            }
        });
        
        // 5. Вернуть запрошенный диапазон из первого чанка
        let start = (offset - first_offset) as usize;
        let end = start + size as usize;
        Ok(first_data[start..end.min(first_data.len())].to_vec())
    }
}
```

**Тест-кейсы** (по мотивам `vfs/vfs_test.go`):

```rust
#[tokio::test]
async fn test_vfs_cache_off_no_caching() {
    let backend = MockBackend::new();
    let vfs = VfsCache::new(CacheMode::Off, VfsConfig::default());
    
    // Два последовательных read должны вызвать backend дважды
    vfs.read(&backend, Path::new("/file.txt"), 0, 100).await.unwrap();
    vfs.read(&backend, Path::new("/file.txt"), 0, 100).await.unwrap();
    
    assert_eq!(backend.read_count(), 2);
}

#[tokio::test]
async fn test_vfs_cache_minimal_dir_cache() {
    let backend = MockBackend::new();
    let mut config = VfsConfig::default();
    config.dir_cache_time = Duration::from_secs(60);
    let vfs = VfsCache::new(CacheMode::Minimal, config);
    
    // Первый readdir → backend
    vfs.readdir(&backend, Path::new("/")).await.unwrap();
    assert_eq!(backend.readdir_count(), 1);
    
    // Второй readdir (в пределах TTL) → кеш
    vfs.readdir(&backend, Path::new("/")).await.unwrap();
    assert_eq!(backend.readdir_count(), 1);
    
    // Через TTL → снова backend
    tokio::time::sleep(Duration::from_secs(61)).await;
    vfs.readdir(&backend, Path::new("/")).await.unwrap();
    assert_eq!(backend.readdir_count(), 2);
}

#[tokio::test]
async fn test_vfs_cache_full_sparse_files() {
    let tempdir = TempDir::new().unwrap();
    let mut config = VfsConfig::default();
    config.vfs_cache_max_size = 1024 * 1024 * 100; // 100MB
    
    let backend = MockBackend::new();
    let vfs = VfsCache::new(CacheMode::Full, config);
    vfs.set_cache_dir(tempdir.path());
    
    // Читаем chunk в середине файла (offset=1MB, size=1KB)
    let data = vfs.read(&backend, Path::new("/bigfile.bin"), 1024*1024, 1024).await.unwrap();
    
    // Проверить, что на диске sparse file (не весь файл скачан)
    let cache_file = tempdir.path().join("bigfile.bin");
    assert!(cache_file.exists());
    
    // Метаданные sparse file должны показывать меньше занимаемого места
    let metadata = std::fs::metadata(&cache_file).unwrap();
    assert!(metadata.len() < 2 * 1024 * 1024);  // Меньше 2MB (не весь файл)
}

#[tokio::test]
async fn test_vfs_writeback_delayed_flush() {
    let backend = MockBackend::new();
    let mut config = VfsConfig::default();
    config.vfs_write_back = Duration::from_secs(5);
    let vfs = VfsCache::new(CacheMode::Writes, config);
    
    // Write в файл
    vfs.write(&backend, Path::new("/file.txt"), 0, b"hello").await.unwrap();
    
    // Сразу после write backend ещё не вызван
    assert_eq!(backend.write_count(), 0);
    
    // Через delay (5s) → flush в backend
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(backend.write_count(), 1);
}
```

---

### 1.4. Pacer — Rate Limiting для Backend

**Источник**: `fs/pacer.go`

**Проблема**: Некоторые backend'ы (Google Drive, Dropbox) имеют rate limits. rclone использует Pacer для автоматической задержки между запросами.

```go
// fs/pacer.go
type Pacer struct {
    minSleep time.Duration
    maxSleep time.Duration
    decayConstant uint
    attackConstant uint
    // ...
}

func (p *Pacer) Call(fn func() (bool, error)) error {
    for {
        p.beginCall()
        retry, err := fn()
        p.endCall(retry)
        if !retry {
            return err
        }
        // Exponential backoff
        time.Sleep(p.calculateSleep())
    }
}
```

**Для omnifuse (Rust)**:

```rust
use std::time::{Duration, Instant};
use tokio::time::sleep;

pub struct Pacer {
    min_sleep: Duration,
    max_sleep: Duration,
    current_sleep: Duration,
    attack_constant: u32,  // Делить на attack при успехе
    decay_constant: u32,   // Умножать на decay при ошибке
    last_call: Option<Instant>,
}

impl Pacer {
    pub fn new(min_sleep: Duration, max_sleep: Duration) -> Self {
        Self {
            min_sleep,
            max_sleep,
            current_sleep: min_sleep,
            attack_constant: 2,
            decay_constant: 2,
            last_call: None,
        }
    }
    
    pub async fn call<F, T>(&mut self, mut f: F) -> Result<T>
    where
        F: FnMut() -> Result<T>,
    {
        loop {
            self.begin_call().await;
            
            match f() {
                Ok(result) => {
                    self.end_call_success();
                    return Ok(result);
                }
                Err(e) if e.is_retryable() => {
                    self.end_call_retry();
                    // Continue loop for retry
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }
    
    async fn begin_call(&mut self) {
        if let Some(last) = self.last_call {
            let elapsed = last.elapsed();
            if elapsed < self.current_sleep {
                sleep(self.current_sleep - elapsed).await;
            }
        }
        self.last_call = Some(Instant::now());
    }
    
    fn end_call_success(&mut self) {
        // Decrease sleep (attack)
        self.current_sleep = (self.current_sleep / self.attack_constant).max(self.min_sleep);
    }
    
    fn end_call_retry(&mut self) {
        // Increase sleep (decay)
        self.current_sleep = (self.current_sleep * self.decay_constant).min(self.max_sleep);
    }
}
```

**Применение в backend**:

```rust
impl WikiBackend {
    async fn fetch_page(&self, title: &str) -> Result<String> {
        self.pacer.call(|| {
            // HTTP request к wiki API
            self.http_client.get(&format!("/wiki/{}", title))
                .send()
                .await
                .map_err(|e| {
                    if e.status() == Some(StatusCode::TOO_MANY_REQUESTS) {
                        Error::Retryable(e)
                    } else {
                        Error::Permanent(e)
                    }
                })
        }).await
    }
}
```

**Тест-кейс**:

```rust
#[tokio::test]
async fn test_pacer_exponential_backoff() {
    let mut pacer = Pacer::new(Duration::from_millis(10), Duration::from_secs(5));
    let mut call_count = 0;
    let mut call_times = vec![];
    
    let result = pacer.call(|| {
        call_times.push(Instant::now());
        call_count += 1;
        
        if call_count < 3 {
            Err(Error::Retryable("rate limit".into()))
        } else {
            Ok("success")
        }
    }).await.unwrap();
    
    assert_eq!(result, "success");
    assert_eq!(call_count, 3);
    
    // Проверить, что задержки росли экспоненциально
    let delay1 = call_times[1] - call_times[0];
    let delay2 = call_times[2] - call_times[1];
    assert!(delay2 > delay1);
}
```

---

### 1.5. Accounting — Статистика операций

**Источник**: `fs/accounting.go`, `fs/accounting_stats.go`

rclone собирает детальную статистику: bytes transferred, transfer rate, ETA, errors.

**Для omnifuse**:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub struct AccountingStats {
    pub bytes_read: AtomicU64,
    pub bytes_written: AtomicU64,
    pub files_read: AtomicUsize,
    pub files_written: AtomicUsize,
    pub errors: AtomicUsize,
    pub start_time: Instant,
}

impl AccountingStats {
    pub fn record_read(&self, bytes: u64) {
        self.bytes_read.fetch_add(bytes, Ordering::Relaxed);
        self.files_read.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_write(&self, bytes: u64) {
        self.bytes_written.fetch_add(bytes, Ordering::Relaxed);
        self.files_written.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn throughput(&self) -> f64 {
        let total_bytes = self.bytes_read.load(Ordering::Relaxed) 
                        + self.bytes_written.load(Ordering::Relaxed);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        total_bytes as f64 / elapsed
    }
}

// Использование
impl OmniFsCore {
    pub async fn read_with_accounting(&self, path: &Path, offset: u64, size: u32) 
        -> Result<Vec<u8>> {
        match self.backend.read_object(path, Some(offset..offset + size as u64)).await {
            Ok(data) => {
                self.stats.record_read(data.len() as u64);
                Ok(data)
            }
            Err(e) => {
                self.stats.record_error();
                Err(e)
            }
        }
    }
}
```

---

## Часть 2: Фундаментальные тест-кейсы

### 2.1. Backend Compliance Tests

**Источник**: `backend/test/test.go` (функция `Run()`)

rclone имеет **generic test suite**, которую каждый backend должен пройти. Это ~200 тестов, проверяющих базовую функциональность.

**Структура**:

```go
// backend/test/test.go:200-1500
func Run(t *testing.T, opt *Opt) {
    // Создать временную директорию в backend
    // Запустить все тесты
    
    t.Run("List", testList)
    t.Run("NewObject", testNewObject)
    t.Run("Put", testPut)
    t.Run("Get", testGet)
    t.Run("Mkdir", testMkdir)
    t.Run("Rmdir", testRmdir)
    t.Run("Copy", testCopy)
    t.Run("Move", testMove)
    // ... ещё ~200 тестов
}
```

**Для omnifuse (Rust)**:

Создать generic test suite `omnifuse-test` крейт:

```rust
// crates/omnifuse-test/lib.rs
pub fn run_backend_compliance_tests<B: OmniFsBackend>(backend: B) {
    // Макрос для генерации тестов
    backend_test_suite! {
        backend: backend,
        
        test_list_empty_directory,
        test_list_with_files,
        test_create_file,
        test_read_file,
        test_write_file,
        test_delete_file,
        test_mkdir,
        test_rmdir,
        test_rename_file,
        test_rename_directory,
        test_copy_file,
        test_move_file,
        test_concurrent_writes,
        test_large_file,
        test_many_small_files,
        // ... ~100 тестов
    }
}

// В каждом backend:
// omnifuse-git/tests/compliance.rs
#[test]
fn test_git_backend_compliance() {
    let tempdir = TempDir::new().unwrap();
    let repo = git2::Repository::init(&tempdir).unwrap();
    let backend = GitBackend::new(repo);
    
    omnifuse_test::run_backend_compliance_tests(backend);
}
```

**Ключевые тесты**:

1. **test_list_consistency**: `readdir` должен возвращать все файлы, которые были созданы через `write`
2. **test_stat_after_write**: `getattr` должен возвращать правильный размер сразу после `write`
3. **test_rename_across_directories**: `rename(/a/file, /b/file)` должен работать
4. **test_delete_non_empty_directory**: `rmdir(/dir)` с файлами внутри должен возвращать ENOTEMPTY
5. **test_concurrent_read_write**: параллельные read/write не должны вызывать race conditions
6. **test_sparse_write**: `write(offset=1GB, data=1KB)` должен работать без скачивания всего файла

---

### 2.2. VFS Integration Tests

**Источник**: `vfs/vfs_test.go`, `vfs/file_test.go`

Тесты монтирования + операций через системные вызовы.

**Примеры**:

```rust
#[test]
#[ignore] // Требует root/FUSE
fn test_vfs_mount_read_write() {
    let tempdir = TempDir::new().unwrap();
    let mountpoint = tempdir.path().join("mount");
    std::fs::create_dir(&mountpoint).unwrap();
    
    let backend = MemBackend::new();
    let vfs = VfsCache::new(CacheMode::Full, VfsConfig::default());
    let mut host = UniFuseHost::new(vfs);
    host.mount(mountpoint.clone(), &[]).unwrap();
    
    // Записать файл через std::fs
    std::fs::write(mountpoint.join("test.txt"), b"hello world").unwrap();
    
    // Прочитать обратно
    let content = std::fs::read_to_string(mountpoint.join("test.txt")).unwrap();
    assert_eq!(content, "hello world");
    
    // Проверить, что данные попали в backend
    let backend_data = backend.read_object(Path::new("/test.txt"), None).unwrap();
    assert_eq!(backend_data, b"hello world");
    
    host.unmount().unwrap();
}

#[test]
#[ignore]
fn test_vfs_cache_write_back() {
    // Аналогично test_vfs_writeback_delayed_flush выше
}

#[test]
#[ignore]
fn test_vfs_concurrent_access() {
    let mountpoint = setup_mount();
    
    // Запустить 10 потоков, каждый пишет в свой файл
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let mp = mountpoint.clone();
            std::thread::spawn(move || {
                let path = mp.join(format!("file{}.txt", i));
                for j in 0..100 {
                    std::fs::write(&path, format!("data{}", j)).unwrap();
                }
            })
        })
        .collect();
    
    for h in handles {
        h.join().unwrap();
    }
    
    // Проверить, что все 10 файлов созданы
    for i in 0..10 {
        assert!(mountpoint.join(format!("file{}.txt", i)).exists());
    }
}
```

---

### 2.3. Конфликты и Sync Tests

**Источник**: `cmd/sync/sync_test.go`, `cmd/bisync/bisync_test.go`

rclone имеет команду `bisync` для bidirectional sync с conflict detection.

**Тест-кейсы для omnifuse**:

```rust
#[tokio::test]
async fn test_conflict_detection_both_modified() {
    let backend = MockBackend::new();
    let sync_engine = SyncEngine::new(backend);
    
    // 1. Синхронизированное состояние: local=v1, remote=v1
    sync_engine.sync().await.unwrap();
    
    // 2. Локально изменить файл: local=v2
    std::fs::write("/mnt/file.txt", b"local v2").unwrap();
    
    // 3. Remote изменить тот же файл: remote=v3
    backend.write_object(Path::new("/file.txt"), Box::new(&b"remote v3"[..])).await.unwrap();
    
    // 4. Запустить sync → должен детектировать конфликт
    let result = sync_engine.sync().await;
    assert!(matches!(result, Err(Error::Conflict(_))));
    
    // 5. Проверить, что ConflictResolver был вызван
    let conflicts = sync_engine.get_conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, Path::new("/file.txt"));
}

#[tokio::test]
async fn test_conflict_resolution_local_wins() {
    let backend = MockBackend::new();
    let mut config = SyncConfig::default();
    config.conflict_strategy = ConflictStrategy::LocalWins;
    let sync_engine = SyncEngine::new(backend, config);
    
    // Создать конфликт (как выше)
    // ...
    
    // Sync с LocalWins
    sync_engine.sync().await.unwrap();
    
    // Проверить, что remote перезаписан локальной версией
    let remote_data = backend.read_object(Path::new("/file.txt"), None).await.unwrap();
    assert_eq!(remote_data, b"local v2");
}

#[tokio::test]
async fn test_conflict_resolution_three_way_merge() {
    let backend = MockBackend::new();
    let mut config = SyncConfig::default();
    config.conflict_strategy = ConflictStrategy::ThreeWayMerge;
    let sync_engine = SyncEngine::new(backend, config);
    
    // 1. Базовая версия
    let base = b"line1\nline2\nline3\n";
    backend.write_object(Path::new("/file.txt"), Box::new(&base[..])).await.unwrap();
    sync_engine.sync().await.unwrap();
    
    // 2. Локально добавить строку в начало
    let local = b"line0\nline1\nline2\nline3\n";
    std::fs::write("/mnt/file.txt", local).unwrap();
    
    // 3. Remote добавить строку в конец
    let remote = b"line1\nline2\nline3\nline4\n";
    backend.write_object(Path::new("/file.txt"), Box::new(&remote[..])).await.unwrap();
    
    // 4. Sync → three-way merge должен объединить
    sync_engine.sync().await.unwrap();
    
    // 5. Результат: обе строки добавлены
    let merged = std::fs::read_to_string("/mnt/file.txt").unwrap();
    assert_eq!(merged, "line0\nline1\nline2\nline3\nline4\n");
}
```

---

### 2.4. Git Backend Tests

**Специфичные для omnifuse-git**:

```rust
#[test]
fn test_git_backend_auto_commit() {
    let tempdir = TempDir::new().unwrap();
    let repo = git2::Repository::init(&tempdir).unwrap();
    
    let mut config = GitBackendConfig::default();
    config.auto_commit = true;
    config.commit_message_template = "Auto: {path}";
    
    let backend = GitBackend::new(repo, config);
    
    // Записать файл
    backend.write_object(Path::new("/README.md"), Box::new(&b"# Hello"[..])).unwrap();
    
    // Проверить, что создан коммит
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.message().unwrap(), "Auto: README.md");
}

#[test]
fn test_git_backend_branch_switching() {
    let repo = create_test_repo_with_branches();
    let backend = GitBackend::new(repo);
    
    // По умолчанию на main
    let content = backend.read_object(Path::new("/file.txt"), None).unwrap();
    assert_eq!(content, b"main version");
    
    // Переключиться на feature-branch
    backend.switch_branch("feature-branch").unwrap();
    
    // Содержимое изменилось
    let content = backend.read_object(Path::new("/file.txt"), None).unwrap();
    assert_eq!(content, b"feature version");
}

#[test]
fn test_git_backend_conflict_on_pull() {
    let repo = create_test_repo_with_remote();
    let backend = GitBackend::new(repo);
    
    // Локально изменить файл
    backend.write_object(Path::new("/file.txt"), Box::new(&b"local"[..])).unwrap();
    
    // Remote тоже изменил тот же файл (simulate)
    simulate_remote_push(&repo, "/file.txt", b"remote");
    
    // Pull → должен создать конфликт
    let result = backend.pull();
    assert!(matches!(result, Err(Error::MergeConflict(_))));
    
    // Проверить, что файл помечен как конфликтный
    let conflicts = backend.get_conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
}
```

---

## Часть 3: Промпты для ИИ

### Промпт 1: Реализация Backend Registry

```
Реализуй backend registry для omnifuse с использованием крейта `inventory`.

Требования:
1. Создать структуру BackendDescriptor с полями: name, description, constructor
2. Использовать inventory::collect! для сбора всех зарегистрированных backends
3. Функция list_backends() -> Vec<&'static BackendDescriptor>
4. Функция create_backend(name: &str) -> Result<Box<dyn OmniFsBackend>>
5. Каждый backend (omnifuse-git, omnifuse-s3, etc.) регистрирует себя через inventory::submit!

Также создай тест, который проверяет, что все backend'ы из workspace зарегистрированы.
```

### Промпт 2: Реализация VFS Cache Layer

```
Реализуй VFS cache layer для omnifuse с 4 режимами кеширования (Off, Minimal, Writes, Full).

Компоненты:
1. DirCache: HashMap<PathBuf, (Vec<DirEntry>, Instant)> с TTL
2. AttrCache: HashMap<PathBuf, (FileAttr, Instant)> с TTL
3. FileCache (для Full mode): sparse files на диске в ~/.cache/omnifuse/, BTreeMap для chunks
4. WritebackQueue (для Writes/Full): Vec<WriteOp> с отложенной записью

Методы VfsCache:
- read_with_cache(backend, path, offset, size) -> Result<Vec<u8>>
- write_with_writeback(backend, path, offset, data) -> Result<()>
- readdir_with_cache(backend, path) -> Result<Vec<DirEntry>>
- getattr_with_cache(backend, path) -> Result<FileAttr>

Конфигурация через VfsConfig:
- cache_mode, dir_cache_time, attr_cache_time, vfs_read_chunk_size, vfs_write_back, etc.

Реализуй также read-ahead (параллельное чтение N чанков) и LRU eviction для FileCache.
```

### Промпт 3: Реализация Pacer

```
Реализуй Pacer для rate limiting в omnifuse.

Структура:
- min_sleep, max_sleep, current_sleep (экспоненциальный backoff)
- attack_constant, decay_constant
- last_call: Option<Instant>

Методы:
- async fn call<F, T>(&mut self, f: F) -> Result<T> where F: FnMut() -> Result<T>
  - begin_call(): ждать min delay между вызовами
  - end_call_success(): уменьшить sleep (attack)
  - end_call_retry(): увеличить sleep (decay)
  - Retry loop для retryable errors

Интеграция:
- Каждый backend должен иметь поле pacer: Pacer
- Использовать в HTTP-вызовах: self.pacer.call(|| http_request()).await

Тест: проверить, что при последовательных retryable errors задержка растёт экспоненциально.
```

### Промпт 4: Generic Backend Compliance Tests

```
Создай generic test suite для проверки compliance backend'ов в omnifuse.

Функция:
pub fn run_backend_compliance_tests<B: OmniFsBackend>(backend: B)

Тесты (~50 штук):
- test_list_empty_directory
- test_list_with_files
- test_create_file
- test_read_file
- test_write_file
- test_delete_file
- test_mkdir, test_rmdir
- test_rename_file, test_rename_directory
- test_copy_file (если Features::server_side_copy)
- test_concurrent_writes
- test_large_file (>100MB)
- test_many_small_files (1000 файлов по 1KB)
- test_sparse_write (write at offset=1GB)
- test_stat_consistency (getattr после write должен возвращать правильный размер)
- ... и другие

Каждый тест:
1. Создаёт временную директорию в backend
2. Выполняет операции
3. Проверяет результат через getattr/readdir/read
4. Очищает временные данные

Использовать в каждом backend:
#[test]
fn test_git_backend_compliance() {
    let backend = setup_git_backend();
    omnifuse_test::run_backend_compliance_tests(backend);
}
```

---

## Заключение

Ключевые архитектурные уроки из rclone для omnifuse:

1. **Plugin-based registry** — расширяемость через inventory/linkme
2. **Features table** — оптимизация операций на основе возможностей backend
3. **VFS cache layer** — 4 режима для разных use-case'ов
4. **Pacer** — защита от rate limits
5. **Accounting** — мониторинг производительности

Фундаментальные тест-кейсы:

1. **Backend compliance suite** — generic тесты для всех backend'ов
2. **VFS integration tests** — реальное монтирование + системные вызовы
3. **Conflict resolution tests** — проверка bidirectional sync
4. **Backend-specific tests** — git auto-commit, branch switching, etc.

Используй эти паттерны при реализации omnifuse, и проект будет иметь ту же архитектурную зрелость, что и rclone.
