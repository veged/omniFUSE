# Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Внедрить correlation-first observability model для OmniFuse: общий logging pipeline, typed operational events, taxonomy ошибок и GUI bridge.

**Architecture:** `omnifuse-core` становится владельцем primitives наблюдаемости и compatibility adapters. Runtime и backend-слои постепенно переводятся на `OperationalEvent` и `ObservabilityContext`, а CLI/GUI начинают использовать единый `init_logging`.

**Tech Stack:** Rust workspace, `tracing`, `tracing-subscriber`, Tauri, `anyhow`, существующие integration/e2e tests.

---

### Task 1: Observability Primitives And Shared Logging

**Files:**
- Create: `crates/omnifuse-core/src/observability.rs`
- Modify: `crates/omnifuse-core/src/lib.rs`
- Modify: `crates/omnifuse-core/src/config.rs`
- Modify: `crates/omnifuse-cli/src/main.rs`
- Modify: `crates/omnifuse-gui/tauri/src/main.rs`
- Test: `crates/omnifuse-core/tests/config_tests.rs`

- [ ] **Step 1: Add failing tests for logging config and observability ids**

Run:
```bash
cargo test -p omnifuse-core config_tests
```

Expected: текущие тесты не покрывают `mount_id`, `op_id` и shared logging init.

- [ ] **Step 2: Implement core observability primitives**

Code targets:
```rust
pub struct ObservabilitySession { /* mount_id, backend, mount_point, local_dir, op_seq */ }
pub struct OperationContext { /* op_id, kind, attempt, path, started_at */ }
pub enum OperationalEvent { /* MountStarted, SyncFinished, RemotePollFailed, ... */ }
pub trait OperationalEventSink: Send + Sync + 'static { /* emit */ }
pub fn init_logging(config: &LoggingConfig) -> anyhow::Result<()>;
```

- [ ] **Step 3: Wire shared init into CLI and GUI**

Run:
```bash
cargo test -p omnifuse-core config_tests
cargo test -p omnifuse-cli
```

Expected: config tests green, CLI still compiles with shared logging init.

### Task 2: Runtime Events And Compatibility Layer

**Files:**
- Modify: `crates/omnifuse-core/src/events.rs`
- Modify: `crates/omnifuse-core/src/lib.rs`
- Modify: `crates/omnifuse-core/src/sync_engine.rs`
- Modify: `crates/omnifuse-core/src/vfs.rs`
- Modify: `crates/omnifuse-core/src/test_utils.rs`
- Test: `crates/omnifuse-core/tests/e2e_pipeline_tests.rs`
- Test: `crates/omnifuse-core/tests/e2e_cross_backend_scenarios.rs`

- [ ] **Step 1: Add failing tests for typed events**

Run:
```bash
cargo test -p omnifuse-core e2e_pipeline_tests -- --nocapture
```

Expected: tests still завязаны на строки и не проверяют typed payload.

- [ ] **Step 2: Introduce compatibility bridge**

Code targets:
```rust
pub struct CompatibilityEventSink<E> { /* maps OperationalEvent to VfsEventHandler */ }
impl VfsEventHandler for CompatibilityEventSink<_> { /* emit legacy signals */ }
```

- [ ] **Step 3: Migrate run_mount, SyncEngine and VFS to OperationalEvent**

Code targets:
```rust
// run_mount
emit(MountStarted { ... });
emit(BackendInitFinished { ... });

// sync_engine
emit(SyncStarted { ... });
emit(SyncDeferredOffline { ... });
emit(RemotePollFailed { ... });

// vfs
emit(FileMarkedDirty { ... });
emit(FileFlushed { ... });
```

- [ ] **Step 4: Run focused core tests**

Run:
```bash
cargo test -p omnifuse-core sync_engine
cargo test -p omnifuse-core e2e_pipeline_tests -- --nocapture
```

Expected: typed events are emitted, legacy compatibility assertions still hold where needed.

### Task 3: Error Taxonomy And Observed Backends

**Files:**
- Modify: `crates/omnifuse-core/src/backend.rs`
- Modify: `crates/omnifuse-core/src/observability.rs`
- Modify: `crates/omnifuse-git/src/lib.rs`
- Modify: `crates/omnifuse-git/src/ops.rs`
- Modify: `crates/omnifuse-git/src/engine.rs`
- Modify: `crates/omnifuse-wiki/src/lib.rs`
- Modify: `crates/omnifuse-wiki/src/client.rs`
- Test: `crates/omnifuse-git/tests/backend_integration.rs`
- Test: `crates/omnifuse-wiki/tests/client_tests.rs`

- [ ] **Step 1: Add failing tests for error classification**

Run:
```bash
cargo test -p omnifuse-git backend_integration
cargo test -p omnifuse-wiki client_tests
```

Expected: failures or gaps around semantic classification of offline/conflict/auth/protocol errors.

- [ ] **Step 2: Add observed backend wrapper and shared classifiers**

Code targets:
```rust
pub struct ObservedBackend<B> { /* backend, session, sink */ }
fn classify_git_error(error: &anyhow::Error) -> ErrorKind;
fn classify_wiki_error(error: &anyhow::Error) -> ErrorKind;
```

- [ ] **Step 3: Remove routing decisions based on string parsing where practical**

Run:
```bash
cargo test -p omnifuse-git
cargo test -p omnifuse-wiki
```

Expected: backend crates pass with shared observability semantics.

### Task 4: Tauri Bridge And Structured UI Payloads

**Files:**
- Modify: `crates/omnifuse-gui/tauri/src/events.rs`
- Modify: `crates/omnifuse-gui/tauri/src/commands.rs`
- Modify: `crates/omnifuse-gui/tauri/src/main.rs`

- [ ] **Step 1: Add failing compile/tests pass for GUI integration**

Run:
```bash
cargo test -p omnifuse-gui
```

Expected: GUI crate either has no coverage for structured payloads or fails once API changes land.

- [ ] **Step 2: Bridge OperationalEvent to Tauri IPC**

Code targets:
```rust
emit("vfs:event", structured_payload);
emit_legacy_events_for_compatibility(&event);
```

- [ ] **Step 3: Verify GUI crate compiles**

Run:
```bash
cargo test -p omnifuse-gui
```

Expected: Tauri backend compiles with shared logging and event bridge.

### Task 5: Regression Suite And Cleanup

**Files:**
- Modify: `crates/omnifuse-core/tests/...`
- Modify: `crates/omnifuse-git/tests/...`
- Modify: `crates/omnifuse-wiki/tests/...`
- Modify: `docs/superpowers/specs/2026-04-06-observability-design.md`

- [ ] **Step 1: Update tests to assert typed event sequences**

Code targets:
```rust
assert!(events.iter().any(|event| matches!(event, OperationalEvent::RemotePollFailed { .. })));
```

- [ ] **Step 2: Run focused workspace verification**

Run:
```bash
cargo test -p omnifuse-core
cargo test -p omnifuse-git
cargo test -p omnifuse-wiki
cargo test -p omnifuse-cli
cargo test -p omnifuse-gui
```

Expected: all targeted crates pass.

- [ ] **Step 3: Run workspace-wide verification**

Run:
```bash
cargo test --workspace
```

Expected: full workspace green.
