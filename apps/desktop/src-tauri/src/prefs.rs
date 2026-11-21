// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings that belong to this app, not to the daemon (whose settings every
//! client shares): what closing the window and Quit do, and the update
//! check. Kept in
//! `<app config dir>/desktop.json`; a missing or unreadable file means the
//! defaults.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State};

const FILE: &str = "desktop.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    /// Closing the window hides it to the tray; downloads go on.
    pub close_to_tray: bool,
    /// Quit closes WDM completely: it stops the daemon, and the browser's
    /// native host does not start the app again for a download until the
    /// user opens it (`wdm_proto::runtime::APP_QUIT_FILE`). On by default.
    /// Off: the daemon keeps downloading in the background, for the browser
    /// extension and the command line. (It replaced `stopDaemonOnQuit`,
    /// which was off by default; that key is ignored, so every install
    /// starts with the new default.)
    pub quit_stops_all: bool,
    /// The "still running in the tray" notice has been shown once.
    pub tray_notice_shown: bool,
    /// The UI's language when it last ran, so the tray starts in it
    /// (`shell_i18n`). Empty: English.
    pub language: String,
    /// Look for a new version once a day (`updates`). Never installs alone.
    pub auto_update_check: bool,
    /// When the app last looked for an update (Unix seconds; 0 = never).
    pub last_update_check: u64,
    /// The "Connect your browser" page has been shown once (first run).
    pub browser_setup_shown: bool,
    /// Launch at login was turned on at the first start (`login_item`);
    /// never again, so turning it off in Settings sticks.
    pub login_item_set: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            quit_stops_all: true,
            tray_notice_shown: false,
            language: String::new(),
            auto_update_check: true,
            last_update_check: 0,
            browser_setup_shown: false,
            login_item_set: false,
        }
    }
}

/// The app's copy, managed by Tauri.
#[derive(Debug, Default)]
pub struct PrefsState(pub Mutex<Prefs>);

impl PrefsState {
    pub fn get(&self) -> Prefs {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(FILE))
}

pub fn load<R: Runtime>(app: &AppHandle<R>) -> Prefs {
    path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save<R: Runtime>(app: &AppHandle<R>, prefs: &Prefs) -> Result<(), String> {
    let path = path(app).ok_or("no app config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(prefs).map_err(|e| e.to_string())?;
    // Write a sibling and rename, so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Changes the prefs and saves them.
pub fn update<R: Runtime>(app: &AppHandle<R>, f: impl FnOnce(&mut Prefs)) -> Result<Prefs, String> {
    let state = app.state::<PrefsState>();
    let mut prefs = state.0.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut prefs);
    save(app, &prefs)?;
    Ok(prefs.clone())
}

#[tauri::command]
pub fn desktop_prefs(state: State<'_, PrefsState>) -> Prefs {
    state.get()
}

#[tauri::command]
pub fn set_desktop_prefs<R: Runtime>(app: AppHandle<R>, prefs: Prefs) -> Result<Prefs, String> {
    update(&app, |p| *p = prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_close_everything_on_quit() {
        let p = Prefs::default();
        assert!(p.close_to_tray);
        assert!(p.quit_stops_all);
        assert!(p.auto_update_check);
        assert!(!p.browser_setup_shown);
        // Not set yet: the first start of an installed build turns launch at login on.
        assert!(!p.login_item_set);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let p: Prefs = serde_json::from_str(r#"{"quitStopsAll": false}"#).unwrap();
        assert!(!p.quit_stops_all);
        assert!(p.close_to_tray);
        assert!(!p.tray_notice_shown);
    }

    #[test]
    fn the_old_keep_running_default_does_not_carry_over() {
        // Earlier versions saved `"stopDaemonOnQuit": false` for everyone.
        let p: Prefs = serde_json::from_str(r#"{"stopDaemonOnQuit": false}"#).unwrap();
        assert!(p.quit_stops_all);
    }
}
