// SPDX-License-Identifier: GPL-3.0-or-later
//! Whoosh desktop app: `packages/ui` over the whoosh daemon (ADR 0007).
//!
//! The UI talks to the daemon over its WebSocket API and asks this side for
//! the address and token ([`daemon::daemon_connection`]), which starts the
//! bundled daemon when none is running. One instance per user: a second
//! launch shows the first one's window. Closing the window hides it to the
//! tray while downloads go on; Quit leaves the daemon running unless the
//! user asked otherwise ([`prefs`]). Window size, position and maximised
//! state are remembered. `whoosh://add?url=…` links open the add dialog
//! ([`deep_links`]); launch at login is off until the user turns it on, and
//! then starts hidden in the tray ([`AUTOSTART_ARG`]).

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

mod daemon;
mod deep_links;
mod prefs;
mod shell_i18n;
mod window_size;

/// Sent to the UI when closing the window hid it to the tray, so it can say
/// so once (in the user's language): `{ "first": bool }`.
pub const EVENT_HIDDEN_TO_TRAY: &str = "desktop://hidden-to-tray";

/// Whether the tray icon exists. Without one (a Linux desktop with no status
/// notifier), closing the window quits, or the app could not be reached.
static TRAY_SHOWN: AtomicBool = AtomicBool::new(false);
/// Set once Quit has started, so closing the window then really closes it.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Passed by the login item (`tauri-plugin-autostart`): start in the tray,
/// without showing the window.
pub const AUTOSTART_ARG: &str = "--autostart";

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

/// Where the language picker's "help improve the translation" link may go.
const TRANSLATION_SITE: &str = "https://hosted.weblate.org/";

/// Opens a page of the translation site in the user's browser. Only that
/// site: the webview must not become a way to launch arbitrary URLs.
#[tauri::command]
fn open_translation_page(url: String) -> Result<(), String> {
    let ok = url.starts_with(TRANSLATION_SITE)
        && url
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b));
    if !ok {
        return Err(format!("not a translation page: {url}"));
    }
    open_in_browser(&url).map_err(|e| format!("could not open the browser: {e}"))
}

fn open_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    // Not waited for here: reaped on a thread so no zombie is left behind.
    let mut child = cmd.arg(url).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        // Best effort: a failure here leaves the window as it was.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn save_window_state<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        window_size::save(&window);
    }
    if let Err(err) = app.save_window_state(window_state_flags()) {
        eprintln!("whoosh-desktop: could not save window state: {err}");
    }
}

/// Quit from the tray. The window state is saved first, while the window
/// still exists: once `exit()` starts, the window-state plugin can only fall
/// back to sizes from resize events, which on GTK include client-side
/// decorations — the window would grow a little on every launch. The daemon
/// keeps running unless the user asked Quit to stop it.
fn quit_app<R: Runtime>(app: &AppHandle<R>) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    save_window_state(app);
    if !app.state::<prefs::PrefsState>().get().stop_daemon_on_quit {
        app.exit(0);
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let stopped = match daemon::data_dir() {
            Ok(dir) => daemon::shutdown(&dir).await,
            Err(e) => Err(e),
        };
        if let Err(err) = stopped {
            eprintln!("whoosh-desktop: could not stop the daemon: {err}");
        }
        app.exit(0);
    });
}

/// Closing the window: hide it to the tray (downloads go on) when there is
/// a tray and the user wants that; otherwise let it close, which quits.
fn on_close_requested<R: Runtime>(window: &tauri::WebviewWindow<R>, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    let hide = TRAY_SHOWN.load(Ordering::SeqCst)
        && !QUITTING.load(Ordering::SeqCst)
        && app.state::<prefs::PrefsState>().get().close_to_tray;
    if !hide {
        save_window_state(app);
        return;
    }
    api.prevent_close();
    window_size::save(window);
    let _ = window.hide();
    let first = !app.state::<prefs::PrefsState>().get().tray_notice_shown;
    if first && let Err(err) = prefs::update(app, |p| p.tray_notice_shown = true) {
        eprintln!("whoosh-desktop: could not save desktop.json: {err}");
    }
    let _ = app.emit(EVENT_HIDDEN_TO_TRAY, serde_json::json!({ "first": first }));
}

fn build_tray<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    // In the language the UI last used; the UI updates it (`shell_i18n`).
    let text = shell_i18n::strings(&app.state::<prefs::PrefsState>().get().language);
    let show = MenuItem::with_id(app, "show", &text.show, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", &text.quit, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    app.manage(shell_i18n::TrayItems {
        show: show.clone(),
        quit: quit.clone(),
    });

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Whoosh")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => quit_app(app),
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
    TRAY_SHOWN.store(true, Ordering::SeqCst);
    Ok(())
}

/// Entry point shared by the binary (and, later, the mobile targets).
pub fn run() {
    let app = tauri::Builder::default()
        // First, so a second launch hands over before anything else starts.
        // With its `deep-link` feature, a second launch's whoosh:// link
        // reaches `deep_links` through the deep-link plugin.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        // Position and maximised state; the size is handled by `window_size`.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(|app| {
            let loaded = prefs::load(app.handle());
            app.manage(prefs::PrefsState(std::sync::Mutex::new(loaded)));
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                window_size::restore(&window);
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        on_close_requested(&w, api);
                    }
                });
            }
            // No tray is not fatal: closing the window then quits.
            if let Err(err) = build_tray(app) {
                eprintln!("whoosh-desktop: no tray icon: {err}");
            }
            deep_links::setup(app);
            // Started at login: stay in the tray (when there is one).
            if TRAY_SHOWN.load(Ordering::SeqCst)
                && std::env::args().any(|a| a == AUTOSTART_ARG)
                && let Some(window) = app.get_webview_window(MAIN_WINDOW)
            {
                let _ = window.hide();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_tray_tooltip,
            daemon::daemon_connection,
            deep_links::take_deep_links,
            prefs::desktop_prefs,
            prefs::set_desktop_prefs,
            shell_i18n::set_shell_language,
            open_translation_page,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|err| {
            eprintln!("whoosh-desktop: failed to start: {err}");
            std::process::exit(1);
        });
    app.run(|app, event| {
        // macOS: clicking the Dock icon brings back a window hidden to the tray.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main_window(app);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app, event);
    });
}
