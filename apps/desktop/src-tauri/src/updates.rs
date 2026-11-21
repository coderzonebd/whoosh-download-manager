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
//! off (`autoUpdateCheck` in `desktop.json`), [`start_daily_check`] keeps the
//! app up to date by itself: once a day it looks, downloads a new version
//! quietly, and installs it at a quiet moment — nothing downloading and the
//! window hidden, or when the user quits — stopping the daemon first (its
//! downloads resume after the restart). A `.deb` or `.rpm` needs the
//! administrator's password, so there the UI is told instead
//! ([`EVENT_UPDATE_AVAILABLE`]) and the user installs with one click.
//!
//! Every copy updates itself, and the package manager it came from sees the
//! new version: winget reads the version the installer writes to Windows'
//! list of programs; the Homebrew cask says `auto_updates`, so `brew` leaves
//! the app to it; a `.deb` or `.rpm` is installed through `dpkg -i` /
//! `rpm -U` (the plugin asks for the password with pkexec), which is the
//! package database apt and dnf read. Only where the app cannot replace
//! itself does the package manager update it instead ([`managed_by`]):
//! Flatpak (sandboxed; the software centre updates it), Scoop (a versioned
//! folder Scoop keeps track of) and Arch's AUR (built from source, no
//! updater bundle).

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
/// With an update downloaded: how often it looks for a quiet moment to install.
const QUIET_TICK: Duration = Duration::from_secs(5 * 60);

/// The update found by the last check, kept for [`install_update`].
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

/// A new version downloaded in the background, waiting for a quiet moment.
#[derive(Default)]
pub struct Ready(Mutex<Option<(Update, Vec<u8>)>>);

/// Set while an automatic install runs, so it happens once.
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl std::fmt::Debug for Ready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ready")
    }
}

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

/// A package manager that owns this copy, and the command that updates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ManagedBy {
    pub manager: &'static str,
    pub command: &'static str,
}

/// Who updates this copy, when it is not the app itself.
pub fn managed_by() -> Option<ManagedBy> {
    let exe = std::env::current_exe().ok()?;
    managed_by_at(
        &exe,
        std::env::var_os("FLATPAK_ID").is_some(),
        std::env::var_os("APPIMAGE").is_some(),
        |p| std::path::Path::new(p).exists(),
    )
}

fn managed_by_at(
    exe: &std::path::Path,
    flatpak: bool,
    appimage: bool,
    exists: impl Fn(&str) -> bool,
) -> Option<ManagedBy> {
    if flatpak {
        return Some(ManagedBy {
            manager: "Flatpak",
            command: "flatpak update",
        });
    }
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if path.contains("/scoop/apps/") {
        return Some(ManagedBy {
            manager: "Scoop",
            command: "scoop update whoosh-download-manager",
        });
    }
    // An AUR build (pacman) has no updater bundle; a .deb or .rpm updates itself through dpkg/rpm.
    if !appimage
        && path.starts_with("/usr/")
        && exists("/var/lib/pacman/local")
        && !exists("/var/lib/dpkg/info/whoosh-download-manager.list")
    {
        return Some(ManagedBy {
            manager: "pacman",
            command: "sudo pacman -Syu",
        });
    }
    None
}

/// Who updates this copy (`null`: the app itself).
#[tauri::command]
pub fn update_source() -> Option<ManagedBy> {
    managed_by()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Option<UpdateInfo>, String> {
    if let Some(m) = managed_by() {
        return Err(format!("{} updates this copy", m.manager));
    }
    let found = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string());
    if let Err(err) = prefs::update(app, |p| p.last_update_check = now()) {
        eprintln!("wdm-desktop: could not save desktop.json: {err}");
    }
    // No published release yet (the feed is not there): nothing newer than this copy.
    let found = match found {
        Err(e) if e.contains("valid release JSON") => None,
        other => other?,
    };
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

/// Installing needs the administrator's password: a `.deb` or `.rpm` (the
/// plugin runs `dpkg -i` / `rpm -U` through pkexec). Never done unasked.
fn needs_password() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("APPIMAGE").is_none()
        && std::env::current_exe().is_ok_and(|e| e.starts_with("/usr"))
}

/// Something is downloading or queued (or the daemon cannot say): not a
/// moment to restart it.
async fn downloads_busy() -> bool {
    let Ok(dir) = crate::daemon::data_dir() else {
        return true;
    };
    match wdm_client::daemon::connect_running(&dir, &crate::daemon::client_info()).await {
        Ok(wdm_client::daemon::Found::Running(c)) => {
            let stats = c
                .client
                .call_value(wdm_proto::method::STATS_GET, serde_json::Value::Null)
                .await;
            c.client.close().await;
            stats.map_or(true, |v| {
                v["active"].as_u64().unwrap_or(1) + v["queued"].as_u64().unwrap_or(0) > 0
            })
        }
        Ok(wdm_client::daemon::Found::NotRunning(_)) => false,
        Err(_) => true,
    }
}

