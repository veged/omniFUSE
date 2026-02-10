//! `OmniFuse` GUI
//!
//! Графический интерфейс для `OmniFuse` на базе Tauri.
//! Поддерживает git и wiki backend'ы.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod commands;
mod events;

use std::sync::Arc;

use commands::AppState;

/// Точка входа для GUI приложения.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            commands::check_fuse,
            commands::mount_git,
            commands::mount_wiki,
            commands::unmount,
            commands::pick_folder,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
