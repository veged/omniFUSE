# unifuse

Cross-platform async FUSE abstraction for Rust.

Write your filesystem once with a single async trait, and **unifuse** handles
the platform-specific plumbing:

| Platform | Backend | Driver |
|----------|---------|--------|
| Linux | [rfuse3](https://crates.io/crates/rfuse3) | libfuse3 / `/dev/fuse` |
| macOS | [rfuse3](https://crates.io/crates/rfuse3) | [macFUSE](https://osxfuse.github.io/) |
| Windows | [winfsp-rs](https://crates.io/crates/winfsp) | [WinFsp](https://winfsp.dev/) |

## Quick start

```rust
use std::path::Path;
use unifuse::{UniFuseFilesystem, UniFuseHost, MountOptions};

// 1. Implement the trait
struct MyFs { /* ... */ }
impl UniFuseFilesystem for MyFs {
    // Only 6 methods are required — the rest have default implementations.
    // See the trait docs for the full list.
    // ...
}

// 2. Mount
let host = UniFuseHost::new(MyFs::new());
host.mount(Path::new("/mnt/myfs"), &MountOptions::default()).await?;
```

## Architecture

```
                     Your code
                        |
              +---------v----------+
              | UniFuseFilesystem  |   async, path-based trait
              +---------+----------+
                        |
           +------------+-------------+
           |                          |
  +--------v---------+    +-----------v----------+
  |  Rfuse3Adapter   |    |   WinfspAdapter      |
  | (Linux / macOS)  |    |     (Windows)        |
  |  inode -> path   |    |  sync -> async       |
  +--------+---------+    +-----------+----------+
           |                          |
     /dev/fuse                  winfsp-x64.dll
```

**Key design decisions:**

- **Path-based trait** — the filesystem trait uses `&Path`, not inodes.
  `Rfuse3Adapter` maintains an `InodeMap` for the inode-to-path translation
  that rfuse3 requires. WinFsp is natively path-based, so no mapping is needed.

- **Async-first** — all trait methods return futures. On Unix, rfuse3 calls them
  natively from a tokio runtime. On Windows, `WinfspAdapter` bridges the
  synchronous WinFsp callbacks to async via `tokio::runtime::Handle::block_on()`.

- **Zero `unsafe`** — the crate uses `#![forbid(unsafe_code)]`.

## The trait

`UniFuseFilesystem` has **6 required methods** and many optional ones
(with sensible defaults that return `FsError::NotSupported`):

| Required | Description |
|----------|-------------|
| `getattr` | Get file/directory attributes |
| `lookup` | Look up a child by name |
| `open` | Open a file |
| `read` | Read data |
| `release` | Close a file |
| `readdir` | List directory contents |
| `statfs` | Filesystem statistics |

Optional methods include `write`, `create`, `mkdir`, `rmdir`, `unlink`,
`rename`, `symlink`, `readlink`, `setattr`, `flush`, `fsync`,
`setxattr`/`getxattr`/`listxattr`/`removexattr`.

## Types

| Type | Description |
|------|-------------|
| `FileAttr` | Cross-platform file attributes (size, timestamps, kind, permissions) |
| `FileHandle` | Opaque handle for an open file |
| `DirEntry` | Directory entry (name + kind) |
| `OpenFlags` | File open flags (read, write, append, truncate, create, exclusive) |
| `StatFs` | Filesystem statistics (blocks, free, files, block size) |
| `FsError` | Error enum with `to_errno()` (Unix) and `to_ntstatus()` (Windows) |
| `FileType` | File type (regular, directory, symlink, device, pipe, socket) |

## Platform detection

```rust
if UniFuseHost::<MyFs>::is_available() {
    println!("FUSE/WinFsp is installed");
}
```

## Prerequisites

| Platform | Install |
|----------|---------|
| Linux | `sudo apt install libfuse3-dev fuse3` |
| macOS | `brew install macfuse` |
| Windows | `choco install winfsp` |

## Minimum Rust version

Requires **nightly** (Rust edition 2024, RPITIT).

## License

MIT
