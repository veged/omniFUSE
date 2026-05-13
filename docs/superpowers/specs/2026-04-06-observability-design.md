# OmniFuse Event Contract

Date: 2026-04-06
Status: implemented baseline

## Context

OmniFuse needs one small product event contract shared by the core, GUI, CLI-oriented tooling and tests.

Technical logs remain a separate `tracing` concern. Events describe user-visible system behavior: mount lifecycle, file changes, sync work, remote updates, conflicts and recoverable failures.

## Decision

The production event API is `event::Sink` receiving `event::Event`.

Legacy string callbacks and per-case GUI events are not part of the contract. The GUI consumes a single Tauri channel:

```text
omnifuse:event
```

## Event Shape

```ts
type Event = {
  version: 1;
  seq: number;
  time: number;
  mountId: string;
  opId?: number;
  kind: Kind;
  level: 'info' | 'warn' | 'error';
  source: Source;
  data?: object;
  error?: EventError;
};
```

`seq` is monotonic inside one mount session. `opId` correlates events belonging to the same operation.

## Kinds

```text
mount.start
mount.ready
mount.stop
mount.fail

file.change
file.flush
file.create
file.delete
file.rename

sync.start
sync.done
sync.defer
sync.fail

remote.poll
remote.change
remote.apply
remote.defer
remote.fail

conflict
queue.drop
auth.fail
```

The kind carries the outcome when that keeps the model smaller. For example, `sync.done`, `sync.defer` and `sync.fail` are separate kinds instead of one `sync.finished` event with an extra outcome field.

## Error

```ts
type EventError = {
  code: Code;
  message: string;
  action: 'retry' | 'wait' | 'fix' | 'stop';
};
```

`message` is diagnostic text. UI behavior must depend on `kind`, `level`, `code` and `action`, not string matching.

## Data

`data` must stay small and semantic. It may include paths, counters and elapsed time, but not document contents.

Examples:

```json
{ "path": "draft.md", "bytes": 120 }
{ "dirty": 3, "synced": 3, "conflicts": 0 }
{ "changed": 2, "deleted": 1 }
{ "paths": ["article.md", "seeds/short.md"] }
```

## Testing

Tests should assert observable behavior through:

- event `kind`;
- `mountId`, `opId` and `seq` correlation;
- small semantic `data` fields;
- final file/backend contents for realistic workflows.

Tests should not depend on exact diagnostic text unless that text is an explicit public contract.
