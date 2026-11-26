// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings that belong to this app, not to the daemon (whose settings every
//! client shares): what closing the window does, and the update check. The
//! fields are in `wdm_tray::prefs`: the tray process reads them too.
//! (Quit always closes WDM completely; the `stopDaemonOnQuit` key of
//! earlier versions is ignored.) Kept in
//! `<app config dir>/desktop.json`; a missing or unreadable file means the
//! defaults.

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, Runtime, State};
use wdm_tray::prefs::FILE;
pub use wdm_tray::prefs::Prefs;

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
        .map(|p| wdm_tray::prefs::load_from(&p))
        .unwrap_or_default()
}

pub fn save<R: Runtime>(app: &AppHandle<R>, prefs: &Prefs) -> Result<(), String> {
    let path = path(app).ok_or("no app config directory")?;
    // Through a sibling file and a rename: never half a file (the tray reads it).
    wdm_tray::prefs::save_to(&path, prefs).map_err(|e| e.to_string())
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

/// Small answers the UI keeps while WDM runs (`host.thisRun`), across the
/// main window being closed to the tray (which ends this process): a file
/// in the data directory ([`wdm_tray::THIS_RUN_FILE`]) that the tray
/// removes when it starts. At most a few short keys.
#[derive(Debug)]
pub struct ThisRun(PathBuf);

impl ThisRun {
    const MAX_KEYS: usize = 64;
    const MAX_KEY_LEN: usize = 64;

    pub fn new(data_dir: &std::path::Path) -> Self {
        Self(data_dir.join(wdm_tray::THIS_RUN_FILE))
    }

    fn keys(&self) -> Vec<String> {
        std::fs::read(&self.0)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn get(&self, key: &str) -> bool {
        self.keys().iter().any(|k| k == key)
    }

    fn set(&self, key: String) -> Result<(), String> {
        if key.is_empty() || key.len() > Self::MAX_KEY_LEN {
            return Err("bad key".into());
        }
        let mut keys = self.keys();
        if keys.contains(&key) {
            return Ok(());
        }
        if keys.len() >= Self::MAX_KEYS {
            return Err("too many keys".into());
        }
        keys.push(key);
        let json = serde_json::to_vec(&keys).map_err(|e| e.to_string())?;
        if let Some(dir) = self.0.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.0, json).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn this_run(state: State<'_, ThisRun>, key: String) -> bool {
    state.get(&key)
}

#[tauri::command]
pub fn set_this_run(state: State<'_, ThisRun>, key: String) -> Result<(), String> {
    state.set(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_run_keeps_a_few_short_keys() {
        let dir = std::env::temp_dir().join(format!("wdm-this-run-{}", std::process::id()));
        let run = ThisRun::new(&dir);
        assert!(!run.get("organizeDismissed"));
        run.set("organizeDismissed".into()).unwrap();
        assert!(run.get("organizeDismissed"));
        assert!(run.set(String::new()).is_err());
        assert!(run.set("x".repeat(65)).is_err());
        for i in 0..63 {
            run.set(format!("k{i}")).unwrap();
        }
        assert!(run.set("one-too-many".into()).is_err());
        run.set("k1".into()).unwrap();
        // Another process (the window made again) sees them.
        assert!(ThisRun::new(&dir).get("k5"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
