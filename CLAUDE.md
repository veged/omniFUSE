# OmniFuse Project Rules

## Language

All code comments, documentation, and commit messages must be in **English**.

## Code Style

- Rust Edition 2024, nightly toolchain
- Formatting: `rustfmt.toml`
- Linting: strict clippy rules (see root `Cargo.toml`)
- Never `unwrap()` / `expect()` — use `?` only
- Document all pub items
- Structured logging via tracing
- Async-first: all traits and APIs are async (rfuse3, Backend, UniFuseFilesystem)

## Architecture

- Monorepo with workspace crates
- `unifuse` — cross-platform FUSE abstraction (publishable to crates.io separately)
- `omnifuse-core` — VFS kernel (platform-agnostic)
- `omnifuse-git` — Git backend
- `omnifuse-wiki` — Wiki backend
- `omnifuse-cli` — CLI binary `of`
- `omnifuse-gui` — Tauri + React GUI

## Key Decisions

- FUSE crate: rfuse3 (async-native, tokio)
- Single `Backend` trait for all backends
- CLI binary: `of`
- Async everywhere: UniFuseFilesystem async, Backend async
