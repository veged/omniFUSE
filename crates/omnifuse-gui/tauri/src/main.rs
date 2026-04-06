//! `OmniFuse` GUI
//!
//! Graphical interface for `OmniFuse` based on Tauri.
//! Supports git and wiki backends.

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod commands;
mod events;

use std::sync::Arc;

use commands::AppState;
use omnifuse_core::LoggingConfig;

/// Entry point for the GUI application.
fn main() -> Result<(), Box<dyn std::error::Error>> {
  omnifuse_core::init_logging(&LoggingConfig::default())?;

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
