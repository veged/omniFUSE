# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-05-09

### Added

- Shared application-level mount service for the CLI and GUI.
- Session-based VFS pipeline: dirty index, file mutation sessions, structural operations, and the remote refresh contract.
- Session filesystem API and adapters for the `rfuse3` and WinFsp compatibility paths.
- Realistic Git/Wiki mount smoke tests, including editorial workflows driven through headless `nvim`.
- Architecture plans for the next codebase improvements.

### Changed

- Git and Wiki backends now use lifecycle/session-oriented synchronization.
- Wiki backend now has a dedicated page domain model and explicit dirty/remote sync boundaries.
- Sourcecraft CI now separates unit and integration checks so flaky FUSE scenarios do not mask normal unit failures.
- Internal workspace dependencies now include explicit versions for crates.io publication.

### Fixed

- Clippy warnings for Linux mode conversions and cache directory handling.
- Concurrent read/write test no longer asserts an invalid transient invariant.
- Restored generated Tauri schemas.

## [0.1.0] - 2025-02-09

### Added

- **unifuse**: cross-platform async FUSE abstraction (`rfuse3` on Unix, WinFsp on Windows).
- **omnifuse-core**: VFS kernel with `Backend`, `SyncEngine` (debounce + polling), and `FileBufferManager` (LRU eviction).
- **omnifuse-git**: Git backend with clone, commit, push/pull retry, and `.gitignore` filtering.
- **omnifuse-wiki**: Wiki backend with HTTP API client, three-way merge via `diffy`, and `MetaStore`.
- **omnifuse-cli**: CLI binary `of` with `mount git`, `mount wiki`, `check`, and `gen-config`.
- **omnifuse-gui**: Tauri desktop GUI with Git/Wiki backend support.
- Testing: 320+ unit and integration tests across all crates.
- CI/CD pipeline for Sourcecraft.dev.
