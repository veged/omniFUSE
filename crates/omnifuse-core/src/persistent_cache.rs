//! Persistent on-disk cache shared between mount runs and concurrent mounts.
//!
//! Keyed by `(instance_hash, path, version_token) -> bytes`. The cache is opaque
//! about what `path` and `version_token` mean — backends own freshness:
//! - S3: object key + `ETag`
//! - Wiki: page slug + revision id
//! - Listings: synthetic prefix + `ttl-fence:<unix-ts>`
//!
//! Storage layout under `$root/v1/`:
//!
//! ```text
//! v1/
//! └── <instance_hash>/
//!     └── <sha256(path)[..2]>/
//!         └── <sha256(path)>/
//!             └── <sha256(version)>     # opaque bytes, immutable
//! ```
//!
//! Each blob is immutable once written: new versions land in new filenames, so
//! reads never race writes. Atomic put: write `*.tmp.<pid>.<nanos>`, then rename.

use std::{
  fs, io,
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering}
  },
  time::{SystemTime, UNIX_EPOCH}
};

use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use tracing::{debug, trace, warn};

/// On-disk schema version. A mismatch wipes the directory and starts fresh.
pub const SCHEMA_VERSION: &str = "v1";

const SCHEMA_MARKER_FILE: &str = ".omnifuse-cache-version";

/// Default total cache size budget (1 GiB).
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Trigger eviction after this many puts.
const EVICTION_PUT_INTERVAL: u64 = 64;

/// Stable hash of normalised mount parameters.
///
/// Two cache users with the same normalised input produce the same hash across
/// runs and across machines (SHA-256 of canonicalised parts).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct InstanceHash(String);

impl InstanceHash {
  /// Derive an `InstanceHash` from an ordered list of normalised parts.
  ///
  /// Parts are joined with `\u{1f}` (Unit Separator) so distinct components
  /// cannot collide via concatenation. The hash is the lower-case hex of SHA-256.
  #[must_use]
  pub fn from_parts(parts: &[&str]) -> Self {
    let mut hasher = Sha256::new();
    for (idx, part) in parts.iter().enumerate() {
      if idx > 0 {
        hasher.update([0x1f_u8]);
      }
      hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
      use std::fmt::Write as _;
      let _ = write!(hex, "{byte:02x}");
    }
    Self(hex)
  }

  /// Borrow the underlying hex string.
  #[must_use]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// Cache lookup key. References a single immutable blob.
#[derive(Debug, Clone, Copy)]
pub struct CacheKey<'a> {
  /// Per-instance namespace.
  pub instance: &'a InstanceHash,
  /// Backend-opaque path (typically the object key or page slug).
  pub path: &'a str,
  /// Backend-owned version token (`ETag`, revision id, `ttl-fence:<unix-ts>`, ...).
  pub version: &'a str
}

