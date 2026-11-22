// SPDX-License-Identifier: GPL-3.0-or-later
//! The WDM desktop app is two processes:
//!
//! * **`wdm-tray`** (this crate's binary): the tray icon and its menu, the
//!   tooltip, the system notifications, starting the daemon (and again if
//!   it crashes), opening the prompt window when the browser hands over a
//!   download, and the daily update check. It has no web view, so it takes
//!   a few megabytes. It runs while WDM runs: started at login, or by the
//!   app.
//! * **`wdm-desktop`** (`apps/desktop`): the main window, a web page. It
//!   runs only while the window is open: closing it to the tray ends it,
//!   and the tray starts it again when it is wanted.
//!
//! The prompt window for what the browser hands over is a third program,
//! **`wdm-prompt`** (`crates/prompt`), a native window that runs only while
//! it has something to ask.
//!
//! This library holds what they share: the strings ([`i18n`]), the shared
//! prefs file ([`prefs`]), the locks that say which process runs
//! ([`locks`]), how one starts another ([`launch`]), the prompt window's
//! followed downloads ([`followed`]) and opening a finished download
//! ([`files`]).

pub mod files;
pub mod followed;
pub mod i18n;
pub mod launch;
pub mod locks;
pub mod prefs;

/// The app's identifier (`tauri.conf.json`): its config directory, and its
/// notifications' app id on Windows and macOS.
pub const IDENTIFIER: &str = "com.coderzonebd.whoosh";

/// `wdm-desktop --prompt` (from the browser's native host, or `$APPIMAGE
/// --prompt`) becomes the prompt window, `wdm-prompt`.
pub const PROMPT_ARG: &str = "--prompt";
/// `wdm-prompt --main` (or `wdm-desktop --main`) opens the native main window.
pub const MAIN_ARG: &str = "--main";
/// `--main --page <history|browsers|settings>`: the main window opens on that page.
pub const PAGE_ARG: &str = "--page";
/// Passed to `wdm-desktop` by the tray: look for an update, with no window.
pub const UPDATE_CHECK_ARG: &str = "--update-check";
/// `wdm-desktop --tray` becomes the tray process (from an AppImage, whose
/// files exist only while a process of it runs).
pub const TRAY_ARG: &str = "--tray";
/// Passed by the login item (`tauri-plugin-autostart`): start in the tray.
pub const AUTOSTART_ARG: &str = "--autostart";

/// The daemon's data directory (`$WDM_DATA_DIR`, else the platform's).
pub fn data_dir() -> Option<std::path::PathBuf> {
    wdm_proto::runtime::default_data_dir().ok()
}

/// The downloads the prompt window follows (`followed.json` in the data
/// directory, [`followed`]), written by the prompt window: when one ends,
/// the tray opens the prompt window again for its "Download complete".
pub const FOLLOWED_FILE: &str = "followed.json";

/// Small answers the UI keeps while WDM runs (`host.thisRun`, in the data
/// directory), written by the app; the tray removes it when it starts, so
/// "while WDM runs" is the tray's life.
pub const THIS_RUN_FILE: &str = "this-run.json";