/// Installs the downloaded update: the daemon is stopped first (the
/// installer replaces it too), then the app restarts — unless `quitting`,
/// when it just ends. Returns only if there was nothing to install or it failed.
async fn install_ready<R: Runtime>(app: &AppHandle<R>, quitting: bool) {
    let Some((update, bytes)) = app
        .state::<Ready>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    else {
        return;
    };
    if INSTALLING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Ok(dir) = crate::daemon::data_dir()
        && let Err(err) = crate::daemon::shutdown(&dir).await
    {
        eprintln!("wdm-desktop: update: could not stop the daemon: {err}");
    }
    match update.install(bytes) {
        Ok(()) if quitting => crate::exit_cleanly(app).await,
        Ok(()) => app.restart(),
        Err(err) => {
            eprintln!("wdm-desktop: installing {}: {err}", update.version);
            INSTALLING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// At Quit: a downloaded update goes in now (no restart). Returns whether
/// it took over the quitting (the caller must not exit by itself).
pub fn install_on_quit<R: Runtime>(app: &AppHandle<R>) -> bool {
    let has = app
        .state::<Ready>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    if !has || needs_password() {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        install_ready(&app, true).await;
        crate::exit_cleanly(&app).await;
    });
    true
}

/// The app is out of the way: its window is hidden (in the tray).
fn window_hidden<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window("main")
        .is_none_or(|w| !w.is_visible().unwrap_or(false))
}

/// Keeps the app up to date by itself. Not in development builds, nor where
/// a package manager updates the app ([`managed_by`]).
pub fn start_daily_check<R: Runtime>(app: AppHandle<R>) {
    if cfg!(debug_assertions) || managed_by().is_some() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        let mut announced = String::new();
        loop {
            let p = app.state::<prefs::PrefsState>().get();
            let have_ready = app
                .state::<Ready>()
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if p.auto_update_check
                && !have_ready
                && now().saturating_sub(p.last_update_check) >= DAY
            {
                match tauri::async_runtime::block_on(check(&app)) {
                    Ok(Some(info)) if needs_password() => {
                        // One click (and the password) away; never a password prompt out of nowhere.
                        if info.version != announced {
                            let _ = app.emit(
                                EVENT_UPDATE_AVAILABLE,
                                serde_json::json!({ "version": info.version }),
                            );
                            announced = info.version;
                        }
                    }
                    Ok(Some(info)) => {
                        let update = app
                            .state::<Pending>()
                            .0
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take();
                        if let Some(update) = update {
                            match tauri::async_runtime::block_on(update.download(|_, _| {}, || {}))
                            {
                                Ok(bytes) => {
                                    *app.state::<Ready>()
                                        .0
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner()) = Some((update, bytes));
                                }
                                Err(err) => {
                                    eprintln!("wdm-desktop: downloading {}: {err}", info.version)
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("wdm-desktop: update check: {err}"),
                }
            }
            // A quiet moment: nothing downloading and the window out of sight.
            let ready = app
                .state::<Ready>()
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if ready
                && p.auto_update_check
                && window_hidden(&app)
                && !tauri::async_runtime::block_on(downloads_busy())
            {
                tauri::async_runtime::block_on(install_ready(&app, false));
            }
            std::thread::sleep(if ready { QUIET_TICK } else { TICK });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn by(exe: &str, flatpak: bool, appimage: bool, files: &[&str]) -> Option<&'static str> {
        managed_by_at(Path::new(exe), flatpak, appimage, |p| files.contains(&p)).map(|m| m.manager)
    }

    #[test]
    fn package_managers_own_their_copies() {
        assert_eq!(
            by(
                "/usr/bin/wdm-desktop",
                false,
                false,
                &["/var/lib/pacman/local"]
            ),
            Some("pacman")
        );
        assert_eq!(
            by("/app/bin/wdm-desktop", true, false, &[]),
            Some("Flatpak")
        );
        assert_eq!(
            by(
                r"C:\Users\a\scoop\apps\whoosh-download-manager\current\wdm-desktop.exe",
                false,
                false,
                &[]
            ),
            Some("Scoop")
        );
    }

    #[test]
    fn installers_update_themselves() {
        // .deb and .rpm through dpkg/rpm, which apt and dnf read.
        let deb = [
            "/var/lib/dpkg/info/whoosh-download-manager.list",
            "/var/lib/pacman/local",
        ];
        assert_eq!(by("/usr/bin/wdm-desktop", false, false, &deb), None);
        assert_eq!(
            by("/usr/bin/wdm-desktop", false, false, &["/usr/bin/dnf"]),
            None
        );
        // An AppImage mounts under /tmp; NSIS/MSI (also from winget) under Program Files or
        // AppData; the macOS app (also from Homebrew) under /Applications.
        assert_eq!(
            by("/tmp/.mount_Whoosh/usr/bin/wdm-desktop", false, true, &[]),
            None
        );
        assert_eq!(
            by("/usr/bin/wdm-desktop", false, true, &["/usr/bin/dnf"]),
            None
        );
        assert_eq!(
            by(
                r"C:\Program Files\Whoosh Download Manager\wdm-desktop.exe",
                false,
                false,
                &[]
            ),
            None
        );
        assert_eq!(
            by(
                "/Applications/Whoosh Download Manager.app/Contents/MacOS/wdm-desktop",
                false,
                false,
                &[]
            ),
            None
        );
        assert_eq!(
            by(
                "/home/a/Projects/wdm/target/release/wdm-desktop",
                false,
                false,
                &[]
            ),
            None
        );
    }
}
