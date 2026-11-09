// SPDX-License-Identifier: GPL-3.0-or-later
//! Vayu desktop shell (Phase 0): renders `packages/ui` on the mock daemon,
//! remembers window size/position, and shows a tray icon whose tooltip the
//! UI keeps updated with the total download speed.
//!
//! Phase 1 links the engine in-process and points the UI at the daemon's
//! WebSocket API instead of the mock.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

/// Tray icon id, so commands can find it again.
pub const TRAY_ID: &str = "vayu-tray";
const MAIN_WINDOW: &str = "main";

/// Longest tooltip we pass on; Windows truncates at 127 UTF-16 units anyway.
const MAX_TOOLTIP_CHARS: usize = 120;

/// Called by the UI about once a second with e.g. "Vayu — ↓ 12.5 MB/s · 3 active".
#[tauri::command]
fn set_tray_tooltip<R: Runtime>(app: AppHandle<R>, text: String) -> Result<(), String> {
    let text: String = text.chars().take(MAX_TOOLTIP_CHARS).collect();
    match app.tray_by_id(TRAY_ID) {
        Some(tray) => tray.set_tooltip(Some(text)).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        // Best effort: a failure here leaves the window as it was.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Vayu", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Vayu")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Entry point shared by the binary (and, later, the mobile targets).
pub fn run() {
    tauri::Builder::default()
        // Restores size, position and maximised state; saves them on exit.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            build_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![set_tray_tooltip])
        .run(tauri::generate_context!())
        .unwrap_or_else(|err| {
            eprintln!("vayu-desktop: failed to start: {err}");
            std::process::exit(1);
        });
}