/// Persistent cache abstraction.
///
/// Three methods are enough. Eviction, schema migration, and on-disk layout are
/// implementation details — not part of the trait.
pub trait PersistentCache: Send + Sync + 'static {
  /// Read bytes for a key. Returns `None` on cache miss.
  fn get(&self, key: CacheKey<'_>) -> impl Future<Output = Option<Vec<u8>>> + Send;

  /// Store bytes for a key. New versions land in new filenames.
  fn put(&self, key: CacheKey<'_>, bytes: Vec<u8>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Configuration knobs for [`FilesystemCache`].
#[derive(Debug, Clone)]
pub struct FilesystemCacheConfig {
  /// Maximum total bytes the cache may occupy. LRU evicts when exceeded.
  pub max_bytes: u64
}

impl Default for FilesystemCacheConfig {
  fn default() -> Self {
    Self {
      max_bytes: DEFAULT_MAX_BYTES
    }
  }
}

impl FilesystemCacheConfig {
  /// Read configuration from environment variables.
  ///
  /// Recognised variables:
  /// - `OMNIFUSE_CACHE_MAX_BYTES` — total byte budget. Invalid values fall back
  ///   to the default and emit a warning.
  #[must_use]
  pub fn from_env() -> Self {
    let mut cfg = Self::default();
    if let Some(raw) = std::env::var_os("OMNIFUSE_CACHE_MAX_BYTES") {
      match raw.to_string_lossy().parse::<u64>() {
        Ok(bytes) if bytes > 0 => cfg.max_bytes = bytes,
        _ => warn!(
          ?raw,
          "OMNIFUSE_CACHE_MAX_BYTES is not a positive integer; using default"
        )
      }
    }
    cfg
  }
}

/// Filesystem-backed persistent cache.
#[derive(Debug)]
pub struct FilesystemCache {
  /// Versioned root, i.e. `$root/v1`.
  root: PathBuf,
  config: FilesystemCacheConfig,
  put_counter: AtomicU64,
  eviction_in_flight: AtomicBool
}

impl FilesystemCache {
  /// Open or create a cache rooted at `root`.
  ///
  /// On schema mismatch the directory is wiped and rebuilt. On open the cache
  /// also kicks off a best-effort eviction pass to bring on-disk usage under
  /// the configured byte budget (for example after a budget reduction or a
  /// crashed previous run).
  ///
  /// # Errors
  ///
  /// Returns an error if the cache root cannot be created or the schema marker
  /// cannot be written.
  pub fn open(root: impl Into<PathBuf>, config: FilesystemCacheConfig) -> anyhow::Result<Arc<Self>> {
    let base = root.into();
    ensure_schema(&base)?;
    let cache = Arc::new(Self {
      root: base.join(SCHEMA_VERSION),
      config,
      put_counter: AtomicU64::new(0),
      eviction_in_flight: AtomicBool::new(false)
    });
    fs::create_dir_all(&cache.root)?;
    cache.spawn_eviction_if_needed();
    Ok(cache)
  }

  /// Cache root including the schema version (`$root/v1`).
  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Configured byte budget.
  #[must_use]
  pub const fn max_bytes(&self) -> u64 {
    self.config.max_bytes
  }

  fn blob_path(&self, key: CacheKey<'_>) -> PathBuf {
    let path_hash = hex_sha256(key.path.as_bytes());
    let version_hash = hex_sha256(key.version.as_bytes());
    let mut buf = self.root.clone();
    buf.push(key.instance.as_str());
    buf.push(&path_hash[..2]);
    buf.push(&path_hash);
    buf.push(&version_hash);
    buf
  }

  fn spawn_eviction_if_needed(self: &Arc<Self>) {
    if self
      .eviction_in_flight
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_err()
    {
      return;
    }
    let this = Arc::clone(self);
    let run = move || {
      if let Err(error) = this.evict_to_budget() {
        warn!(error = %error, "cache eviction failed");
      }
      this.eviction_in_flight.store(false, Ordering::SeqCst);
    };
    // When called from inside a Tokio runtime, run eviction off the async pool so
    // the put path stays responsive. When called from sync code (e.g. cache open
    // during process startup), run inline — eviction is best-effort and a fresh
    // open is the only sync path that triggers it.
    if tokio::runtime::Handle::try_current().is_ok() {
      let handle: JoinHandle<()> = tokio::task::spawn_blocking(run);
      drop(handle);
    } else {
      run();
    }
  }

  /// Walk the cache and evict by mtime until under the byte budget.
  ///
  /// Public for tests and the eventual daemon; backends should not call it
  /// directly — the cache triggers eviction automatically.
  ///
  /// # Errors
  ///
  /// Returns IO errors from walking or removing files. Individual file errors
  /// are logged and skipped.
  pub fn evict_to_budget(&self) -> anyhow::Result<()> {
    let mut files = Vec::new();
    let mut total: u64 = 0;
    collect_blobs(&self.root, &mut files, &mut total)?;

    if total <= self.config.max_bytes {
      trace!(total, budget = self.config.max_bytes, "cache under budget");
      return Ok(());
    }

    files.sort_by_key(|entry| entry.mtime);
    let mut over = total.saturating_sub(self.config.max_bytes);
    for entry in files {
      if over == 0 {
        break;
      }
      match fs::remove_file(&entry.path) {
        Ok(()) => {
          debug!(path = %entry.path.display(), size = entry.size, "cache blob evicted");
          over = over.saturating_sub(entry.size);
        }
        Err(error) => warn!(path = %entry.path.display(), error = %error, "cache eviction skipped")
      }
    }
    Ok(())
  }
}

impl PersistentCache for Arc<FilesystemCache> {
  async fn get(&self, key: CacheKey<'_>) -> Option<Vec<u8>> {
    let path = self.blob_path(key);
    let read = tokio::task::spawn_blocking(move || read_blob_and_touch(&path))
      .await
      .ok()?;
    match read {
      Ok(Some(bytes)) => {
        trace!(path = %key.path, "cache hit");
        Some(bytes)
      }
      Ok(None) => {
        trace!(path = %key.path, "cache miss");
        None
      }
      Err(error) => {
        warn!(error = %error, path = %key.path, "cache get failed");
        None
      }
    }
  }

  async fn put(&self, key: CacheKey<'_>, bytes: Vec<u8>) -> anyhow::Result<()> {
    let path = self.blob_path(key);
    let cache = Self::clone(self);
    let write_result = tokio::task::spawn_blocking(move || write_blob_atomic(&path, &bytes)).await?;
    write_result?;
    let count = cache.put_counter.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    if count % EVICTION_PUT_INTERVAL == 0 {
      cache.spawn_eviction_if_needed();
    }
    Ok(())
  }
}

fn ensure_schema(root: &Path) -> anyhow::Result<()> {
  fs::create_dir_all(root)?;
  let marker = root.join(SCHEMA_MARKER_FILE);
  match fs::read_to_string(&marker) {
    Ok(existing) if existing.trim() == SCHEMA_VERSION => Ok(()),
    Ok(other) => {
      warn!(
        marker = %marker.display(),
        existing = %other.trim(),
        expected = SCHEMA_VERSION,
        "cache schema mismatch — wiping cache"
      );
      reset_cache_root(root)?;
      fs::write(&marker, SCHEMA_VERSION)?;
      Ok(())
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      reset_cache_root(root)?;
      fs::write(&marker, SCHEMA_VERSION)?;
      Ok(())
    }
    Err(error) => Err(error.into())
  }
}

fn reset_cache_root(root: &Path) -> io::Result<()> {
  if !root.exists() {
    fs::create_dir_all(root)?;
    return Ok(());
  }
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let path = entry.path();
    if path.file_name().is_some_and(|name| name == SCHEMA_MARKER_FILE) {
      continue;
    }
    if entry.file_type()?.is_dir() {
      fs::remove_dir_all(&path)?;
    } else {
      fs::remove_file(&path)?;
    }
  }
  Ok(())
}

fn hex_sha256(input: &[u8]) -> String {
  let digest = Sha256::digest(input);
  let mut hex = String::with_capacity(digest.len() * 2);
  for byte in digest {
    use std::fmt::Write as _;
    let _ = write!(hex, "{byte:02x}");
  }
  hex
}

fn read_blob_and_touch(path: &Path) -> io::Result<Option<Vec<u8>>> {
  match fs::read(path) {
    Ok(bytes) => {
      // Best-effort mtime touch so LRU reflects use. Ignore failures: a missed
      // touch only affects eviction ordering.
      let _ = touch_mtime(path);
      Ok(Some(bytes))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error)
  }
}

fn write_blob_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  if path.exists() {
    // Same (path, version) already cached — keep the existing file. We still
    // touch mtime so freshly-requested content moves to the end of the LRU.
    let _ = touch_mtime(path);
    return Ok(());
  }

  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }

  let pid = std::process::id();
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.subsec_nanos())
    .unwrap_or_default();
  let tmp_name = format!(
    "{}.tmp.{pid}.{nanos:08x}",
    path.file_name().and_then(|name| name.to_str()).unwrap_or("blob")
  );
  let tmp_path = path.with_file_name(tmp_name);
  fs::write(&tmp_path, bytes)?;

  match fs::rename(&tmp_path, path) {
    Ok(()) => Ok(()),
    Err(error) => {
      let _ = fs::remove_file(&tmp_path);
      Err(error)
    }
  }
}

