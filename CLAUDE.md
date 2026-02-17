# OmniFuse Project Rules

Architecture, code style, design decisions, and full development guide: see [CONTRIBUTING.md](CONTRIBUTING.md).

## Language

All code comments, documentation, and commit messages must be in **English**.

## Key Rules

- Never `unwrap()` / `expect()` — use `?` only
- Async-first: all traits and APIs are async
- CLI binary name: `of`
