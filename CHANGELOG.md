# Changelog

Все заметные изменения проекта фиксируются в этом файле.

Формат основан на [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
проект следует [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-05-09

### Добавлено

- Общий application-level mount service для CLI и GUI.
- Session-based VFS pipeline: dirty index, file mutation sessions, структурные операции и refresh-контракт для удалённых изменений.
- Session filesystem API и адаптеры для `rfuse3`/WinFsp compatibility path.
- Реалистичные smoke-тесты для Git/Wiki mount, включая сценарии редакционной работы через headless `nvim`.
- Архитектурные планы развития кодовой базы.

### Изменено

- Git и Wiki backend переведены на lifecycle/session-ориентированную синхронизацию.
- Wiki backend получил доменную модель страницы и явное разделение dirty/remote sync.
- CI на Sourcecraft разделён на unit/integration контуры, чтобы flaky FUSE-сценарии не маскировали обычные unit-проверки.
- Внутренние workspace-зависимости теперь имеют явные версии для публикации crates.io packages.

### Исправлено

- Clippy-предупреждения Linux mode conversions и cache directory handling.
- Нестабильный concurrent read/write тест больше не закрепляет некорректный transient invariant.
- Восстановлены generated Tauri schemas.

## [0.1.0] - 2025-02-09

### Добавлено

- **unifuse**: cross-platform async FUSE abstraction (`rfuse3` на Unix, WinFsp на Windows).
- **omnifuse-core**: VFS kernel с `Backend`, `SyncEngine` (debounce + polling), `FileBufferManager` (LRU eviction).
- **omnifuse-git**: Git backend с clone, commit, push/pull retry и `.gitignore` filtering.
- **omnifuse-wiki**: Wiki backend с HTTP API client, three-way merge через `diffy` и `MetaStore`.
- **omnifuse-cli**: CLI binary `of` с командами `mount git`, `mount wiki`, `check`, `gen-config`.
- **omnifuse-gui**: Tauri desktop GUI с Git/Wiki backend support.
- Testing: 320+ unit и integration tests во всех crates.
- CI/CD pipeline для Sourcecraft.dev.
