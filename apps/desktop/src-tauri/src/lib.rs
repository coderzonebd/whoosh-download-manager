// SPDX-License-Identifier: GPL-3.0-or-later
//! WDM desktop app: `packages/ui` over the wdm daemon (ADR 0007).
//!
//! The app is two processes (see `crates/tray`): this one shows the windows
//! and runs only while one is open; `wdm-tray`, a small process with no web
//! view, holds the tray icon, starts the daemon and keeps it running, and
//! opens this app again when it is wanted. This one starts the tray when it
//! is not running.
//!
//! The UI talks to the daemon over its WebSocket API and asks this side for
//! the address and token ([`daemon::daemon_connection`]), which starts the
//! bundled daemon when none is running. One instance per user: a second
//! launch shows the first one's window. Closing the window leaves WDM in the
//! tray while downloads go on: this process ends, so the window and its web
//! page take no memory. Quit (in the tray, or closing the window when the
//! user does not keep WDM in the tray) closes WDM completely. Window size,
//! position and maximised state are remembered. `whoosh://add?url=…` links
//! open the add dialog ([`deep_links`]); launch at login is turned on once,
//! at the first start of an installed build, and starts only the tray
//! ([`wdm_tray::AUTOSTART_ARG`]); Settings turns it off for good. What the
//! browser hands over (a download that waits for confirmation, a video page
//! for the media picker) is asked about in a small prompt window of its
//! own, never in the main one ([`prompt`]). On macOS ⌘Q keeps WDM in the
//! menu bar like closing the window does ([`macos_menu`]). The browsers are
//! connected at start where no installer did it ([`browsers`]), and the app
//! updates itself from signed GitHub releases ([`updates`]; with no window
//! open the tray has it look once a day, `--update-check`).

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime, WindowEvent};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};
use wdm_tray::i18n::{self, Notice};
use wdm_tray::locks;

mod browsers;
mod crash;
mod daemon;
mod deep_links;
mod files;
mod login_item;
#[cfg(target_os = "macos")]
mod macos_menu;
mod prefs;
mod prompt;
mod shell_language;
mod updates;
mod watch;
mod window_size;

/// Set once Quit has started, so closing the window then really closes it.
pub(crate) static QUITTING: AtomicBool = AtomicBool::new(false);
/// Set while a look for an update runs with no window (`--update-check`),
/// or an update is being installed: the app does not end with its last
/// window then.
pub(crate) static UPDATE_BUSY: AtomicBool = AtomicBool::new(false);

const MAIN_WINDOW: &str = "main";

/// The app runs while a window is open; the tray leaves the notifications
/// and the prompt window to it meanwhile ([`wdm_tray::locks::APP`]).
#[derive(Debug)]
struct AppLock(#[allow(dead_code)] Option<std::fs::File>);

/// The plugin restores position and maximised state; size is ours (see `window_size`).
fn window_state_flags() -> StateFlags {
    StateFlags::all() - StateFlags::SIZE
}

/// A system notification, in the UI's language.
pub(crate) fn notify<R: Runtime>(app: &AppHandle<R>, which: &Notice<'_>) {
    use tauri_plugin_notification::NotificationExt;
    let language = app.state::<prefs::PrefsState>().get().language;
    let (title, body) = i18n::notice(&language, which);
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        eprintln!("wdm-desktop: could not show a notification: {err}");
    }
}

/// The main window is open and in front (it then says things itself).
pub(crate) fn main_window_in_front<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(MAIN_WINDOW).is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
    })
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
    // From an AppImage, xdg-open and the browser must not load the image's bundled libraries.
    wdm_proto::outside_appimage!(cmd);
    // Not waited for here: reaped on a thread so no zombie is left behind.
    let mut child = cmd.arg(url).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The UI (the prompt window) asks for the main window. Async: making a
