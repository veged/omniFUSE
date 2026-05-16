# VFS landscape — product opportunities (2026 Q2)

Internal working notes. Synthesises:

- the May 2026 cross-platform VFS frameworks research
  (Rust crates, mount drivers, FSKit shift),
- the alternatives audit done while drafting the README "Alternatives"
  section (open-source and commercial mount tools).

Captures opportunities the current landscape opens for omniFUSE, with
rough sizing. Not for publication.

## 1. Mount-driver layer

### macFUSE 5 FSKit backend (macOS 26)

macFUSE 5.x ships an FSKit backend that runs entirely in user space on
macOS 26, no kext, no reboot for permission grant.

- **Why it matters.** The single largest UX papercut for our macOS
  users today is the kext-approval dance. FSKit removes it.
- **Cost.** Medium. `rfuse3` may not speak FSKit yet — track upstream;
  if backlog grows, consider an in-house adapter.
- **Decision.** Track `rfuse3` FSKit support. Document the macOS 26
  story explicitly in `of check` once available.

### NFS fallback (fernfs / nfsserve)

Both crates are functional Rust NFSv3 server implementations. Loopback
mount via the OS NFS client bypasses FUSE/WinFsp entirely.

- **Why it matters.** Containers, restricted Windows hosts, and CI
  runners where FUSE/WinFsp install is impossible — currently we tell
  these users "sorry".
- **Cost.** Medium-large (a third mount transport, separate from
  `unifuse`'s FUSE+WinFsp pair).
- **Decision.** Defer. Reconsider when there are concrete user reports
  of FUSE-unfriendly environments.

### S3 throughput via S3 CRT (mountpoint-s3 pattern)

`mountpoint-s3` uses AWS Common Runtime for parallelised range GETs and
hides multi-part GET behind a streaming interface.

- **Why it matters.** Large-file throughput is a known weak point of
  generic object stores. Our cap right now is whatever OpenDAL gives us.
- **Cost.** Medium. An alternative S3 transport in `omnifuse-s3`,
  feature-flagged.
- **Decision.** Revisit only when a real workload shows user-visible
  lag. Persistent cache (planned) will mask most cold-read pain.

### WinFsp 2.2 CVE-2026-3006

Pre-2.2 WinFsp has a CVE. We should set a minimum WinFsp version and
surface it via `of check`.

- **Cost.** Trivial.
- **Decision.** Do.

### libfuse-fs vs rfuse3

`libfuse-fs` reached production in Scorpio/mega and was presented at
Oxidize Conf 2026. We currently use `rfuse3`. No urgent reason to
switch — but if `rfuse3` becomes a bottleneck (FSKit support, async
quirks), `libfuse-fs` is the obvious fallback.

- **Decision.** Track; no action.

## 2. Backend coverage

### Quantitative gap vs rclone (70+ vs 3)

We will never catch up on count. Our win is the orthogonal axis:
concurrent-safe writes, real OS mount, agent UX, one Rust binary. Don't
fight rclone on width.

- **Next four backends, in rough priority order.** WebDAV (closest
  cousin of S3 by shape; covers Nextcloud / ownCloud); SFTP/SSH
  (different niche, broad demand); GitHub Issues/PRs as files (agent
  use case, leverages git backend); Notion (popular but heavier auth +
  API rate-limit story).
- **Decision.** Lock the order via separate plan documents before
  starting.

### Many-small-objects on S3 (GeeseFS patterns)

GeeseFS performs aggressive large-batch listing and prefetch — a
significant edge on S3 buckets with tens of thousands of tiny objects.

- **Relevance.** Overlaps with M3 of the persistent-cache plan
  (S3 ↔ cache wiring).
- **Decision.** Carry into the M3 design.

### Huge-repo git VFS (EdenFS, Scorpio, VFS for Git)

Pattern: lazy-load file content from a content-addressable store; only
materialise on first read. Unlocks billion-line monorepos.

- **Cost.** Very large.
- **Decision.** Out of scope for V1. Revisit on signal.

## 3. UX

### Free GUI in a paid-tier market

Mountain Duck (~$39 one-time), ExpanDrive ($99/mo business),
CloudMounter (~$30–45 or subscription) all charge for commodity
multi-cloud GUI mounting. Our Tauri GUI is the differentiator — but
needs table-stakes feature parity to compete on this axis:

- Multi-mount manager view
- Tray icon + autostart
- Per-mount credential storage tied to system keyring
- "Reconnect on network change" semantics
- Status badge per mount (synced / dirty / error)

- **Decision.** Separate plan after persistent cache lands; daemon
  enables most of this naturally.

### RClone Manager lessons

Rust-based, GTK-inspired, cross-platform shell around rclone. Picks
worth stealing for our GUI: tray, multi-profile, remote rclone
instances (analogue: remote-`of`-daemon control).

- **Decision.** Input for the Tauri GUI roadmap, not a separate plan.

### Mirage's framework adapters

Mirage ships adapters for OpenAI Agents SDK, LangChain, Vercel AI SDK,
Pydantic AI, CAMEL, etc. Our model is different — agents speak shell,
no SDK — but the *documentation* surface should match:

- Expand `for-<tool>.md` skill tails as new agent frameworks emerge.
- Add a "Cookbook" page covering: Claude Code (`--add-dir`), OpenAI
  Agents SDK shell tool, LangChain `ShellTool`, Vercel AI SDK
  generateText with execution, OpenHands.

- **Decision.** Low-cost. Add adapters as usage signal arrives, not
  pre-emptively.

## 4. Distribution

### `unifuse` on crates.io

The 2026 research conflated our `unifuse` with a different crate by
user `Ivanbeethoven`. Either we are not on crates.io yet, our metadata
isn't surfacing, or someone else holds the name.

- **Action.** Confirm crates.io status (see if `unifuse` resolves to
  our owner). If absent, publish 0.2.0 with explicit description and
  repository link. If the name is taken, rename and re-publish.
- **Decision.** Investigate before any further public visibility push.

### Trademark / name collision

Tied to the above. The other `unifuse` is described as "FUSE-based
virtual filesystem with Antares overlay for monorepo builds" — a
different product. Either we share the name on crates.io (problem) or
the research source confused us (no problem). Verify.

## 5. Things we should not chase

Recorded so they don't sneak in:

- **MCP server for mount operations.** The mount is the interface;
  MCP would duplicate it. Skill manual is the right shape.
- **Embedded SDK in another language.** Python / Node bindings via
  PyO3 / napi are tempting but don't match our "agents speak shell"
  thesis.
- **Cross-host cache sharing.** Local-only is the right scope.
- **Backends that are not really files** (Slack messages, MongoDB
  rows). Mirage covers that turf and pays the abstraction tax. We
  stick to things that are naturally files or naturally tree-shaped
  text.

## 6. Open questions

- Does our `concurrent-safe writes` story have a real-world demo that
  we can show alongside the README? A 90-second screencast of two
  editors saving the same git-backed file at once, showing both
  changes survive, would be the single highest-leverage marketing
  asset.
- Is there a registry of agent-friendly tools we should be listed in
  (e.g. an OpenAI / Anthropic awesome list)? Skill manual gives us a
  unique check-mark vs nearly everyone else in the table.
- macOS 26 timing: when does FSKit-only become a hard requirement?
  Plan the migration ahead of forced kext removal.
