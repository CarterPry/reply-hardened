mod commands;
pub mod config;
pub mod contacts;
pub mod error;
pub mod pending;
pub mod replies;
pub mod state;

use state::StateStore;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tauri_plugin_positioner::{Position, WindowExt};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_positioner::init())
        .setup(|app| {
            // Menu bar app: no dock icon, no cmd-tab entry. The tray
            // icon is the app's presence; the window is a drop-down.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolving app data dir: {e}"))?;
            std::fs::create_dir_all(&data_dir).ok();
            let store = StateStore::open(&data_dir.join("state.db"))
                .map_err(|e| format!("opening state db: {e}"))?;
            app.manage(store);

            let quit = MenuItem::with_id(
                app,
                "quit",
                "Quit Repli",
                true,
                None::<&str>,
            )?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::with_id("main")
                .icon(
                    app.default_window_icon()
                        .expect("bundle has a default window icon")
                        .clone(),
                )
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    // Feed the positioner so Position::TrayCenter knows
                    // where the icon is.
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        let Some(win) = app.get_webview_window("main") else {
                            return;
                        };
                        // Toggle on visibility alone. Checking focus too
                        // misfires under the Accessory activation policy
                        // (the window often reports unfocused), which made
                        // tray clicks re-show instead of hide.
                        if win.is_visible().unwrap_or(false) {
                            let _ = win.hide();
                        } else {
                            let _ = win.move_window(Position::TrayCenter);
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Show the window on first launch so the app doesn't appear
            // to do nothing (and so the FDA prompt is visible).
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        // Closing the window hides it — the app lives in the menu bar.
        // Quit via the tray menu. Losing focus also hides it (standard
        // menu-bar drop-down behavior); disabled in dev builds so
        // devtools focus doesn't constantly dismiss the window.
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Focused(false) if !cfg!(debug_assertions) => {
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_replies,
            commands::mark_complete,
            commands::snooze_chat,
            commands::mute_chat,
            commands::unmute_chat,
            commands::list_muted,
            commands::open_fda_settings,
            commands::open_accessibility_settings,
            commands::open_automation_settings,
            commands::open_chat,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
