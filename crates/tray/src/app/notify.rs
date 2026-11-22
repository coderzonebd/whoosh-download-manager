// SPDX-License-Identifier: GPL-3.0-or-later
//! System notifications from the tray, as the app sends them
//! (`tauri-plugin-notification` does the same per platform): under the
//! app's identifier on Windows (its Start menu shortcut's app id; not for a
//! build run from `target/`) and macOS.

use wdm_tray::i18n::{self, Notice};

pub fn show(language: &str, which: &Notice<'_>) {
    let (title, body) = i18n::notice(language, which);
    let mut n = notify_rust::Notification::new();
    // Named like the app (not "wdm-tray") where the desktop shows a source.
    n.appname(&title).summary(&title).body(&body).auto_icon();
    #[cfg(windows)]
    {
        let from_target = std::env::current_exe().ok().is_some_and(|exe| {
            exe.parent()
                .is_some_and(|d| d.ends_with("target/debug") || d.ends_with("target/release"))
        });
        if !from_target {
            n.app_id(wdm_tray::IDENTIFIER);
        }
    }
    #[cfg(target_os = "macos")]
    let _ = notify_rust::set_application(wdm_tray::IDENTIFIER);
    if let Err(err) = n.show() {
        eprintln!("wdm-tray: could not show a notification: {err}");
    }
}