/// window inside a synchronous command deadlocks on Windows.
#[tauri::command]
async fn show_window<R: Runtime>(app: AppHandle<R>) {
    show_main_window(&app);
}

/// Shows the main window, making it again when it was closed to the tray.
pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    // Windows are made on the main thread.
    let _ = app.run_on_main_thread(move || {
        // Back in the Dock while the window is open.
        #[cfg(target_os = "macos")]
        let _ = handle.set_activation_policy(tauri::ActivationPolicy::Regular);
        let window = match handle.get_webview_window(MAIN_WINDOW) {
            Some(window) => window,
            None => match create_main_window(&handle) {
                Ok(window) => window,
                Err(err) => {
                    eprintln!("wdm-desktop: could not open the window: {err}");
                    return;
                }
            },
        };
        // Best effort: a failure here leaves the window as it was.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    });
}

/// Makes the main window (hidden) from its entry in `tauri.conf.json`
/// (`"create": false`, so it is made here), at its saved size and position.
fn create_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::WebviewWindow<R>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN_WINDOW)
        .cloned()
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    // The window-state plugin puts it where it was as it is made.
    let window = tauri::WebviewWindowBuilder::from_config(app, &config)?
        .visible(false)
        .build()?;
    window_size::restore(&window);
    let w = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            on_close_requested(&w, api);
        }
    });
    Ok(window)
}

fn save_window_state<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        window_size::save(&window);
    }
    if let Err(err) = app.save_window_state(window_state_flags()) {
        eprintln!("wdm-desktop: could not save window state: {err}");
    }
}

/// Quit (closing the window when the user does not keep WDM in the tray, or
/// the macOS app menu's Quit then; the tray's Quit does the same from the
/// tray). The window state is saved first, while the window
/// still exists: once `exit()` starts, the window-state plugin can only fall
/// back to sizes from resize events, which on GTK include client-side
/// decorations — the window would grow a little on every launch. Quit
/// closes WDM completely: the daemon stops, and a mark in the data directory
/// tells the browser's native host not to start the app for a download until
/// the user opens it again. (Downloads go on in the background only while
/// the app runs in the tray: closing the window keeps it there.)
fn quit_app<R: Runtime>(app: &AppHandle<R>) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    save_window_state(app);
    // A new version downloaded in the background goes in now (it stops the daemon itself).
    if updates::install_on_quit(app) {
        if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
            let _ = window.hide();
        }
        return;
    }
    daemon::mark_quit(true);
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
            eprintln!("wdm-desktop: could not stop the daemon: {err}");
        }
        exit_cleanly(&app).await;
    });
}

/// How long the windows' web processes get to end after their windows close.
const WEBVIEWS_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

/// Ends the app with its windows closed first. Exiting while a window's page
/// is alive made WebKitGTK's web process crash on Linux (SIGSEGV, and a
/// "WebKitWebProcess closed unexpectedly" report on every Quit): the windows
/// are destroyed (no close request, so no hiding to the tray), their web
/// processes get a moment to end, then the app exits.
pub(crate) async fn exit_cleanly<R: Runtime>(app: &AppHandle<R>) {
    for window in app.webview_windows().into_values() {
        let _ = window.destroy();
    }
    tokio::time::sleep(WEBVIEWS_CLOSE_GRACE).await;
    app.exit(0);
}

/// The tray process runs (it holds [`wdm_tray::locks::TRAY`]). Without one
/// (a Linux desktop with no status notifier), closing the window quits.
fn tray_running() -> bool {
    wdm_tray::data_dir().is_some_and(|dir| locks::held(&dir, locks::TRAY))
}

/// Starts the tray process when it is not running.
fn ensure_tray() {
    if !tray_running()
        && let Err(err) = wdm_tray::launch::start_tray()
    {
        eprintln!("wdm-desktop: could not start the tray: {err}");
    }
}

