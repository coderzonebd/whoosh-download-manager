// SPDX-License-Identifier: GPL-3.0-or-later
//! Launch at login is on by default: an installed build turns it on at its
//! first start, once (`Prefs::login_item_set`), and the UI tells the user
//! where to turn it off. A development build never registers itself.
//!
//! The login item stores the program's path when it is written. An update
//! that moves or renames the program (e.g. `whoosh-desktop` → `wdm-desktop`)
//! would leave it pointing at nothing, so while it is on, every start of an
//! installed build writes it again with the current path.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_autostart::ManagerExt;

use crate::prefs;

/// Set when this start turned launch at login on; the UI shows a notice once.
static JUST_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn first_run<R: Runtime>(app: &AppHandle<R>) {
    if cfg!(debug_assertions) {
        return;
    }
    if app.state::<prefs::PrefsState>().get().login_item_set {
        refresh(app);
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

/// Launch at login is on: write it again so it names this program's path.
/// Off (the user turned it off): leave it off.
fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let autolaunch = app.autolaunch();
    match autolaunch.is_enabled() {
        Ok(true) => {
            if let Err(err) = autolaunch.enable() {
                eprintln!("wdm-desktop: could not update launch at login: {err}");
            }
        }
        Ok(false) => {}
        Err(err) => eprintln!("wdm-desktop: could not read launch at login: {err}"),
    }
}

/// True once, when this start turned launch at login on.
#[tauri::command]
pub fn take_login_notice() -> bool {
    JUST_ENABLED.swap(false, Ordering::SeqCst)
}
