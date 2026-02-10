# Правила проекта OmniFuse

## Язык

Все комментарии в коде, документация и сообщения коммитов должны быть на **русском языке**.

## Код-стайл

- Rust Edition 2024, nightly toolchain
- Форматирование: `rustfmt.toml`
- Линтинг: строгие clippy правила (см. корневой `Cargo.toml`)
- Никогда `unwrap()` / `expect()` — только `?`
- Документировать все pub items
- Structured logging через tracing
- Async-first: все trait'ы и API — async (rfuse3, Backend, UniFuseFilesystem)

## Архитектура

- Монорепозиторий с workspace crates
- `unifuse` — кроссплатформенная FUSE-абстракция (публикуется на crates.io отдельно)
- `omnifuse-core` — VFS ядро (platform-agnostic)
- `omnifuse-git` — Git backend
- `omnifuse-wiki` — Wiki backend
- `omnifuse-cli` — CLI binary `of`
- `omnifuse-gui` — Tauri + React GUI

## Ключевые решения

- FUSE crate: rfuse3 (async-native, tokio)
- Один `Backend` trait для всех backend'ов
- CLI binary: `of`
- Async everywhere: UniFuseFilesystem async, Backend async