/// Whether closing the window (or ⌘Q on macOS) keeps WDM in the tray: the
/// tray is there, the user wants that, and the app is not quitting.
fn keeps_running_in_tray<R: Runtime>(app: &AppHandle<R>) -> bool {
    !QUITTING.load(Ordering::SeqCst)
        && app.state::<prefs::PrefsState>().get().close_to_tray
        && tray_running()
}

/// The window is closing and WDM stays in the tray; downloads go on. The
/// window is closed for real, and with the last window this process ends:
/// its web page (WebKit's or WebView2's processes) would otherwise hold
/// some 100 MB while nobody sees it. The tray makes it again. The first
/// time, a notification says WDM is still there.
fn went_to_tray<R: Runtime>(app: &AppHandle<R>) {
    save_window_state(app);
    let first = !app.state::<prefs::PrefsState>().get().tray_notice_shown;
    if first {
        if let Err(err) = prefs::update(app, |p| p.tray_notice_shown = true) {
            eprintln!("wdm-desktop: could not save desktop.json: {err}");
        }
        notify(app, &Notice::InTray);
    }
}

/// Closing the window: WDM stays in the tray (downloads go on) when there
/// is a tray and the user wants that; when the user does not, closing quits
/// WDM like the tray's Quit. With no tray at all the app just ends.
fn on_close_requested<R: Runtime>(window: &tauri::WebviewWindow<R>, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    if QUITTING.load(Ordering::SeqCst) {
        return;
    }
    if keeps_running_in_tray(app) {
        went_to_tray(app);
    } else if tray_running() {
        api.prevent_close();
        quit_app(app);
    } else {
        save_window_state(app);
    }
}

/// ⌘Q, the app menu's Quit (macOS): the same as closing the window when
/// WDM stays in the tray, else a real quit. The tray's Quit always quits.
#[cfg(target_os = "macos")]
fn quit_from_app_menu<R: Runtime>(app: &AppHandle<R>) {
    if keeps_running_in_tray(app) {
        if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
            // Closes it like its close button: `on_close_requested`.
            let _ = window.close();
        }
    } else {
        quit_app(app);
    }
}

