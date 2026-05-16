# omniFUSE

<img src="./omniFUSE.png" width="200" height="200" alt="omniFUSE logo">

Universal virtual filesystem for AI and humans: mount git repos, wikis, and cloud storage as local directories

Edit files with your favorite editor — or let an AI agent do it through plain shell tools — and **omniFUSE** syncs changes automatically.

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
of mount wiki <BASE_URL> <ROOT_SLUG> <MOUNTPOINT> --auth TOKEN

# Auth token can also be set via environment variable
export OMNIFUSE_WIKI_TOKEN=your-token
of mount wiki <BASE_URL> <ROOT_SLUG> <MOUNTPOINT>
```

Wiki pages appear as `.md` files. Edits are synced back via the wiki API with three-way merge for conflict resolution.

#### Yandex Wiki

For Yandex 360 Wiki use the API host and an [OAuth token](https://yandex.ru/support/wiki/ru/api-ref/access):

```bash
# Yandex 360 (external organizations)
export OMNIFUSE_WIKI_TOKEN=your-oauth-token
of mount wiki https://api.wiki.yandex.net my/project ~/mnt/wiki --org-id YOUR_ORG_ID
```

> **Note:** use the API host (`api.wiki.yandex.net`), not the web UI host.

### Mount S3-compatible storage

OmniFuse mounts any S3-compatible bucket through OpenDAL. Text objects with
non-overlapping local and remote UTF-8 changes are merged automatically; binary
objects report conflicts when both sides change.

```bash
export OMNIFUSE_S3_ACCESS_KEY_ID=your-access-key
export OMNIFUSE_S3_SECRET_ACCESS_KEY=your-secret-key

of mount s3 my-bucket ~/mnt/storage \
  --endpoint https://s3.amazonaws.com \
  --region us-east-1 \
  --prefix project
```

For Cloudflare R2, point at the account endpoint and use `--region auto`:

```bash
of mount s3 my-bucket ~/mnt/r2 \
  --endpoint https://ACCOUNT_ID.r2.cloudflarestorage.com \
  --region auto
```

The backend refuses providers that do not advertise conditional writes
(`write_with_if_match` / `write_with_if_not_exists`) — there is no degraded
unsafe-write mode in V1.

### Other commands

```bash
of check        # Verify FUSE is installed
of gen-config   # Print example TOML config
```

### Desktop GUI

**omniFUSE** includes a Tauri-based GUI with git, wiki, and S3-compatible
backend support, real-time sync logs, and a system folder picker.

```bash
cd crates/omnifuse-gui
cd web && npm install && cd ..
cargo tauri dev
```

---

## For AI agents

Because **omniFUSE** mounts at the OS level via FUSE/WinFsp, any agent that can
spawn a shell, read files, or open paths already speaks its language —
no SDK, no plugin, no provider-specific tool wiring.

```bash
# Mount once on the host (or inside a long-lived sandbox)
of mount git https://github.com/user/repo /workspace/repo

# The agent uses whatever it already knows
grep -r "TODO" /workspace/repo
cat /workspace/repo/src/main.rs
echo "fix" >> /workspace/repo/notes.md
```

What this gives you, beyond a generic shared volume:

- **Versioned memory for free.** On the git backend, every save the agent makes
  becomes a real commit pushed to the remote. Recover with `git log`,
  `git revert`, `git diff` — the same tools humans already use.
- **Concurrent editing with humans.** Both backends accept simultaneous writes.
  Git delegates to native git merge (fast-forward, merge commit, or conflict
  markers); wiki uses three-way merge via `diffy`. No locks, no "agent took
  over the file" surprises.
- **Credentials stay outside the sandbox.** Mount runs on the host with your
  tokens. The agent sees a directory — it never touches the GitHub PAT or the
  wiki OAuth token, even if its sandbox is compromised.
- **Drop-in for any agent framework.** Claude Code (`--add-dir`), OpenAI Agents
  SDK shell tool, LangChain `ShellTool`, self-hosted runners — all work
  unmodified because the mount looks like a local directory.

## Roadmap

- **More backends.** S3-compatible (AWS S3, R2, B2, MinIO, Yandex Object
  Storage), WebDAV (Nextcloud, ownCloud), SFTP/SSH, GitHub Issues/PRs as files,
  Notion, Confluence, GitLab Wiki.
- **Persistent cache between runs.** Reuse already-fetched data for the same
  remote resource across CLI invocations.
- **Index/metadata cache.** Separate listing cache for slow backends so
  `readdir` stays snappy on S3, GDrive, etc.
- **Workspace snapshots.** Capture and restore the state of a mount —
  particularly useful as a rollback point for agent runs.
- **Daemon mode.** Single `of` daemon owning all active mounts, with
  `of list` / `of status` and shared cache between processes.
- **`of skill` / `of <cmd> --skill`.** Agent-oriented help format, symmetric
  to `--help` (works as both a subcommand and a flag on every command);
  optional `--for=<tool>` to add tool-specific guidance.

Contributions and proposals welcome — open an issue before starting on a new
backend so we can agree on the shape.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for architecture, building, testing,
code style, and how to add a new backend.

## License

MIT
