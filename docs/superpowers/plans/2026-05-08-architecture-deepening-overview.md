# План углубления архитектурных модулей OmniFuse

> **Для agentic workers:** при реализации отдельных планов используй `subagent-driven-development` для независимых задач или `executing-plans` для последовательного выполнения. Шаги в планах оформлены чекбоксами для отслеживания.

**Цель:** провести серию небольших refactor-RFC, которые делают shallow-связки глубже, честнее по контрактам и проще для boundary-тестирования.

**Архитектура:** рефакторинг разбит на шесть независимых вертикалей. Каждая вертикаль должна дать один глубокий модуль с маленькой внешней поверхностью, заменить часть тестов на проверку наблюдаемого поведения и не добавлять внешних зависимостей без отдельного решения.

**Технологии:** Rust workspace, `tokio`, `anyhow`, `tracing`, `dashmap`, текущие crates `omnifuse-core`, `omnifuse-git`, `omnifuse-wiki`, `unifuse`, CLI и Tauri GUI.

---

## Принципы выполнения

- Производительность важнее косметической чистоты: планы избегают лишних абстракций на горячих путях `write/read/poll`.
- Консистентность важнее локально красивого API: новые интерфейсы должны быть похожи на существующий стиль Rust-кода в workspace.
- DRY важнее YAGNI только там, где повторение имеет одну причину: CLI/GUI mount preparation выносится, но registry для будущих backend'ов не навязывается заранее.
- Тесты проверяют поведение на границе модуля, а не случайную внутреннюю форму: порядок vec, строки логов и внутренние поля проверяются только если это публичный контракт.
- Ошибки и неопределённость оформляются явно: каждый план содержит риски и предположения.
- Каждый план можно реализовать отдельно и смерджить независимо.

## Рекомендуемый порядок

1. [MountService и единая подготовка mount](./2026-05-08-mount-service.md)
2. [GitSyncLifecycle](./2026-05-08-git-sync-lifecycle.md)
3. [WikiPageSyncSession](./2026-05-08-wiki-page-sync-session.md)
4. [RemoteRefresh contract](./2026-05-08-remote-refresh-contract.md)
5. [VFS file mutation pipeline](./2026-05-08-vfs-file-mutation-pipeline.md)
6. [unifuse SessionPathFs](./2026-05-08-unifuse-session-path-fs.md)

Такой порядок снижает риск: сначала убираем реальные рассинхроны конфигурации и локализуем backend lifecycle, затем меняем общий core-контракт, после этого трогаем VFS hot path и библиотечный API `unifuse`.

## Общие инварианты

- `mount_point` и `local_dir/work_dir` всегда разные концепты. Пользователь видит `mount_point`; backend и VFS работают с `local_dir`.
- Backend не должен отдавать наружу фиктивные данные. Если Git хочет `pull`, он не должен притворяться `RemoteChange::Modified { content: Vec::new() }`.
- Wiki invariant `slug <-> path <-> .vfs/meta <-> .vfs/base <-> remote page` должен жить в одном модуле.
- VFS write lifecycle должен проходить через одну точку: buffer, disk flush, dirty event и user event не должны расходиться по разным call sites.
- `unifuse` lifecycle должен быть совместим с `Arc<F>`: `start/stop` на `&self`, а не `init/destroy(&mut self)`.

## Глобальная проверка после серии

Запускать после каждого плана:

```bash
cargo test -p omnifuse-core
cargo test -p omnifuse-git
cargo test -p omnifuse-wiki
cargo test -p omnifuse-cli
cargo test -p unifuse
```

После всех планов:

```bash
cargo test --workspace
```

Ожидаемый результат: все non-ignored тесты проходят. Ignored FUSE/API тесты остаются smoke-слоем и не становятся обязательным условием для локального refactor loop.
