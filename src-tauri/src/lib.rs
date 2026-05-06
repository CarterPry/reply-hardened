mod commands;
mod contacts;
mod error;
mod pending;
mod state;

use state::StateStore;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolving app data dir: {e}"))?;
            std::fs::create_dir_all(&data_dir).ok();
            let store = StateStore::open(&data_dir.join("state.db"))
                .map_err(|e| format!("opening state db: {e}"))?;
            app.manage(store);

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_pending_replies,
            commands::refresh,
            commands::mark_complete,
            commands::open_fda_settings,
            commands::open_chat,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
