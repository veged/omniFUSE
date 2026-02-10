//! Tauri commands для управления VFS.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use crate::events::TauriEventHandler;

/// Состояние приложения.
pub struct AppState {
    /// Флаг, смонтирована ли VFS.
    pub mounted: Mutex<bool>,
    /// Токен для отмены монтирования.
    pub cancel_token: Mutex<Option<tokio::sync::oneshot::Sender<()>>>
}

impl AppState {
    /// Создать новое состояние.
    pub fn new() -> Self {
        Self {
            mounted: Mutex::new(false),
            cancel_token: Mutex::new(None)
        }
    }
}

/// Проверить доступность платформы FUSE.
#[tauri::command]
#[allow(clippy::unnecessary_wraps)] // Tauri command требует Result
pub fn check_fuse() -> Result<bool, String> {
    Ok(omnifuse_core::is_fuse_available())
}

/// Смонтировать git backend.
#[tauri::command]
#[allow(clippy::similar_names)] // mount_point (param) vs mnt (local) — разные вещи
pub async fn mount_git(
    source: String,
    mount_point: String,
    branch: Option<String>,
    app: AppHandle,
    state: State<'_, Arc<AppState>>
) -> Result<(), String> {
    let mut mounted = state.mounted.lock().await;
    if *mounted {
        return Err("Уже смонтировано".to_string());
    }

    let mnt = PathBuf::from(&mount_point);

    let git_config = omnifuse_git::GitConfig {
        source: source.clone(),
        branch: branch.unwrap_or_else(|| "main".to_string()),
        max_push_retries: 10,
        poll_interval_secs: 30
    };

    let git_backend = omnifuse_git::GitBackend::new(git_config);

    let mount_config = omnifuse_core::MountConfig {
        mount_point: mnt.clone(),
        local_dir: mnt.clone(),
        sync: omnifuse_core::SyncConfig::default(),
        buffer: omnifuse_core::BufferConfig::default(),
        mount_options: omnifuse_core::FuseMountOptions {
            fs_name: "omnifuse-git".to_string(),
            allow_other: false,
            read_only: false
        },
        logging: omnifuse_core::LoggingConfig::default()
    };

    let _ = app.emit(
        "vfs:log",
        serde_json::json!({
            "level": "info",
            "message": format!("запуск монтирования: {} -> {}", source, mnt.display())
        })
    );

    let events = TauriEventHandler::new(app.clone());
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

    *state.cancel_token.lock().await = Some(cancel_tx);
    *mounted = true;
    drop(mounted);

    let state_clone = Arc::clone(&state);

    tokio::spawn(async move {
        tokio::select! {
            result = omnifuse_core::run_mount(mount_config, git_backend, events) => {
                if let Err(e) = result {
                    tracing::error!("VFS error: {e}");
                    let _ = app.emit("vfs:error", serde_json::json!({
                        "message": e.to_string()
                    }));
                    let _ = app.emit("vfs:unmounted", ());
                }
            }
            () = async { let _ = cancel_rx.await; } => {
                tracing::info!("VFS cancelled");
                let _ = app.emit("vfs:unmounted", ());
            }
        }

        *state_clone.mounted.lock().await = false;
    });

    Ok(())
}

/// Смонтировать wiki backend.
#[tauri::command]
#[allow(clippy::similar_names)] // mount_point (param) vs mnt (local) — разные вещи
pub async fn mount_wiki(
    base_url: String,
    root_slug: String,
    auth_token: String,
    mount_point: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>
) -> Result<(), String> {
    let mut mounted = state.mounted.lock().await;
    if *mounted {
        return Err("Уже смонтировано".to_string());
    }

    let mnt = PathBuf::from(&mount_point);

    let wiki_config = omnifuse_wiki::WikiConfig {
        base_url: base_url.clone(),
        auth_token,
        root_slug: root_slug.clone(),
        poll_interval_secs: 60,
        max_depth: 10,
        max_pages: 500
    };

    let wiki_backend = omnifuse_wiki::WikiBackend::new(wiki_config)
        .map_err(|e| format!("ошибка создания wiki backend: {e}"))?;

    let mount_config = omnifuse_core::MountConfig {
        mount_point: mnt.clone(),
        local_dir: mnt.clone(),
        sync: omnifuse_core::SyncConfig::default(),
        buffer: omnifuse_core::BufferConfig::default(),
        mount_options: omnifuse_core::FuseMountOptions {
            fs_name: "omnifuse-wiki".to_string(),
            allow_other: false,
            read_only: false
        },
        logging: omnifuse_core::LoggingConfig::default()
    };

    let _ = app.emit(
        "vfs:log",
        serde_json::json!({
            "level": "info",
            "message": format!("запуск монтирования: {} ({}) -> {}", base_url, root_slug, mnt.display())
        })
    );

    let events = TauriEventHandler::new(app.clone());
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

    *state.cancel_token.lock().await = Some(cancel_tx);
    *mounted = true;
    drop(mounted);

    let state_clone = Arc::clone(&state);

    tokio::spawn(async move {
        tokio::select! {
            result = omnifuse_core::run_mount(mount_config, wiki_backend, events) => {
                if let Err(e) = result {
                    tracing::error!("VFS error: {e}");
                    let _ = app.emit("vfs:error", serde_json::json!({
                        "message": e.to_string()
                    }));
                    let _ = app.emit("vfs:unmounted", ());
                }
            }
            () = async { let _ = cancel_rx.await; } => {
                tracing::info!("VFS cancelled");
                let _ = app.emit("vfs:unmounted", ());
            }
        }

        *state_clone.mounted.lock().await = false;
    });

    Ok(())
}

/// Размонтировать VFS.
#[tauri::command]
pub async fn unmount(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut mounted = state.mounted.lock().await;
    if !*mounted {
        return Err("Не смонтировано".to_string());
    }

    let cancel = state.cancel_token.lock().await.take();
    if let Some(tx) = cancel {
        let _ = tx.send(());
    }

    *mounted = false;
    drop(mounted);
    Ok(())
}

/// Выбрать папку через системный диалог.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let result = app.dialog().file().blocking_pick_folder();
    Ok(result.map(|path| path.to_string()))
}
