// SPDX-License-Identifier: GPL-3.0-or-later
//! Connecting the browsers (ADR 0007, amendment 1): the "Connect your
//! browser" page asks for [`browser_status`], registers the native messaging
//! host with [`connect_browser`] and opens the extension's store page with
//! [`open_extension_page`]. The work itself is `wdm_nmhost::connect`, which
//! the native main window uses too.
//!
//! Installers register the host where they can (the NSIS installer per
//! user, the MSI and the `.deb`/`.rpm` for every user). A `.dmg` or an
//! AppImage cannot, so at every start [`ensure_registered`] registers it for
//! this user in the browsers they use, unless a system-wide registration
//! already covers that browser. From an AppImage, whose files move on every
//! run, the host is first copied to `<data dir>/bin/`.

use std::path::PathBuf;

use serde::Serialize;
use wdm_nmhost::connect::{self, Listing};
use wdm_nmhost::register::Browser;

/// One row of the "Connect your browser" page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRow {
    pub browser: &'static str,
    pub in_use: bool,
    pub host_registered: bool,
    pub extension_found: bool,
    pub store: Listing,
}

#[tauri::command]
pub async fn browser_status() -> Vec<BrowserRow> {
    tauri::async_runtime::spawn_blocking(|| {
        connect::rows()
            .into_iter()
            .map(|r| BrowserRow {
                browser: r.browser.name(),
                in_use: r.in_use,
                host_registered: r.host_registered,
                extension_found: r.extension_found,
                store: r.store,
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The host executable to register: the one bundled next to the app, or its
/// copy outside an AppImage.
fn host_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    connect::host_path(&exe, crate::daemon::data_dir().ok().as_deref())
}

fn register_for(browsers: Option<Vec<Browser>>, offer: bool) -> Result<(), String> {
    connect::register_for(browsers, host_path()?, offer)
}

/// Registers the host for one browser (by name) or for every browser in use,
/// and asks those browsers to offer the extension from their store.
#[tauri::command]
pub async fn connect_browser(browser: Option<String>) -> Result<(), String> {
    let browsers = match browser {
        None => None,
        Some(name) => Some(vec![
            Browser::parse(&name).ok_or_else(|| format!("unknown browser {name:?}"))?,
        ]),
    };
    tauri::async_runtime::spawn_blocking(move || register_for(browsers, true))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_extension_page(browser: String) -> Result<(), String> {
    let browser = Browser::parse(&browser).ok_or_else(|| format!("unknown browser {browser:?}"))?;
    crate::open_in_browser(&connect::store_url(browser))
        .map_err(|e| format!("could not open the browser: {e}"))
}

/// At start: register the host for this user in each browser in use that
/// has no registration (or a stale one). On macOS, whose `.dmg` has no
/// installer to do it, the first run also asks the browsers to offer the
/// extension from their store. Quiet: failures are only logged, and the
/// "Connect your browser" page shows what is missing.
pub fn ensure_registered(first_run: bool) {
    let missing: Vec<Browser> = connect::statuses()
        .into_iter()
        .filter(|s| s.in_use && !s.system_registered)
        .filter(|s| !s.host_registered || connect::stale_user_manifest(s.browser))
        .map(|s| s.browser)
        .collect();
    if !missing.is_empty()
        && let Err(err) = register_for(Some(missing), false)
    {
        eprintln!("wdm-desktop: browser integration: {err}");
    }
    if first_run
        && cfg!(target_os = "macos")
        && let Err(err) = connect::offer_extension()
    {
        eprintln!("wdm-desktop: store offers: {err}");
    }
}
