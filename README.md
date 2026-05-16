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

# Or an S3 data lake: agents read/write objects through normal POSIX paths
of mount s3 my-bucket /workspace/data --region us-east-1 --prefix raw/

# The agent uses whatever it already knows
grep -r "TODO" /workspace/repo
cat /workspace/repo/src/main.rs
echo "fix" >> /workspace/repo/notes.md
jq '.events[]' /workspace/data/2026-05-15/ingest.json
```

What this gives you, beyond a generic shared volume:

- **Versioned memory for free.** On the git backend, every save the agent makes
  becomes a real commit pushed to the remote. Recover with `git log`,
  `git revert`, `git diff` — the same tools humans already use.
- **Concurrent editing with humans.** All three backends accept simultaneous
  writes without locks. Git delegates to native git merge (fast-forward, merge
  commit, or conflict markers); wiki and S3 use three-way merge via `diffy`,
  with S3 layering compare-and-swap on top (`If-Match` / `If-None-Match`) so
  concurrent remote updates can never be silently overwritten. Binary objects
  on S3 surface as explicit conflicts instead of clobbering. No locks, no
  "agent took over the file" surprises.
- **Credentials stay outside the sandbox.** Mount runs on the host with your
  tokens. The agent sees a directory — it never touches the GitHub PAT, wiki
  OAuth token, or S3 access keys, even if its sandbox is compromised.
- **Drop-in for any agent framework.** Claude Code (`--add-dir`), OpenAI Agents
  SDK shell tool, LangChain `ShellTool`, self-hosted runners — all work
  unmodified because the mount looks like a local directory.

## Roadmap

- **More backends.** WebDAV (Nextcloud, ownCloud), SFTP/SSH, GitHub Issues/PRs
  as files, Notion, Confluence, GitLab Wiki. (S3-compatible storage — AWS S3,
  R2, B2, MinIO, Yandex Object Storage — is already shipped, see
  [Mount S3-compatible storage](#mount-s3-compatible-storage).)
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

## Alternatives

omniFUSE sits at the intersection of: a real OS-level mount (so any tool that
opens files just works), multiple backends in one binary, safe concurrent
writes with humans, and a first-class UX for AI agents. Adjacent tools, grouped
by approach:

### Open-source mounters

| Project | Kind | Backends | OS | License |
|---|---|---|---|---|
| [rclone](https://rclone.org) + `rclone mount` | Generic | 70+ (S3, GDrive, SFTP, …) | L / M / W | MIT |
| [JuiceFS](https://juicefs.com) Community | POSIX over object storage + metadata DB | S3 + Redis / Postgres / TiKV / … | L / M / W | Apache 2.0 |
| [mountpoint-s3](https://github.com/awslabs/mountpoint-s3) (AWS) | Single backend | S3 (sequential writes, no rename) | L | Apache 2.0 |
| [GeeseFS](https://github.com/yandex-cloud/geesefs) (Yandex) | Single backend | S3 (tuned for many small objects) | L / M | Apache-derived |
| [s3fs-fuse](https://github.com/s3fs-fuse/s3fs-fuse) | Single backend | S3 | L / M | GPL-2.0 |
| [sshfs](https://github.com/libfuse/sshfs) | Single backend | SSH / SFTP | L / M | GPL-2.0 |
| [davfs2](https://savannah.nongnu.org/projects/davfs2/) | Single backend | WebDAV | L | GPL-3.0 |
| [gitfs](https://github.com/presslabs/gitfs) (presslabs) | Git-as-FS | git repos | L / M | Apache 2.0 |
| [EdenFS](https://github.com/facebook/sapling) (Meta Sapling) | Source-control VFS | Sapling / Mercurial | L / M / W | GPL-2.0 |
| [Mirage](https://github.com/strukto-ai/mirage) (strukto-ai) | In-process VFS for AI agents | S3, GDrive, Slack, Gmail, … | L / M | Apache 2.0 |

### Commercial GUI mounters

| Project | Backends | OS | Price |
|---|---|---|---|
| [Mountain Duck](https://mountainduck.io/) | S3, GDrive, Dropbox, OneDrive, SharePoint, SMB, … | M / W | one-time ~$39 standalone; Mac App Store: subscription |
| [ExpanDrive](https://www.expandrive.com/) | S3, GDrive, OneDrive, SFTP, WebDAV, … | L / M / W | free ≤ 10 users; business ~$99/mo for 50 devices |
| [CloudMounter](https://cloudmounter.net/) | OneDrive, GDrive, S3, Dropbox, FTP / SFTP, WebDAV, … | M / W (+ Linux client) | one-time ~$30–45 standalone; Mac App Store: subscription |

### Where omniFUSE fits

If you need the broadest backend coverage today, **rclone** is the better
choice. If you live on S3 on Linux and want maximum throughput, **mountpoint-s3**
wins. If you want a polished end-user GUI and a credit card is fine, **Mountain
Duck** or **ExpanDrive** are honest answers.

omniFUSE differs in: a real OS-level mount in one Rust binary; git, wiki, and
S3 covered today with more coming; explicit conflict-safe writes per backend
(native git merge, three-way wiki merge via [`diffy`](https://github.com/bmwill/diffy),
S3 conditional writes via `If-Match` / `If-None-Match`); and an agent-oriented
manual via `of skill --for=<tool>` that ships in the binary.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for architecture, building, testing,
code style, and how to add a new backend.

## License

MIT
