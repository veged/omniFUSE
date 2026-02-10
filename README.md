# omniFUSE

<img src="./omniFUSE.png" width="200" height="200" alt="omniFUSE logo">

Universal virtual filesystem — mount git repos, wikis, and cloud storage as local directories.

Edit files with your favorite editor, and **omniFUSE** syncs changes automatically.

## Installation

### Prerequisites

| Platform | FUSE Driver | Install |
|----------|------------|---------|
| macOS | macFUSE | `brew install macfuse` |
| Linux | libfuse3 | `sudo apt install libfuse3-dev fuse3` |
| Windows | WinFsp | `choco install winfsp` *(planned)* |

### From source

```bash
cargo install --path crates/omnifuse-cli
```

## Usage

### Mount a git repository

```bash
# Remote repo
of mount git https://github.com/user/repo ~/mnt/repo

# Local repo
of mount git /path/to/repo ~/mnt/repo

# Specific branch
of mount git https://github.com/user/repo ~/mnt/repo --branch develop
```

Files you edit in `~/mnt/repo` are auto-committed and pushed.
Remote changes are pulled periodically.

### Mount a wiki

```bash
of mount wiki https://wiki.example.com my/project ~/mnt/wiki --auth TOKEN

# Auth token can also be set via environment variable
export OMNIFUSE_WIKI_TOKEN=your-token
of mount wiki https://wiki.example.com my/project ~/mnt/wiki
```

Wiki pages appear as `.md` files. Edits are synced back via the wiki API
with three-way merge for conflict resolution.

### Other commands

```bash
of check        # Verify FUSE is installed
of gen-config   # Print example TOML config
```

### Desktop GUI

**omniFUSE** includes a Tauri-based GUI with git and wiki backend support,
real-time sync logs, and a system folder picker.

```bash
cd crates/omnifuse-gui
cd web && npm install && cd ..
cargo tauri dev
```

---

## Development

### Architecture

```
┌─────────────┐     ┌───────────────┐     ┌──────────┐
│ FUSE/WinFsp │ ──► │  OmniFuseVfs  │ ──► │ Backend  │
│  (unifuse)  │     │ (files+cache) │     │(git/wiki)│
└─────────────┘     └──────┬────────┘     └──────────┘
                           │
                    ┌──────▼────────┐
                    │  SyncEngine   │
                    │(debounce+poll)│
                    └───────────────┘
```

### Crates

| Crate | Description |
|-------|-------------|
| [`unifuse`](crates/unifuse/) | Cross-platform async FUSE abstraction (rfuse3/WinFsp) |
| [`omnifuse-core`](crates/omnifuse-core/) | VFS kernel: `Backend` trait, `SyncEngine`, `FileBufferManager` |
| [`omnifuse-git`](crates/omnifuse-git/) | Git backend (clone, commit, push/pull, .gitignore filtering) |
| [`omnifuse-wiki`](crates/omnifuse-wiki/) | Wiki backend (HTTP API, three-way merge via diffy) |
| [`omnifuse-cli`](crates/omnifuse-cli/) | CLI binary `of` |
| [`omnifuse-gui`](crates/omnifuse-gui/) | Desktop GUI (Tauri + React) |

### Building

```bash
# Requires Rust nightly
rustup toolchain install nightly

# Build all crates
cargo build --workspace

# Lint
cargo clippy --workspace
```

### Testing

All tests (~319) run by default:

```bash
cargo test --workspace
```

#### Skipping tests in environments without dependencies

If a runtime lacks certain dependencies, explicitly exclude specific test groups:

```bash
# CI without FUSE — skip FUSE mount tests
cargo test --workspace -- --skip fuse_mount

# CI without Wiki API credentials — skip real API tests
cargo test --workspace -- --skip real_api

# Combined skip
cargo test --workspace -- --skip fuse_mount --skip real_api
```

#### Real API tests (Wiki)

Tests in `real_api_tests.rs` hit the real Wiki API.
Without env variables the tests do an early return (they don't fail):

```bash
export OMNIFUSE_WIKI_URL=https://wiki.example.com
export OMNIFUSE_WIKI_TOKEN=your-token
export OMNIFUSE_WIKI_ROOT_SLUG=my/project
cargo test -p omnifuse-wiki --test real_api_tests
```

#### Running individual crates

```bash
cargo test -p omnifuse-core       # VFS, SyncEngine, buffer, config (131 tests)
cargo test -p omnifuse-core --test fuse_mount_tests  # FUSE mount (30 tests)
cargo test -p omnifuse-git        # Git engine, ops, filter (53 + 8 integration)
cargo test -p omnifuse-wiki       # Wiki merge, meta, models, client, backend (67 tests)
cargo test -p omnifuse-cli        # CLI E2E (11 tests)
cargo test -p unifuse             # Types, inode (19 tests)
```

### Design decisions

- **Async-first** — all traits and APIs are async (rfuse3, Backend, UniFuseFilesystem)
- **RPITIT** for `Backend` trait (no `async-trait` macro)
- **`unsafe_code = "forbid"`** — no unsafe anywhere
- **`unwrap_used = "deny"`** — `?` operator only, no panics in library code
- **Three-way merge** via `diffy` for wiki conflict resolution
- **Git merge** delegated to native git (push → pull → merge)

### Adding a new backend

Implement the `Backend` trait from `omnifuse-core`:

```rust
#[trait_variant::make(Send)]
pub trait Backend: Send + Sync + 'static {
    async fn init(&self, local_dir: &Path) -> anyhow::Result<InitResult>;
    async fn sync(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult>;
    async fn poll_remote(&self) -> anyhow::Result<Vec<RemoteChange>>;
    async fn apply_remote(&self, changes: Vec<RemoteChange>) -> anyhow::Result<()>;
    fn should_track(&self, path: &Path) -> bool;
    fn poll_interval(&self) -> Duration;
    async fn is_online(&self) -> bool;
    fn name(&self) -> &'static str;
}
```

Then wire it into `omnifuse-cli` and/or `omnifuse-gui`.

## License

MIT
