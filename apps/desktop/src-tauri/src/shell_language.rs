// SPDX-License-Identifier: GPL-3.0-or-later
//! The UI's language, for the strings the shell shows itself
//! (`wdm_tray::i18n`): kept in `desktop.json`, where the tray process reads
//! it for its menu, tooltip and notifications.

use tauri::{AppHandle, Manager, Runtime};

use crate::prefs;

/// Called by the UI whenever its language changes (it follows the daemon's
/// `Settings.language`).
#[tauri::command]
pub fn set_shell_language<R: Runtime>(app: AppHandle<R>, language: String) -> Result<(), String> {
    if app.state::<prefs::PrefsState>().get().language != language {
        prefs::update(&app, |p| p.language = language)?;
    }
    Ok(())
}