/// `wdm-desktop --tray` (or `--autostart`, the login item): become the tray
/// process, with no window. On Linux and macOS this process is replaced by
/// `wdm-tray` (from an AppImage that keeps the image's files there); on
/// Windows the tray is started and this one ends.
fn become_tray() -> ! {
    let tray = std::env::current_exe()
        .map(|exe| wdm_tray::launch::tray_exe_next_to(&exe))
        .unwrap_or_else(|_| std::path::PathBuf::from("wdm-tray"));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&tray).exec();
        eprintln!("wdm-desktop: could not start {}: {err}", tray.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let plan = wdm_tray::launch::Plan {
            program: tray,
            args: Vec::new(),
        };
        if let Err(err) = wdm_tray::launch::spawn(&plan) {
            eprintln!("wdm-desktop: could not start the tray: {err}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }
}

/// Remembers where the bundled media tools are, for the daemon the tray
/// starts (the tray cannot ask Tauri for the resource directory).
fn record_media_tools<R: Runtime>(app: &AppHandle<R>) {
    let dir = daemon::media_tools_dir(app)
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    if app.state::<prefs::PrefsState>().get().media_tools_dir != dir
        && let Err(err) = prefs::update(app, |p| p.media_tools_dir = dir)
    {
        eprintln!("wdm-desktop: could not save desktop.json: {err}");
    }
}

/// Entry point shared by the binary (and, later, the mobile targets).
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|a| a == wdm_tray::TRAY_ARG || a == wdm_tray::AUTOSTART_ARG)
    {
        become_tray();
    }
    let for_update = args.iter().any(|a| a == wdm_tray::UPDATE_CHECK_ARG);
    // Crash reports (on unless turned off), before anything that could panic.
    crash::init();
    let app = tauri::Builder::default()
        // First, so a second launch hands over before anything else starts.
        // With its `deep-link` feature, a second launch's whoosh:// link
        // reaches `deep_links` through the deep-link plugin.
        // `--prompt` (from wdm-nmhost) opens the prompt window instead.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if argv.iter().any(|a| a == wdm_tray::UPDATE_CHECK_ARG) {
                // The tray's daily look while a window is open: the app does it itself.
            } else if prompt::wants_prompt(&argv) {
                if let Err(err) = prompt::open(app) {
                    eprintln!("wdm-desktop: could not open the prompt window: {err}");
                }
            } else {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![wdm_tray::AUTOSTART_ARG]),
        ))
        // Position and maximised state; the size is handled by `window_size`.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(move |app| {
            let data_dir = daemon::data_dir()?;
            app.manage(AppLock(locks::take(&data_dir, locks::APP)));
            // Opened again: the browser may hand downloads to WDM (and start it) again.
            if !for_update {
                daemon::mark_quit(false);
            }
            let loaded = prefs::load(app.handle());
            let first_run = !loaded.browser_setup_shown;
            app.manage(prefs::PrefsState(std::sync::Mutex::new(loaded)));
            app.manage(updates::Pending::default());
            app.manage(updates::Ready::default());
            app.manage(watch::Followed::default());
            app.manage(prefs::ThisRun::new(&data_dir));
            record_media_tools(app.handle());
            // The tray holds the icon and keeps the daemon running.
            ensure_tray();
            std::thread::spawn(move || browsers::ensure_registered(first_run));
            if for_update {
                updates::check_once(app.handle().clone());
            } else {
                updates::start_daily_check(app.handle().clone());
            }
            // Installed builds start at login unless the user turned that off.
            login_item::first_run(app.handle());
            #[cfg(target_os = "macos")]
            {
                let menu = macos_menu::build(app.handle())?;
                app.set_menu(menu)?;
                app.on_menu_event(|app, event| {
                    if event.id().as_ref() == macos_menu::QUIT_ID {
                        quit_from_app_menu(app);
                    }
                });
            }
            watch::start(app.handle(), &QUITTING);
            // Started by the tray for the prompt window or an update check:
            // no main window until it is wanted.
            let for_prompt = prompt::wants_prompt(std::env::args());
            if for_prompt || for_update {
                // In the menu bar, not the Dock.
                #[cfg(target_os = "macos")]
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            } else {
                let window = create_main_window(app.handle())?;
                window.show()?;
                let _ = window.set_focus();
            }
            deep_links::setup(app);
            if for_prompt && let Err(err) = prompt::open(app.handle()) {
                eprintln!("wdm-desktop: could not open the prompt window: {err}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            show_window,
            prompt::open_prompt,
            prompt::close_prompt,
            files::open_download,
            watch::set_followed,
            daemon::daemon_connection,
            deep_links::take_deep_links,
            prefs::desktop_prefs,
            prefs::set_desktop_prefs,
            prefs::this_run,
            prefs::set_this_run,
            shell_language::set_shell_language,
            open_translation_page,
            browsers::browser_status,
            browsers::connect_browser,
            browsers::open_extension_page,
            updates::check_for_update,
            updates::install_update,
            updates::update_source,
            updates::update_notice,
            crash::crash_reports,
            crash::set_crash_reports,
            crash::dismiss_crash_notice,
            crash::report_ui_error,
            login_item::take_login_notice,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|err| {
            eprintln!("wdm-desktop: failed to start: {err}");
            std::process::exit(1);
        });
    app.run(|app, event| match event {
        // The last window closed: the app ends (the tray stays), unless a
        // look for an update is still running.
        tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } if UPDATE_BUSY.load(Ordering::SeqCst) && !QUITTING.load(Ordering::SeqCst) => {
            api.prevent_exit();
        }
        // macOS: clicking the Dock icon brings back the window.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main_window(app),
        _ => {
            #[cfg(not(target_os = "macos"))]
            let _ = app;
        }
    });
}
