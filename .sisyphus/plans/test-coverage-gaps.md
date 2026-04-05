# План дополнения автотестов OmniFuse

> **Цель**: покрыть все пользовательские сценарии, которые сейчас не проверяются.
> **Контекст**: ~319 тестов уже есть, но они сосредоточены на unit- и компонентном уровне.
> Не хватает комплексных E2E-сценариев "глазами пользователя".

---

## Фаза 1: E2E пользовательские сценарии (высокий приоритет)

### 1.1 Git-бэкенд: полный цикл mount → edit → sync

**Файл**: `crates/omnifuse-core/tests/e2e_git_user_scenarios.rs`

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Create → Write → Release → Backend.sync()** | Пользователь создал файл, отредактировал, закрыл — файл закоммичен и запушен |
| 2 | **Edit existing file → sync** | Изменение существующего файла → автокоммит |
| 3 | **Delete file → sync** | Удаление файла через VFS → коммит удаления |
| 4 | **Rename file → sync** | Переименование → коммит с git mv |
| 5 | **Multiple files edited → single batch sync** | Редактирование 3 файлов в течение debounce → один batch-коммит |
| 6 | **Edit → remote change → pull → conflict detection** | Пользователь редактирует, кто-то пушит в репо → конфликт обнаружен |
| 7 | **Write during sync (race condition)** | Пользователь пишет файл, пока идёт sync → данные не потеряны |
| 8 | **Large file (>10MB) write → sync** | Большой файл корректно буферизуется и синхронизируется |

### 1.2 Wiki-бэкенд: полный цикл mount → edit → sync

**Файл**: `crates/omnifuse-wiki/tests/e2e_wiki_user_scenarios.rs`

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Init → download tree → files on disk** | Монтирование wiki → все страницы скачаны как .md файлы |
| 2 | **Edit page → sync → page updated on server** | Редактирование .md → WikiBackend.sync() → update_page API вызван |
| 3 | **Create new page → sync → page created on server** | Создание нового .md → create_page API вызван |
| 4 | **Delete page → sync → page deleted on server** | Удаление .md → delete_page API вызван |
| 5 | **Concurrent edit: local + remote change → conflict** | Пользователь редактирует, сервер изменил страницу → three-way merge conflict |
| 6 | **Concurrent edit: same page, different sections → auto-merge** | Three-way merge успешно разрешает конфликт (diffy) |
| 7 | **Nested pages: create/edit/delete in subdirectory** | Работа со вложенными страницами (root/docs/api.md) |
| 8 | **Poll detects remote change → file updated on disk** | Кто-то изменил wiki → poll_remote → apply_remote → файл обновлён |

### 1.3 Cross-backend: общие сценарии

**Файл**: `crates/omnifuse-core/tests/e2e_cross_backend_scenarios.rs`

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Offline mode: write files → go online → sync succeeds** | Пользователь работает без сети → файлы в dirty set → восстановление → sync |
| 2 | **Offline mode: poll fails gracefully** | Backend.is_online() = false → poll_worker не падает |
| 3 | **Sync error → retry on next event** | Backend.sync() возвращает Err → файл остаётся dirty → повторный sync |
| 4 | **Backend returns Conflict → event handler receives warning** | Conflict detected → on_log(Warn) вызван |
| 5 | **Multiple VFS instances → single SyncEngine** | 5 VFS на одном SyncEngine → все события обработаны |
| 6 | **Shutdown → final sync → no data loss** | Пользователь размонтирует → final sync → все dirty файлы синхронизированы |

---

## Фаза 2: CLI-тесты (высокий приоритет)

### 2.1 CLI: расширенные тесты аргументов

**Файл**: `crates/omnifuse-cli/tests/cli_tests.rs` (дополнение)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | `of mount git` — missing mountpoint | Error message содержит "required" |
| 2 | `of mount wiki` — missing auth token | Error message содержит "auth" или "token" |
| 3 | `of mount git` — `--branch` flag parsing | Branch argument корректно парсится |
| 4 | `of mount git` — `--poll-interval` flag parsing | Poll interval корректно парсится |
| 5 | `of mount git` — `--allow-other` flag parsing | Allow other корректно парсится |
| 6 | `of mount git` — `--read-only` flag parsing | Read only корректно парсится |
| 7 | `of mount wiki` — `--org-id` flag parsing | Org ID корректно парсится |
| 8 | `of mount wiki` — `OMNIFUSE_WIKI_TOKEN` env var | Token из env var подхватывается |
| 9 | `of mount wiki` — `OMNIFUSE_WIKI_ORG_ID` env var | Org ID из env var подхватывается |
| 10 | `of` — unknown command | Error message содержит "unrecognized" |
| 11 | `of mount` — no backend specified | Error message содержит "required" |

### 2.2 CLI: cache directory

