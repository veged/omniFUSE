# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-02-09

### Added

- **unifuse**: Cross-platform async FUSE abstraction (rfuse3 on Unix, WinFsp on Windows)
- **omnifuse-core**: VFS kernel with Backend trait, SyncEngine (debounce + polling), FileBufferManager (LRU eviction)
- **omnifuse-git**: Git backend — clone, commit, push/pull with retry, .gitignore filtering
- **omnifuse-wiki**: Wiki backend — HTTP API client, three-way merge via diffy, MetaStore
- **omnifuse-cli**: CLI binary `of` with `mount git`, `mount wiki`, `check`, `gen-config` commands
- **omnifuse-gui**: Tauri-based desktop GUI with git/wiki backend support
- **Testing**: 320+ unit and integration tests across all crates
- **CI/CD**: SourceCraft.dev pipeline configuration
