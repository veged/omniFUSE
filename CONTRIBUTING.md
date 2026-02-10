# Contributing to omniFUSE

## Prerequisites

- Rust nightly toolchain (`rustup toolchain install nightly`)
- FUSE driver (macFUSE on macOS, libfuse3 on Linux)

## Building

```bash
cargo build --workspace
```

## Testing

Run all tests:

```bash
cargo test --workspace
```

Skip FUSE mount tests (if no FUSE driver):

```bash
cargo test --workspace -- --skip fuse_mount
```

Skip real Wiki API tests:

```bash
cargo test --workspace -- --skip real_api
```

## Linting

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Code style

- **Rust Edition 2024**, nightly toolchain
- Formatting: `rustfmt.toml` in repo root
- Strict clippy: `unsafe_code = "forbid"`, `unwrap_used = "deny"`, `expect_used = "deny"`
- Use `?` operator, never `unwrap()` or `expect()` in library code
- `#[allow(clippy::expect_used)]` is acceptable in tests
- Document all `pub` items
- Structured logging via `tracing`

## Project structure

| Crate | Description |
|-------|-------------|
| `unifuse` | Cross-platform async FUSE abstraction |
| `omnifuse-core` | VFS kernel: Backend trait, SyncEngine, FileBufferManager |
| `omnifuse-git` | Git backend |
| `omnifuse-wiki` | Wiki backend |
| `omnifuse-cli` | CLI binary `of` |
| `omnifuse-gui` | Desktop GUI (Tauri + React) |

## Adding a new backend

1. Create a new crate `crates/omnifuse-<name>/`
2. Implement the `Backend` trait from `omnifuse-core`
3. Wire it into `omnifuse-cli` and/or `omnifuse-gui`
4. Add tests following existing patterns (MockBackend for unit tests, tempdir for integration)

## Commit messages

Use conventional format:

```
type(scope): short description

Longer explanation if needed.
```

Types: `feat`, `fix`, `docs`, `test`, `refactor`, `chore`, `ci`

## Pull requests

- All tests must pass
- No clippy warnings
- Code must be formatted with `cargo fmt`
