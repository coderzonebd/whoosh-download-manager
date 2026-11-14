// SPDX-License-Identifier: GPL-3.0-or-later
//! Launch at login is on by default: an installed build turns it on at its
//! first start, once (`Prefs::login_item_set`), and the UI tells the user
//! where to turn it off. A development build never registers itself.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_autostart::ManagerExt;

use crate::prefs;

/// Set when this start turned launch at login on; the UI shows a notice once.
static JUST_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn first_run<R: Runtime>(app: &AppHandle<R>) {
    if cfg!(debug_assertions) || app.state::<prefs::PrefsState>().get().login_item_set {
        return;
    }
    match app.autolaunch().enable() {
        Ok(()) => JUST_ENABLED.store(true, Ordering::SeqCst),
        Err(err) => eprintln!("wdm-desktop: could not turn on launch at login: {err}"),
    }
    // Recorded either way: a failure is not retried at every start.
    if let Err(err) = prefs::update(app, |p| p.login_item_set = true) {
        eprintln!("wdm-desktop: could not save desktop.json: {err}");
    }
}

/// True once, when this start turned launch at login on.
#[tauri::command]
pub fn take_login_notice() -> bool {
    JUST_ENABLED.swap(false, Ordering::SeqCst)
}
