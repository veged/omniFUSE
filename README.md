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

See [CONTRIBUTING.md](CONTRIBUTING.md) for architecture, building, testing,
code style, and how to add a new backend.

## License

MIT