fn touch_mtime(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    let now = SystemTime::now();
    let file = fs::OpenOptions::new().write(true).custom_flags(0).open(path)?;
    file.set_modified(now)
  }
  #[cfg(not(unix))]
  {
    let now = SystemTime::now();
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(now)
  }
}

#[derive(Debug)]
struct BlobEntry {
  path: PathBuf,
  mtime: SystemTime,
  size: u64
}

fn collect_blobs(root: &Path, out: &mut Vec<BlobEntry>, total: &mut u64) -> io::Result<()> {
  if !root.exists() {
    return Ok(());
  }
  for entry in fs::read_dir(root)? {
    let Ok(entry) = entry else {
      continue;
    };
    let path = entry.path();
    let Ok(file_type) = entry.file_type() else {
      continue;
    };
    if file_type.is_dir() {
      collect_blobs(&path, out, total)?;
      continue;
    }
    if !file_type.is_file() {
      continue;
    }
    let name = entry.file_name();
    let name_str = name.to_string_lossy();
    if name_str == SCHEMA_MARKER_FILE || name_str.contains(".tmp.") {
      continue;
    }
    let Ok(meta) = entry.metadata() else {
      continue;
    };
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let size = meta.len();
    *total = total.saturating_add(size);
    out.push(BlobEntry { path, mtime, size });
  }
  Ok(())
}

