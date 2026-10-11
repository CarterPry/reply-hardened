mod commands;
pub mod config;
pub mod contacts;
pub mod error;
pub mod pending;
pub mod replies;
pub mod safestorage;
pub mod signal;
pub mod sources;
pub mod state;
pub mod unknown;
pub mod whatsapp;

use commands::SourceStatusStore;
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
            let state_path = data_dir.join("state.db");
            // One-time carry-over from the pre-rename bundle id
            // (`local.codex.repli.hardened`): the data dir is keyed by
            // identifier, so without this the done/snoozed/muted
            // history would silently start from empty after the rename.
            if !state_path.exists() {
                if let Some(parent) = data_dir.parent() {
                    let legacy = parent.join("local.codex.repli.hardened").join("state.db");
                    if legacy.exists() {
                        match std::fs::copy(&legacy, &state_path) {
                            Ok(_) => log::info!("copied state.db from legacy data dir"),
                            Err(e) => log::warn!("could not copy legacy state.db: {e}"),
                        }
                    }
                }
            }
            let store =
                StateStore::open(&state_path).map_err(|e| format!("opening state db: {e}"))?;
            app.manage(store);
            // Per-source read status, populated on every poll.
            app.manage(SourceStatusStore::default());

            let quit = MenuItem::with_id(app, "quit", "Quit Reply Hardened", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;

            // The app icon is a filled green tile, which a template
            // image would flatten into a solid square. The menu bar
            // gets its own monochrome bubble silhouette instead.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                .map_err(|e| format!("decoding tray icon: {e}"))?;
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
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
        // Closing the window hides it - the app lives in the menu bar.
        // Quit via the tray menu. Losing focus also hides it (standard
        // menu-bar drop-down behavior); disabled in dev builds so
        // devtools focus doesn't constantly dismiss the window.
        //
        // The blur-hide only arms after the window has actually been
        // focused once. An Accessory app launched from Finder often
        // never wins focus, so without the gate the first-launch
        // window flashed and hid before the user saw it.
        .on_window_event(|window, event| {
            use std::sync::atomic::{AtomicBool, Ordering};
            static EVER_FOCUSED: AtomicBool = AtomicBool::new(false);
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                tauri::WindowEvent::Focused(true) => {
                    EVER_FOCUSED.store(true, Ordering::Relaxed);
                }
                tauri::WindowEvent::Focused(false)
                    if !cfg!(debug_assertions) && EVER_FOCUSED.load(Ordering::Relaxed) =>
                {
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_replies,
            commands::get_sources,
            commands::get_unknown_senders,
            commands::add_to_contacts,
            commands::mark_done,
            commands::snooze_chats,
            commands::mute_chats,
            commands::undo_last,
            commands::unmute_chat,
            commands::list_muted,
            commands::open_fda_settings,
            commands::open_accessibility_settings,
            commands::open_automation_settings,
            commands::open_chat,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // Re-launching the app (Finder, Launchpad, `open`) while it's
        // already running lands here — re-show the window instead of
        // doing nothing, since a menu-bar app has no dock icon to
        // signal "it's already open".
        .run(|app, event| {
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        });
}
