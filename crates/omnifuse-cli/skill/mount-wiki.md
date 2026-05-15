## `of mount wiki`

```
of mount wiki <BASE_URL> <ROOT_SLUG> <MOUNTPOINT> --auth <TOKEN>
                                                  [--org-id <ID>]
                                                  [--poll-interval=60]
                                                  [--allow-other]
                                                  [--read-only]
```

- `BASE_URL` — wiki API host (e.g., `https://api.wiki.yandex.net`).
- `ROOT_SLUG` — root page slug for the subtree to mount.
- `MOUNTPOINT` — directory where pages will appear.
- `--auth` or `OMNIFUSE_WIKI_TOKEN` — OAuth or IAM token.
- `--org-id` or `OMNIFUSE_WIKI_ORG_ID` — required for Yandex 360 Wiki.

### File mapping

Each wiki page appears as `<slug>.md`. The directory layout under the
mountpoint mirrors the wiki hierarchy.

### Sync semantics

- Writes are batched, debounced, then pushed via the wiki API.
- A background poller refreshes the index every `--poll-interval`
  seconds.

### Conflict behavior

Concurrent edits to the same page are resolved by three-way merge via
`diffy`. If a clean merge is impossible, the local edit is preserved and
the conflicting remote version is fetched into a `.conflict.md` sibling
file for review. Conflict markers are never written into wiki content.