#[cfg(test)]
#[allow(
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::panic,
  clippy::cast_possible_truncation
)]
mod tests {
  use std::{thread::sleep, time::Duration};

  use tempfile::TempDir;

  use super::*;

  fn make_cache(dir: &Path, max_bytes: u64) -> Arc<FilesystemCache> {
    FilesystemCache::open(dir, FilesystemCacheConfig { max_bytes }).expect("open cache")
  }

  #[test]
  fn instance_hash_stable_for_same_parts() {
    let a = InstanceHash::from_parts(&["s3", "bucket", "us-east-1", ""]);
    let b = InstanceHash::from_parts(&["s3", "bucket", "us-east-1", ""]);
    assert_eq!(a.as_str(), b.as_str());
    assert_eq!(a.as_str().len(), 64); // sha256 hex
  }

  #[test]
  fn instance_hash_distinguishes_part_boundaries() {
    // "ab" + "c" vs "a" + "bc": without a separator these would collide.
    let left = InstanceHash::from_parts(&["ab", "c"]);
    let right = InstanceHash::from_parts(&["a", "bc"]);
    assert_ne!(left.as_str(), right.as_str());
  }

  #[tokio::test]
  async fn put_then_get_returns_bytes() {
    let dir = TempDir::new().expect("tempdir");
    let cache = make_cache(dir.path(), 1024 * 1024);
    let inst = InstanceHash::from_parts(&["test", "one"]);

    cache
      .put(
        CacheKey {
          instance: &inst,
          path: "objects/file.txt",
          version: "etag-1"
        },
        b"hello".to_vec()
      )
      .await
      .expect("put");

    let got = cache
      .get(CacheKey {
        instance: &inst,
        path: "objects/file.txt",
        version: "etag-1"
      })
      .await;
    assert_eq!(got.as_deref(), Some(&b"hello"[..]));
  }

  #[tokio::test]
  async fn get_returns_none_on_miss() {
    let dir = TempDir::new().expect("tempdir");
    let cache = make_cache(dir.path(), 1024 * 1024);
    let inst = InstanceHash::from_parts(&["test"]);

    let got = cache
      .get(CacheKey {
        instance: &inst,
        path: "absent",
        version: "v"
      })
      .await;
    assert!(got.is_none());
  }

