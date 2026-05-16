# Plan: persistent cache and daemon

One project: a persistent on-disk cache shared between runs and between
concurrent mounts, plus a background daemon that owns active mounts,
shares the hot RAM tier, and serves `of list / status / unmount`.

## Goal

- Stop refetching the same bytes every time `of mount ...` starts.
- Make multiple concurrent mounts of the same instance share RAM.
- Give agents and humans a way to see what is currently mounted.

## Non-goals

- KV store / embedded database backing the cache.
- Schema migrations (drop on mismatch instead).
- Cache freshness logic inside the cache (backends own this).
- Mount auto-restart.
- Cross-host cache sharing.
- Credential persistence in the daemon beyond a single mount's lifetime.

## Recorded decisions

- **Unified key**: `(instance_hash, path, version_token) → bytes`. The
  cache is opaque about what `path` and `version_token` mean. Listings
  are just bytes under a path the backend chooses.
- **Backends own freshness**. The cache never asks "is this version
  current?" — it stores and returns bytes for a given `(path, version)`.
  For resources without a natural version (e.g. S3 listings), the
  backend synthesises a string like `ttl-fence:<unix-ts>` and bumps it
  on its own schedule.
- **Storage**: raw filesystem at `$XDG_CACHE_HOME/omnifuse/v1/...`. No
  index, no KV. LRU walks the tree using file mtime.
- **Eviction**: single global byte budget,
  `OMNIFUSE_CACHE_MAX_BYTES` (default 1 GiB), LRU by mtime.
- **Schema versioning**: `v1/`. On mismatch the whole `v1/` is dropped
  and rebuilt. No migration code.
- **Hot tier stays as it is.** `FileBufferManager` keeps its current
  shape; the persistent cache is a layer below it. Backends consult the
  persistent cache before going to remote.
- **Git does not use the cache trait.** Git's persistence is the
  working tree plus `.git/`: on subsequent mounts we fetch + checkout
  instead of cloning.
- **Daemon IPC**: Unix domain socket, newline-delimited JSON. No
  JSON-RPC framing.
- **Daemon is opt-in.** Foreground mount remains the default. When a
  daemon is running, `of mount ...` hands off to it.
- **`of list` / `of status` work without a daemon** in a degraded mode
  by scanning `/proc/self/mountinfo` (Linux) or `mount` output (macOS)
  for FUSE entries owned by us.

## Cache abstraction

```rust
pub struct InstanceHash(String);   // stable hash of normalised mount params

pub struct CacheKey<'a> {
    pub instance: &'a InstanceHash,
    pub path: &'a str,
    pub version: &'a str,
}

#[trait_variant::make(Send)]
pub trait PersistentCache: Send + Sync + 'static {
    async fn get(&self, key: CacheKey<'_>) -> Option<Bytes>;
    async fn put(&self, key: CacheKey<'_>, bytes: Bytes) -> anyhow::Result<()>;
}
```

Three methods are enough. Eviction is a background concern of the
implementation, not part of the trait.

### Filesystem layout

```
$XDG_CACHE_HOME/omnifuse/v1/
└── <instance_hash>/
    └── <sha256(path)[..2]>/
        └── <sha256(path)>/
            └── <sha256(version)>     # opaque bytes
```

Three-level fanout keeps directories small. Each blob file is
immutable once written: new versions land in new filenames, so reads
do not race writes. Eviction takes an exclusive flock on the instance
root before removing files.

Atomic put: write `<...>.tmp.<pid>.<rand>`, then `rename`.

### Eviction

Per-cache background task. Trigger: every 60 seconds, or after every
N (= 64) puts, whichever is sooner. Walk `v1/`, sum sizes, evict
least-recently-mtimed files until under the cap. Touch `mtime` on
every `get` hit so LRU reflects use.

## Daemon

### Process

Long-lived `of daemon` process. Owns:

- One `PersistentCache` instance.
- All hosted mounts (each runs as a tokio task).
- One UDS listener.

PID file: `$XDG_RUNTIME_DIR/omnifuse/daemon.pid`.
Socket: `$XDG_RUNTIME_DIR/omnifuse/daemon.sock` on Linux,
`$HOME/Library/Caches/omnifuse/daemon.sock` on macOS.

### Protocol

Newline-delimited JSON. One JSON object per line in each direction.
Every request has `method`, optional `params`, and a client-chosen
`id`. Every response echoes `id` and contains either `result` or
`error`. Schema version is a constant `"v": 1` field, used as a hard
gate (mismatch → response error, no negotiation).

Methods:

- `mount.git`, `mount.wiki`, `mount.s3` — params mirror the CLI
  arguments. Response returns when the mount is established or fails.
- `list` — returns active mounts with backend, instance, mountpoint,
  age.
- `status` — params: `mountpoint`. Returns dirty count, last sync
  timestamp, cache hit ratio.
- `unmount` — params: `mountpoint`. Returns when unmounted.

No streaming, no subscriptions in V1. If the daemon needs to push
something later, a `subscribe` method can be added without breaking
clients.

### CLI behaviour