**Файл**: `crates/omnifuse-cli/tests/cli_tests.rs` (дополнение)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | `cache_dir_for` — non-existent mountpoint parent | Parent не существует → корректная ошибка |
| 2 | `cache_dir_for` — relative path | Relative path → canonicalize работает |
| 3 | `cache_dir_for` — path with spaces | Path с пробелами → корректный hash |
| 4 | `cache_dir_for` — XDG_CACHE_HOME override | Linux: XDG_CACHE_HOME используется |
| 5 | `cache_dir_for` — macOS default path | macOS: ~/Library/Caches используется |

### 2.3 CLI: gen-config output validation

**Файл**: `crates/omnifuse-cli/tests/cli_tests.rs` (дополнение)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | Output is valid TOML | Можно распарсить через `toml::from_str` |
| 2 | Contains both git and wiki examples | Оба бэкенда представлены |
| 3 | Contains mount examples | Секция [mounts] присутствует |

---

## Фаза 3: Конфигурация (средний приоритет)

### 3.1 TOML config parsing

**Файл**: `crates/omnifuse-core/tests/config_tests.rs` (новый)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | Full config TOML → MountConfig roundtrip | Все поля сериализуются/десериализуются |
| 2 | Partial config → defaults applied | Отсутствующие поля получают default-значения |
| 3 | Invalid TOML → parse error | Некорректный TOML → Err |
| 4 | Invalid field values (negative debounce) | Отрицательные значения → Err или saturating |
| 5 | Empty config → all defaults | `{}` → все поля default |
| 6 | Multiple backends in config | `[backends.git-repo]` + `[backends.wiki]` |
| 7 | Config with custom sync settings | Custom debounce, retry, backoff |
| 8 | Config with custom buffer settings | Custom max_memory_mb, lru disabled |
| 9 | Config with logging to file | log_file = "/path/to/log" |
| 10 | Config with read_only mount option | read_only = true |

---

## Фаза 4: Edge-кейсы пользователя (средний приоритет)

### 4.1 Offline/Online transition

**Файл**: `crates/omnifuse-core/tests/e2e_offline_scenarios.rs` (новый)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Init in offline mode** | Backend.init() когда remote недоступен → InitResult::Offline |
| 2 | **Sync in offline mode** | Backend.sync() → SyncResult::Offline → файлы остаются dirty |
| 3 | **Recovery after offline** | Offline → Success → файлы синхронизированы |
| 4 | **Poll during offline** | poll_remote() → Err → logged, не падает |
| 5 | **is_online() toggling** | Backend.is_online() false → true → poll resumes |
| 6 | **Extended offline (many writes)** | 20 файлов написано в offline → recovery → все 20 синхронизированы |

### 4.2 Large file handling

**Файл**: `crates/omnifuse-core/tests/e2e_large_file_scenarios.rs` (новый)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Write 10MB file** | Buffer handles large content |
| 2 | **Read 10MB file** | Buffer returns correct data |
| 3 | **Partial read of large file** | Read at offset, size < total |
| 4 | **Multiple writes to large file** | Write at different offsets |
| 5 | **Flush large file** | Buffer → disk persistence |
| 6 | **LRU eviction under memory pressure** | max_memory_mb exceeded → old buffers evicted |

### 4.3 Error resilience

**Файл**: `crates/omnifuse-core/tests/e2e_error_scenarios.rs` (новый)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Backend.init() fails** | run_mount() → Err, graceful cleanup |
| 2 | **Backend.sync() fails repeatedly** | Retry logic, error logged |
| 3 | **Backend.poll_remote() fails** | Error logged, poll continues |
| 4 | **Backend.apply_remote() fails** | Error logged, next poll retries |
| 5 | **Disk full during write** | FsError returned, not panic |
| 6 | **Corrupted cache directory** | Init handles gracefully |
| 7 | **Channel closed during operation** | Graceful handling, no panic |

---

## Фаза 5: VFS operation completeness (средний приоритет)

### 5.1 VFS: дополнительные операции

**Файл**: `crates/omnifuse-core/src/vfs.rs` (дополнение unit-тестов)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **symlink + readlink** | Создание и чтение symlink |
| 2 | **setxattr / getxattr / listxattr / removexattr** | Extended attributes (Unix) |
| 3 | **setattr: mode change** | chmod через FUSE |
| 4 | **setattr: mtime change** | touch -t через FUSE |
| 5 | **read beyond EOF** | Read at offset > file size → empty/short read |
| 6 | **write at offset > file size** | Sparse file behavior |
| 7 | **rename: overwrite existing target** | Atomic replace |
| 8 | **mkdir: already exists** | Error handling |
| 9 | **readdir: empty directory** | Returns empty list (no dot entries from VFS) |
| 10 | **readdir: deeply nested** | readdir на вложенную директорию |

### 5.2 Buffer manager