  #[tokio::test]
  async fn different_versions_coexist() {
    let dir = TempDir::new().expect("tempdir");
    let cache = make_cache(dir.path(), 1024 * 1024);
    let inst = InstanceHash::from_parts(&["test"]);

    cache
      .put(
        CacheKey {
          instance: &inst,
          path: "obj",
          version: "v1"
        },
        b"one".to_vec()
      )
      .await
      .expect("put v1");
    cache
      .put(
        CacheKey {
          instance: &inst,
          path: "obj",
          version: "v2"
        },
        b"two".to_vec()
      )
      .await
      .expect("put v2");

    assert_eq!(
      cache
        .get(CacheKey {
          instance: &inst,
          path: "obj",
          version: "v1"
        })
        .await
        .as_deref(),
      Some(&b"one"[..])
    );
    assert_eq!(
      cache
        .get(CacheKey {
          instance: &inst,
          path: "obj",
          version: "v2"
        })
        .await
        .as_deref(),
      Some(&b"two"[..])
    );
  }

  #[tokio::test]
  async fn concurrent_puts_same_key_do_not_corrupt() {
    let dir = TempDir::new().expect("tempdir");
    let cache = make_cache(dir.path(), 1024 * 1024);
    let inst = InstanceHash::from_parts(&["concurrent"]);

    let mut handles = Vec::new();
    for _ in 0..16 {
      let cache = Arc::clone(&cache);
      let inst = inst.clone();
      handles.push(tokio::spawn(async move {
        cache
          .put(
            CacheKey {
              instance: &inst,
              path: "race",
              version: "same"
            },
            b"identical".to_vec()
          )
          .await
      }));
    }
    for h in handles {
      h.await.expect("join").expect("put");
    }

    let got = cache
      .get(CacheKey {
        instance: &inst,
        path: "race",
        version: "same"
      })
      .await;
    assert_eq!(got.as_deref(), Some(&b"identical"[..]));
  }

  #[test]
  fn schema_mismatch_resets_cache() {
    let dir = TempDir::new().expect("tempdir");

    // Seed with a stale schema marker and a stray file.
    fs::write(dir.path().join(SCHEMA_MARKER_FILE), "v0").expect("seed marker");
    fs::write(dir.path().join("stale-data"), "junk").expect("seed stale");

    let cache = make_cache(dir.path(), 1024);
    drop(cache);

    let marker = fs::read_to_string(dir.path().join(SCHEMA_MARKER_FILE)).expect("read marker");
    assert_eq!(marker.trim(), SCHEMA_VERSION);
    assert!(
      !dir.path().join("stale-data").exists(),
      "stale data must be wiped on schema mismatch"
    );
  }

  #[test]
  fn evict_removes_oldest_until_under_budget() {
    let dir = TempDir::new().expect("tempdir");
    // Tiny budget so two blobs already exceed it.
    let cache = make_cache(dir.path(), 5);
    let inst = InstanceHash::from_parts(&["evict"]);

    // Write blob 1 manually so we control mtime ordering.
    let key_a = CacheKey {
      instance: &inst,
      path: "a",
      version: "v"
    };
    let key_b = CacheKey {
      instance: &inst,
      path: "b",
      version: "v"
    };

    let blob_a = cache.blob_path(key_a);
    let blob_b = cache.blob_path(key_b);
    fs::create_dir_all(blob_a.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(blob_b.parent().expect("parent")).expect("mkdir");
    fs::write(&blob_a, b"AAAA").expect("write a");
    sleep(Duration::from_millis(50));
    fs::write(&blob_b, b"BBBB").expect("write b");

    cache.evict_to_budget().expect("evict");

    // The older blob (a) should be gone, the newer one (b) should remain.
    assert!(!blob_a.exists(), "oldest blob must be evicted");
    assert!(blob_b.exists(), "newest blob must survive eviction");
  }
}