- `of mount <backend> ...` — if `OMNIFUSE_NO_DAEMON=1` or socket
  missing or daemon unreachable: mount in the foreground as today.
  Otherwise: send `mount.<backend>`, return when the mount is ready.
- `of list` — daemon: `list`. No daemon: scan mount table.
- `of status <mp>` — daemon: `status`. No daemon: report whether the
  path is a FUSE mount of ours; no cache or dirty stats.
- `of unmount <mp>` — daemon: `unmount`. No daemon: `fusermount -u` on
  Linux, `umount` on macOS.
- `of daemon start` — fork, detach, write PID file. Idempotent: if
  already running, return success.
- `of daemon stop` — graceful shutdown: unmount all hosted mounts,
  remove PID file.
- `of daemon status` — running / not, PID, mount count.

### Hot tier sharing

Inside the daemon, mounts with the same `instance_hash` share a single
`FileBufferManager` instance. Implementation: a `dashmap`
`instance_hash → Arc<FileBufferManager>` owned by the daemon. In the
foreground (no-daemon) path each mount has its own manager as today —
that is the price of running standalone.

## Per-backend wiring

### S3 (`omnifuse-s3`)

- `version_token` for objects = ETag.
- `version_token` for listings = `ttl-fence:<unix-ts>` per prefix,
  bumped after any local write under that prefix and on a periodic
  TTL.
- Read path: backend HEAD's the object (cheap, one round trip), then
  asks the cache for `(path, etag)`. Hit → serve from cache. Miss →
  GET, then `put`.

### Wiki (`omnifuse-wiki`)

- `version_token` for pages = revision id.
- `version_token` for tree = `ttl-fence:<root>:<unix-ts>`, bumped on
  local edits and on a periodic TTL.

### Git (`omnifuse-git`)

- Persistent working tree per instance at
  `$XDG_CACHE_HOME/omnifuse/v1/git/<instance>/`.
- On mount: if the directory exists, is a valid working tree on the
  right branch, and has no uncommitted changes → fetch + checkout.
  Otherwise → fresh clone (log a warning when we discarded a stale
  worktree).
- Does not use the `PersistentCache` trait.

## Tests

- Unit:
  - `InstanceHash` is stable across runs for the same normalised
    inputs.
  - Atomic put under concurrent writers: no torn files, all visible
    versions readable.
  - LRU eviction removes the right files when over the cap.
  - Schema-version mismatch drops `v1/` and recreates it.
- Integration:
  - S3 against MinIO: two-run scenario, second mount issues zero GETs
    for unchanged objects (asserted via a request counter on the
    OpenDAL operator wrapper).
  - Wiki against the stub server: same shape, keyed on revision.
  - Git: second mount of the same repo reuses the working tree (no
    `clone` invocation in the trace).
  - Daemon: `of daemon start`, two mounts of the same S3 instance,
    `of list` shows both, RAM-tier hit counter shows shared reads.
  - No-daemon fallbacks: `of list` shows currently mounted paths even
    without a daemon.

## Milestones

All shipped together in this PR.

1. **Done.** `PersistentCache` trait + filesystem implementation + LRU
   eviction in `omnifuse-core`. Shipped as
   `omnifuse_core::{PersistentCache, FilesystemCache, InstanceHash, CacheKey}`.
2. **Done.** Daemon scaffolding in `omnifuse-app::daemon` — UDS server,
   newline-delimited JSON protocol with schema version `v=1`,
   `of daemon start/stop/status`, plus `of list/status/unmount` that
   transparently falls back to scanning the OS mount table when no
   daemon is running.
3. **Done.** S3 ↔ cache wiring. `S3Backend::with_cache` plus
   `S3Session::read_object_cached` route reads through the cache by
   `(instance, object_path, etag)`. Cached after successful PUT.
4. **Done.** Daemon hosts mounts: `of mount <backend> ...` opportunistically
   hands off to the running daemon, otherwise mounts in the foreground.
   `Daemon` pools `Arc<FileBufferManager>` keyed by `InstanceHash`, so
   concurrent mounts of the same instance share the hot tier.
5. **Done.** Wiki ↔ cache wiring. `WikiBackend::with_cache` plus
   `WikiPageSyncSession::read_page_cached` key by
   `(instance, slug, modified_at)`. Modified-at acts as the V1 revision
   token. Cache is also written after successful page updates.
6. **Done.** Git persistent working tree. `ensure_remote_repo` and the
   local-clone-into-cache path now validate the cached worktree (no
   in-progress merge/rebase/bisect, no uncommitted changes) before
   reusing it. A clean tree → fetch + checkout. A stale tree → wipe +
   re-clone with a warning. The `local_dir` was already keyed on
   `(source, branch)` so the cache is preserved across runs.

## Open questions to resolve during implementation

- Should eviction be per-instance or fully global? Start global, watch
  for one noisy instance starving others; tune if it shows up.
- What to do with cache when an instance is unmounted: keep on disk by
  default; opt-in `OMNIFUSE_CACHE_PURGE_ON_UNMOUNT=1` if a user wants
  ephemeral mounts.
- Should the daemon expose Prometheus metrics? Probably yes, but as a
  separate later step — not part of V1.
