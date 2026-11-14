// SPDX-License-Identifier: GPL-3.0-or-later
//! In-app updates through `tauri-plugin-updater`: `latest.json` on the
//! repository's `updater` branch (kept by `.github/workflows/updater-feed.yml`
//! from each published release, pre-releases included), every bundle signed
//! with our minisign key, whose public half is
//! pinned in `tauri.conf.json` (`plugins.updater.pubkey`). An update that
//! does not verify is refused by the plugin.
//!
//! The UI checks on request ([`check_for_update`]) and installs
//! ([`install_update`], which restarts the app). Unless the user turned it
//! off (`autoUpdateCheck` in `desktop.json`), [`start_daily_check`] looks
//! once a day, quietly, and only tells the UI when there is something new
//! ([`EVENT_UPDATE_AVAILABLE`]); it never installs on its own.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::prefs;

/// `{ version }`: the daily check found a newer version.
pub const EVENT_UPDATE_AVAILABLE: &str = "desktop://update-available";

const DAY: u64 = 24 * 60 * 60;
/// The first automatic check waits until the app has settled.
const FIRST_DELAY: Duration = Duration::from_secs(90);
/// How often the background thread wakes to see whether a day has passed.
const TICK: Duration = Duration::from_secs(60 * 60);

/// The update found by the last check, kept for [`install_update`].
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Option<UpdateInfo>, String> {
    let found = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string());
    if let Err(err) = prefs::update(app, |p| p.last_update_check = now()) {
        eprintln!("whoosh-desktop: could not save desktop.json: {err}");
    }
    let found = found?;
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone(),
    });
    *app.state::<Pending>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = found;
    Ok(info)
}

/// "Check for updates": `null` when this is the newest version.
#[tauri::command]
pub async fn check_for_update<R: Runtime>(app: AppHandle<R>) -> Result<Option<UpdateInfo>, String> {
    check(&app).await
}

/// Downloads and installs the update found by the last check (checking
/// again if there was none), then restarts the app.
#[tauri::command]
pub async fn install_update<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, Pending>,
) -> Result<(), String> {
    let taken = pending.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    let update = match taken {
        Some(update) => update,
        None => {
            check(&app).await?;
            pending
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or("this is the newest version")?
        }
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}

/// The quiet daily check. Not in development builds.
pub fn start_daily_check<R: Runtime>(app: AppHandle<R>) {
    if cfg!(debug_assertions) {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        let mut announced = String::new();
        loop {
            let p = app.state::<prefs::PrefsState>().get();
            if p.auto_update_check && now().saturating_sub(p.last_update_check) >= DAY {
                match tauri::async_runtime::block_on(check(&app)) {
                    Ok(Some(info)) if info.version != announced => {
                        let _ = app.emit(
                            EVENT_UPDATE_AVAILABLE,
                            serde_json::json!({ "version": info.version }),
                        );
                        announced = info.version;
                    }
                    Ok(_) => {}
                    Err(err) => eprintln!("whoosh-desktop: update check: {err}"),
                }
            }
            std::thread::sleep(TICK);
        }
    });
}