**Файл**: `crates/omnifuse-core/src/buffer.rs` (дополнение unit-тестов)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **LRU eviction: oldest removed first** | Memory limit reached → oldest buffer evicted |
| 2 | **Accessed buffer promoted in LRU** | Read → buffer becomes newest |
| 3 | **Remove non-cached file** | No-op, no panic |
| 4 | **Flush non-cached file** | No-op, no panic |
| 5 | **Concurrent read/write same buffer** | No data corruption |
| 6 | **Truncate beyond current size** | Extends with zeros |
| 7 | **Read at exact EOF** | Returns empty |
| 8 | **Write at exact EOF** | Appends |

---

## Фаза 6: Git backend — дополнительные сценарии (средний приоритет)

### 6.1 Git backend integration

**Файл**: `crates/omnifuse-git/tests/backend_integration.rs` (дополнение)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Clone remote repo → init** | GitBackend.init() с URL → clone |
| 2 | **Sync: commit + push** | dirty files → git add → commit → push |
| 3 | **Sync: push failure → retry** | Push fails → retry logic |
| 4 | **Poll remote: new commits detected** | Remote has new commits → RemoteChange detected |
| 5 | **Apply remote: pull new commits** | RemoteChange → git pull → files updated |
| 6 | **should_track: .gitignore patterns** | Complex patterns (directory, negation) |
| 7 | **should_track: nested .gitignore** | .gitignore in subdirectory |
| 8 | **is_online(): remote reachable** | Network check |
| 9 | **is_online(): remote unreachable** | Network down → false |
| 10 | **Sync: merge conflict with remote** | Local + remote changes to same file → conflict |

---

## Фаза 7: Wiki backend — дополнительные сценарии (средний приоритет)

### 7.1 Wiki backend integration

**Файл**: `crates/omnifuse-wiki/tests/backend_integration.rs` (дополнение)

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Sync: page rename (slug changed)** | File renamed → old page deleted, new created |
| 2 | **Sync: non-md file ignored** | .txt file → should_track = false |
| 3 | **Poll: page deleted on server** | RemoteChange::Deleted → file removed from disk |
| 4 | **Poll: new page on server** | RemoteChange::Modified (new) → file created |
| 5 | **Apply remote: base version updated** | apply_remote → .vfs/base/ updated |
| 6 | **Conflict: three-way merge success** | Local + remote changes, different lines → auto-merge |
| 7 | **Conflict: three-way merge failure** | Same line changed → Conflict result |
| 8 | **Init: empty wiki (no pages)** | Empty tree → graceful handling |
| 9 | **Init: large wiki (500+ pages)** | max_pages limit respected |
| 10 | **is_online(): API unreachable** | Network down → false |

---

## Фаза 8: GUI (низкий приоритет)

### 8.1 GUI: Rust-часть (Tauri commands)

**Файл**: `crates/omnifuse-gui/tauri/src/lib.rs` + тесты

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Tauri command: mount_git** | Command accepts params, creates config |
| 2 | **Tauri command: mount_wiki** | Command accepts params, creates config |
| 3 | **Tauri command: unmount** | Graceful unmount |
| 4 | **Tauri command: get_status** | Returns current mount status |
| 5 | **Tauri command: get_logs** | Returns sync logs |

### 8.2 GUI: веб-часть (React)

**Файл**: `crates/omnifuse-gui/web/src/` + тесты

| # | Сценарий | Что проверяет |
|---|----------|---------------|
| 1 | **Folder picker component** | User selects mount point |
| 2 | **Mount form: git** | Form validation, submission |
| 3 | **Mount form: wiki** | Form validation, submission |
| 4 | **Status display** | Shows mounted backends, sync status |
| 5 | **Log viewer** | Displays sync logs in real-time |

---

## Сводка по объёму

| Фаза | Приоритет | Новых тестов (оценка) | Файлов |
|------|-----------|----------------------|--------|
| 1. E2E пользовательские сценарии | 🔴 High | ~25 | 3 |
| 2. CLI расширенные | 🔴 High | ~15 | 1 (existing) |
| 3. Конфигурация | 🟡 Medium | ~10 | 1 |
| 4. Edge-кейсы | 🟡 Medium | ~20 | 3 |
| 5. VFS operations | 🟡 Medium | ~18 | 2 (existing) |
| 6. Git backend | 🟡 Medium | ~10 | 1 (existing) |
| 7. Wiki backend | 🟡 Medium | ~10 | 1 (existing) |
| 8. GUI | 🟢 Low | ~10 | TBD |
| **Итого** | | **~118 новых тестов** | **~10 файлов** |

---

## Порядок выполнения

1. **Фаза 1** — E2E сценарии (самое важное для пользователя)
2. **Фаза 2** — CLI (быстро, много coverage за мало усилий)
3. **Фаза 3** — Конфигурация (критичная инфраструктура)
4. **Фаза 4** — Edge-кейсы (надёжность)
5. **Фаза 5-7** — Дополнение существующих тестов
6. **Фаза 8** — GUI (отдельный большой проект)
