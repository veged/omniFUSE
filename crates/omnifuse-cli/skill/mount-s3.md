## `of mount s3`

```
of mount s3 <BUCKET> <MOUNTPOINT> [--endpoint <URL>]
                                  [--region <REGION>]
                                  [--prefix <KEY_PREFIX>]
                                  [--access-key-id <ID>]
                                  [--secret-access-key <SECRET>]
                                  [--session-token <TOKEN>]
                                  [--virtual-host-style]
                                  [--poll-interval=60]
                                  [--allow-other]
                                  [--read-only]
```

- `BUCKET` — name of the S3-compatible bucket.
- `MOUNTPOINT` — directory where bucket objects will appear.
- `--endpoint` or `OMNIFUSE_S3_ENDPOINT` — service URL. Use the AWS
  regional endpoint for AWS S3, the account endpoint for Cloudflare R2,
  the cluster endpoint for MinIO/RustFS, etc.
- `--region` or `OMNIFUSE_S3_REGION` — region string. Use `auto` for R2
  and most MinIO deployments; a real AWS region for AWS S3.
- `--prefix` — object-key prefix mounted as the filesystem root. Useful
  when you want a single subtree of a bucket rather than the whole one.
- `--access-key-id` / `OMNIFUSE_S3_ACCESS_KEY_ID` and
  `--secret-access-key` / `OMNIFUSE_S3_SECRET_ACCESS_KEY` — credentials.
- `--session-token` / `OMNIFUSE_S3_SESSION_TOKEN` — temporary credential.
- `--virtual-host-style` — opt into virtual-hosted-style URLs (off by
  default; some providers require it).

### File mapping

Object keys map directly to relative paths under the mountpoint. The
key separator `/` becomes a directory separator. Empty-directory
placeholders are ignored (V1 tracks regular file objects only).

Internal metadata lives under `<MOUNTPOINT>/.vfs/s3/`:

- `manifest.json` — per-object remote state (ETag, version, size).
- `base/` — last-known remote bytes used as the base for three-way
  text merges. Treat this directory as internal: do not edit it.

### Sync semantics

- New objects are uploaded with `If-None-Match: *` so two writers
  cannot silently overwrite each other's first write.
- Updates are uploaded with `If-Match: <etag>` against the manifest's
  recorded ETag. A `Precondition Failed` response triggers a remote
  re-read, a fresh three-way merge, and a retry with the new ETag (up
  to three attempts before reporting a conflict).
- A background poller calls `list_with("").recursive(true)` every
  `--poll-interval` seconds to detect remote drift.
- Remote refresh is two-pass: changes are classified first, and only
  applied locally when none of them conflict. A single binary-merge
  conflict defers the entire batch — local files are never partially
  rewritten.

### Conflict behavior

- UTF-8 text objects: three-way merge via `diffy` against the stored
  base. Non-overlapping edits merge automatically; overlapping edits
  surface as `SyncResult::Conflict` and the file remains as it is
  locally for the user to resolve.
- Non-UTF-8 (binary) objects: if both local and remote changed, the
  file is reported as a conflict. The user must overwrite with the
  desired side and run sync again.
- Pre-existing remote object that is absent from the manifest: a
  conflict, unless the local bytes happen to byte-match.
- Local delete vs. concurrent remote update: a conflict.

### Capability requirements

The backend refuses to mount providers that do not advertise the
following OpenDAL capabilities:

- `stat`, `read`, `write`, `delete`, `list`
- `list_with_recursive`
- `write_with_if_match`
- `write_with_if_not_exists`
- `stat_with_if_match` (proxy for ETag availability)

There is no degraded "unsafe write" mode in V1. AWS S3, Cloudflare R2,
Yandex Object Storage and MinIO all satisfy these requirements;
Backblaze B2 is currently unverified.

### Examples

```bash
# AWS S3, project subtree.
export OMNIFUSE_S3_ACCESS_KEY_ID=...
export OMNIFUSE_S3_SECRET_ACCESS_KEY=...
of mount s3 my-bucket ~/mnt/storage \
  --endpoint https://s3.amazonaws.com \
  --region us-east-1 \
  --prefix project

# Cloudflare R2.
of mount s3 my-bucket ~/mnt/r2 \
  --endpoint https://ACCOUNT_ID.r2.cloudflarestorage.com \
  --region auto

# Local MinIO, default test bucket.
of mount s3 omnifuse-test ~/mnt/minio \
  --endpoint http://127.0.0.1:9000 \
  --region us-east-1 \
  --access-key-id minioadmin \
  --secret-access-key minioadmin
```
