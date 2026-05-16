# omniFUSE — skill manual for AI agents

omniFUSE mounts remote backends (git repositories, wikis, more to come) as
real local directories via FUSE (Linux/macOS) or WinFsp (Windows). Any tool
that opens files — `cat`, `grep`, `python`, your editor — works against the
mount with no special wiring.

## Mount lifecycle

1. The user (or you, if authorized) runs
   `of mount <backend> <args...> <mountpoint>`.
2. omniFUSE keeps the mount alive in the foreground. Send SIGTERM
   (Ctrl-C) to unmount cleanly.
3. While mounted, reads and writes to files under `<mountpoint>` are
   routed to the backend with caching, write debouncing, and conflict
   resolution.

## Where credentials live

Credentials are read by `of` at mount time, on the host that runs `of`.
Reads and writes from inside a sandbox never see them — only the mount.

Common environment variables:

- `OMNIFUSE_WIKI_TOKEN` — wiki OAuth/IAM token.
- `OMNIFUSE_WIKI_ORG_ID` — Yandex 360 organization ID.

For git: the standard `git` credentials on the host (SSH agent, `~/.netrc`,
credential helper) are used.

## Safety properties

- File operations are POSIX. Same semantics as a local disk.
- `unlink` on a tracked file removes it and stages a backend-side delete.
- Concurrent edits from a human are not silently overwritten —
  see the per-backend conflict notes below.
