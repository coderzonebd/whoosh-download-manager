// SPDX-License-Identifier: GPL-3.0-or-later
//! Whoosh desktop shell (Phase 0): renders `packages/ui` on the mock daemon,
//! remembers window size, position and maximised state, and shows a tray
//! icon whose tooltip the UI keeps updated with the total download speed.
//!
//! Phase 1 links the engine in-process and points the UI at the daemon's
//! WebSocket API instead of the mock.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, WindowEvent};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

mod window_size;

/// Tray icon id, so commands can find it again.
pub const TRAY_ID: &str = "whoosh-tray";
const MAIN_WINDOW: &str = "main";

/// Longest tooltip we pass on; Windows truncates at 127 UTF-16 units anyway.
const MAX_TOOLTIP_CHARS: usize = 120;

/// The plugin restores position and maximised state; size is ours (see `window_size`).
fn window_state_flags() -> StateFlags {
    StateFlags::all() - StateFlags::SIZE
}

/// Called by the UI about once a second with e.g. "Whoosh — ↓ 12.5 MB/s · 3 active".
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

/// Quit from the tray. The window state is saved first, while the window
/// still exists: once `exit()` starts, the window-state plugin can only fall
/// back to sizes from resize events, which on GTK include client-side
/// decorations — the window would grow a little on every launch.
fn save_state_and_quit<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        window_size::save(&window);
    }
    if let Err(err) = app.save_window_state(window_state_flags()) {
        eprintln!("whoosh-desktop: could not save window state: {err}");
    }
    app.exit(0);
}

fn build_tray<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Whoosh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Whoosh")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => save_state_and_quit(app),
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
        // Position and maximised state; the size is handled by `window_size`.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(|app| {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                window_size::restore(&window);
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { .. } = event {
                        window_size::save(&w);
                    }
                });
            }
            build_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![set_tray_tooltip])
        .run(tauri::generate_context!())
        .unwrap_or_else(|err| {
            eprintln!("whoosh-desktop: failed to start: {err}");
            std::process::exit(1);
        });
}
