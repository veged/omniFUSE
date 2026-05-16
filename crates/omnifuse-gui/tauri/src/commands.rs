//! Tauri commands for VFS management.

use std::{path::PathBuf, sync::Arc};

use omnifuse_app::{GitMountArgs, MountService, S3MountArgs, WikiMountArgs};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use crate::events::TauriEventHandler;

/// Application state.
pub struct AppState {
  /// Flag indicating whether the VFS is mounted.
  pub mounted: Mutex<bool>,
  /// Cancellation token for unmounting.
  pub cancel_token: Mutex<Option<tokio::sync::oneshot::Sender<()>>>
}

impl AppState {
  /// Create a new state.
  pub fn new() -> Self {
    Self {
      mounted: Mutex::new(false),
      cancel_token: Mutex::new(None)
    }
  }
}

/// Check FUSE platform availability.
#[tauri::command]
#[allow(clippy::unnecessary_wraps)] // Tauri command requires Result
pub fn check_fuse() -> Result<bool, String> {
  Ok(omnifuse_core::is_fuse_available())
}

/// Mount a git backend.
#[tauri::command]
#[allow(clippy::similar_names)] // mount_point (param) vs mnt (local) — different things
pub async fn mount_git(
  source: String,
  mount_point: String,
  branch: Option<String>,
  app: AppHandle,
  state: State<'_, Arc<AppState>>
) -> Result<(), String> {
  let mut mounted = state.mounted.lock().await;
  if *mounted {
    return Err("Already mounted".to_string());
  }

  let mnt = PathBuf::from(&mount_point);

  let events = TauriEventHandler::new(app);
  let args = GitMountArgs {
    source,
    mount_point: mnt,
    branch,
    poll_interval_secs: Some(30),
    allow_other: false,
    read_only: false
  };
  let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

  *state.cancel_token.lock().await = Some(cancel_tx);
  *mounted = true;
  drop(mounted);

  let state_clone = Arc::clone(&state);

  tokio::spawn(async move {
    let service = MountService::default();
    tokio::select! {
        result = service.run_git(args, events) => {
            if let Err(e) = result {
                tracing::error!("VFS error: {e}");
            }
        }
        () = async { let _ = cancel_rx.await; } => {
            tracing::info!("VFS cancelled");
        }
    }

    *state_clone.mounted.lock().await = false;
  });

  Ok(())
}

/// Mount a wiki backend.
#[tauri::command]
#[allow(clippy::similar_names)] // mount_point (param) vs mnt (local) — different things
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
    return Err("Already mounted".to_string());
  }

  let mnt = PathBuf::from(&mount_point);

  let events = TauriEventHandler::new(app);
  let args = WikiMountArgs {
    base_url,
    root_slug,
    auth_token,
    org_id: None,
    mount_point: mnt,
    poll_interval_secs: Some(60),
    allow_other: false,
    read_only: false
  };
  let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

  *state.cancel_token.lock().await = Some(cancel_tx);
  *mounted = true;
  drop(mounted);

  let state_clone = Arc::clone(&state);

  tokio::spawn(async move {
    let service = MountService::default();
    tokio::select! {
        result = service.run_wiki(args, events) => {
            if let Err(e) = result {
                tracing::error!("VFS error: {e}");
            }
        }
        () = async { let _ = cancel_rx.await; } => {
            tracing::info!("VFS cancelled");
        }
    }

    *state_clone.mounted.lock().await = false;
  });

  Ok(())
}

/// Mount an S3-compatible backend.
#[tauri::command]
#[allow(clippy::similar_names)]
#[allow(clippy::too_many_arguments)]
pub async fn mount_s3(
  bucket: String,
  prefix: Option<String>,
  endpoint: Option<String>,
  region: Option<String>,
  access_key_id: Option<String>,
  secret_access_key: String,
  mount_point: String,
  app: AppHandle,
  state: State<'_, Arc<AppState>>
) -> Result<(), String> {
  let mut mounted = state.mounted.lock().await;
  if *mounted {
    return Err("Already mounted".to_string());
  }

  let mnt = PathBuf::from(&mount_point);

  let events = TauriEventHandler::new(app);
  let args = S3MountArgs {
    bucket,
    mount_point: mnt,
    prefix,
    endpoint,
    region,
    access_key_id,
    secret_access_key: Some(secret_access_key),
    session_token: None,
    virtual_host_style: false,
    poll_interval_secs: Some(60),
    allow_other: false,
    read_only: false
  };
  let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

  *state.cancel_token.lock().await = Some(cancel_tx);
  *mounted = true;
  drop(mounted);

  let state_clone = Arc::clone(&state);

  tokio::spawn(async move {
    let service = match MountService::with_default_cache() {
      Ok(svc) => svc,
      Err(error) => {
        tracing::warn!(%error, "failed to initialize persistent cache; mounting without it");
        MountService::default()
      }
    };
    tokio::select! {
        result = service.run_s3(args, events) => {
            if let Err(e) = result {
                tracing::error!("VFS error: {e}");
            }
        }
        () = async { let _ = cancel_rx.await; } => {
            tracing::info!("VFS cancelled");
        }
    }

    *state_clone.mounted.lock().await = false;
  });

  Ok(())
}

/// Unmount the VFS.
#[tauri::command]
pub async fn unmount(state: State<'_, Arc<AppState>>) -> Result<(), String> {
  let mut mounted = state.mounted.lock().await;
  if !*mounted {
    return Err("Not mounted".to_string());
  }

  let cancel = state.cancel_token.lock().await.take();
  if let Some(tx) = cancel {
    let _ = tx.send(());
  }

  *mounted = false;
  drop(mounted);
  Ok(())
}

/// Pick a folder using the system dialog.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
  let result = app.dialog().file().blocking_pick_folder();
  Ok(result.map(|path| path.to_string()))
}
