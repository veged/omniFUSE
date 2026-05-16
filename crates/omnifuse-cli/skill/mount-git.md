## `of mount git`

```
of mount git <SOURCE> <MOUNTPOINT> [--branch=main]
                                   [--poll-interval=30]
                                   [--allow-other]
                                   [--read-only]
```

- `SOURCE` — remote URL or path to a local repository.
- `MOUNTPOINT` — directory where the repo will appear.
- `--branch` — branch to track (default `main`).
- `--poll-interval` — how often to fetch from origin, in seconds.
- `--read-only` — disable writes back to the repo.

### What files appear

Every file in the working tree is visible. Files matching `.gitignore`
are filtered out (not shown in `ls`).

### Sync semantics

- Reads are served from the local working copy.
- Writes are buffered in memory and debounced; on flush they become real
  commits pushed to `origin/<branch>`.
- A background poller fetches every `--poll-interval` seconds.
  Fast-forward merges are applied silently; non-fast-forward merges may
  create a merge commit; conflicts leave standard git conflict markers
  in the affected files.

### Conflict behavior

If a push is rejected (someone else pushed first), omniFUSE pulls and
retries automatically. If the merge produces conflict markers, the
affected file will contain the usual `<<<<<<<`, `=======`, `>>>>>>>`
blocks. Treat such files as you would in any git working tree — resolve
and save, and the next sync will commit the resolution.
