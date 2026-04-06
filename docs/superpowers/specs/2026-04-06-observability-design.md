# OmniFuse Observability Design

Дата: 2026-04-06
Статус: draft

## Контекст

В проекте уже есть `tracing`, локальные `warn!/error!` и строковый `VfsEventHandler`, но наблюдаемость остаётся фрагментированной:

- CLI инициализирует `tracing`, GUI нет.
- `LoggingConfig` существует как конфиг, но не управляет реальным logging pipeline.
- значимая часть ошибок передаётся как строки и местами классифицируется через `contains(...)`;
- пользовательские события и техническая диагностика смешаны.

Это достаточно для happy path и локального дебага, но недостаточно для разбора сложных сбоев, корреляции событий и честного UI для нештатных сценариев.

## Цель

Прийти к модели, в которой для любого инцидента можно быстро ответить на четыре вопроса:

1. Что сломалось.
2. Где сломалось.
3. Насколько это серьёзно.
4. Что система сделала дальше.

## Не-цели

- Не внедрять сейчас внешнюю telemetry platform.
- Не тащить в проект тяжёлые зависимости ради наблюдаемости.
- Не переписывать сразу все контракты между crates одним большим коммитом.

## Ключевое решение

Наблюдаемость делится на два контура:

- технический контур: структурные логи через `tracing`;
- продуктовый контур: typed operational events для UI и верхнеуровневой диагностики.

Оба контура строятся поверх общего `ObservabilityContext`.

## Целевая модель

### 1. ObservabilityContext

В `omnifuse-core` вводится root-session для каждого mount:

- `mount_id`
- `backend`
- `mount_point`
- `local_dir`
- `session_started_at`
- `op_seq`

Из него порождаются контексты операций:

- `MountOp`
- `InitOp`
- `SyncOp`
- `PollOp`
- `ApplyRemoteOp`
- `FileOp`

У каждой операции есть:

- `op_id`
- `kind`
- `attempt`
- `started_at`
- опционально `path`

### 2. OperationalEvent

Строковый `VfsEventHandler` перестаёт быть primary contract. Основной моделью становится typed enum `OperationalEvent`.

Минимальный набор событий:

- `MountStarted`
- `MountFinished`
- `MountFailed`
- `BackendInitStarted`
- `BackendInitFinished`
- `SyncStarted`
- `SyncFinished`
- `SyncDeferredOffline`
- `ConflictDetected`
- `RemotePollStarted`
- `RemotePollFailed`
- `RemoteChangesDetected`
- `RemoteApplyFailed`
- `SlowOperationDetected`
- `FileMarkedDirty`
- `FileFlushed`
- `PushRejected`
- `UserVisibleWarning`

Каждое событие несёт:

- `context`
- `severity`
- `outcome`
- `error_kind`
- `source`
- `disposition`
- payload с конкретными полями сценария

### 3. Taxonomy ошибок

Вместо строковых эвристик вводится стабильная классификация.

`ErrorKind`:

- `Offline`
- `Timeout`
- `AuthFailed`
- `PermissionDenied`
- `NotFound`
- `Conflict`
- `RateLimited`
- `InvalidConfig`
- `InvalidInput`
- `FsIo`
- `BackendCommandFailed`
- `ProtocolViolation`
- `Internal`

`ErrorSource`:

- `Mount`
- `Vfs`
- `SyncEngine`
- `Git`
- `Wiki`
- `Fuse`
- `Gui`

`Disposition`:

- `Retryable`
- `AutoRetrying`
- `UserActionRequired`
- `Fatal`

### 4. Единый logging pipeline

В `omnifuse-core` появляется единая инициализация логирования:

- общий `init_logging`;
- поддержка `LoggingConfig.level`;
- поддержка `LoggingConfig.log_file`;
- одинаковый pipeline для CLI и GUI;
- структурные поля по умолчанию: `mount_id`, `backend`, `op_id`, `attempt`, `path`, `elapsed_ms`, `outcome`.

### 5. Совместимость

Миграция делается через adapter layer:

- новый контракт: `OperationalEventSink`;
- старый контракт: `VfsEventHandler`;
- bridge: `OperationalEvent -> VfsEventHandler`.

Это позволяет перевести core, GUI и тесты постепенно.

## Точки интеграции

### `crates/omnifuse-core`

Новые или изменяемые модули:

- `src/observability.rs`: `ObservabilitySession`, `OperationContext`, `OperationalEvent`, `ErrorKind`, `OperationalEventSink`, `init_logging`;
- `src/lib.rs`: создание root session, mount-level events, wiring sinks;
- `src/sync_engine.rs`: `SyncOp`, `PollOp`, `ApplyRemoteOp`, slow-iteration detection как typed events;
- `src/vfs.rs`: file-level events и честная диагностика потери событий при backpressure;
- `src/events.rs`: compatibility adapter.

### `crates/omnifuse-git`

Базовая идея: не ломать сразу `Backend`, а обернуть backend в `ObservedBackend<B>`.

Это позволит:

- создавать spans и operation context вокруг `init/sync/poll_remote/apply_remote`;
- классифицировать git-ошибки в `ErrorKind`;
- убрать принятие решений по `e.to_string().contains(...)`.

### `crates/omnifuse-wiki`

Нужно:

- поднять текущую HTTP-классификацию до общего `ErrorKind`;
- ограничить body logging и добавить redaction для чувствительных данных;
- эмитить typed events для offline, auth, conflict и protocol-level ошибок.

### `crates/omnifuse-gui/tauri`

Нужно:

- инициализировать тот же logging pipeline, что и в CLI;
- перейти от string-first IPC к typed operational events;
- оставить совместимый mapping для текущих UI-событий на время миграции.

## План внедрения

### Фаза 1. База инфраструктуры

Результат:

- единый `init_logging`;
- реальное использование `LoggingConfig`;
- root `ObservabilitySession`;
- `mount_id` в логах CLI и GUI.

Маленькие коммиты:

1. `core: add observability primitives and init_logging`
2. `cli: wire logging config into shared init`
3. `gui: initialize shared logging pipeline`

### Фаза 2. Operation context и backend decorator

Результат:

- `ObservedBackend<B>`;
- operation spans и `op_id`;
- start/finish/failure events для `init/sync/poll/apply_remote`.

Маленькие коммиты:

1. `core: add operation context and observed backend wrapper`
2. `core: emit mount and backend lifecycle events`
3. `git/wiki: classify backend failures into shared error kinds`

### Фаза 3. Typed events в core runtime

Результат:

- `SyncEngine` и `VFS` перестают эмитить только строки;
- UI и тесты получают typed operational events;
- slow/offline/conflict scenarios видны как отдельные исходы.

Маленькие коммиты:

1. `core: add operational event sink and compatibility adapter`
2. `core: migrate sync engine to typed events`
3. `core: migrate vfs file lifecycle to typed events`

### Фаза 4. GUI bridge и совместимость

Результат:

- Tauri подписан на typed events;
- старые IPC события живут как compatibility layer;
- UI не парсит английские строки для различения сценариев.

Маленькие коммиты:

1. `gui: add bridge from operational events to tauri payloads`
2. `gui: expose structured mount/sync/error states`
3. `gui: reduce string-first event handling`

### Фаза 5. Очистка и закрепление

Результат:

- удалены строковые эвристики и лишние дубли;
- тесты опираются на typed payload;
- новая модель становится дефолтной.

Маленькие коммиты:

1. `core: remove string-based error routing`
2. `tests: migrate observability assertions to typed events`
3. `docs: document observability model and operator workflow`

## Тестовая стратегия

Покрытие нужно строить по слоям:

- unit: `ErrorKind` mapping, `mount_id/op_id`, adapters;
- integration: offline, conflict, slow sync, poll failure, apply_remote failure, cancel during mount;
- event-sequence tests: проверка последовательности `OperationalEvent`, а не строк;
- correlation tests: один `mount_id` связывает mount, sync engine, backend и GUI bridge;
- regression tests на backpressure в `VFS`.

## Безопасность логов

По умолчанию нельзя логировать:

- auth token;
- authorization headers;
- сырые тела ответов API без explicit trace gate;
- пользовательские данные без необходимости.

TRACE-body logging допускается только с redaction и только как режим целевого дебага.

## Definition of Done

Вариант 3 считается достигнутым, когда:

- у каждого mount есть стабильный `mount_id`;
- у каждой значимой операции есть `op_id`, `attempt`, `elapsed_ms`, `outcome`;
- UI различает `offline`, `conflict`, `auth`, `timeout`, `invalid config` без парсинга текста;
- core не принимает решения через `contains(...)` по тексту ошибки;
- CLI и GUI используют один logging pipeline;
- `LoggingConfig` реально управляет выводом;
- по одному `mount_id` можно восстановить цепочку инцидента end-to-end;
- чувствительные данные не попадают в обычные логи.

## Риски

- Миграция затронет существующие e2e и test-utils, где ожидания завязаны на строки.
- При неаккуратной миграции можно получить дубли между `tracing`, `OperationalEvent` и compatibility adapter.
- Если сразу полезть менять `Backend` trait, объём и риск вырастут непропорционально.

## Рекомендация

Цель фиксируется как вариант 3, но реализация идёт последовательно:

1. выровнять logging infrastructure;
2. ввести typed operational events;
3. перевести runtime и GUI на correlation-first модель;
4. удалить строковые эвристики и временные мосты.

Это даёт честную наблюдаемость без лишнего архитектурного взрыва.
