// SPDX-License-Identifier: GPL-3.0-or-later
//! Connecting the browsers (ADR 0007, amendment 1): the "Connect your
//! browser" page asks for [`browser_status`], registers the native messaging
//! host with [`connect_browser`] and opens the extension's store page with
//! [`open_extension_page`].
//!
//! Installers register the host where they can (the NSIS installer per
//! user, the MSI and the `.deb`/`.rpm` for every user). A `.dmg` or an
//! AppImage cannot, so at every start [`ensure_registered`] registers it for
//! this user in the browsers they use, unless a system-wide registration
//! already covers that browser. From an AppImage, whose files move on every
//! run, the host is first copied to `<data dir>/bin/`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use whoosh_nmhost::detect::{self, BrowserStatus};
use whoosh_nmhost::external;
use whoosh_nmhost::ids::ids;
use whoosh_nmhost::register::{self, Browser, Location};
use whoosh_nmhost::runtime::{Env, Os};

const HOST_EXE: &str = if cfg!(windows) {
    "whoosh-nmhost.exe"
} else {
    "whoosh-nmhost"
};

/// Where the user gets the extension for a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Listing {
    EdgeAddOns,
    ChromeWebStore,
    Amo,
    /// No store listing yet: the release page, with the manual install.
    Github,
}

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

const RELEASES_URL: &str = "https://github.com/coderzonebd/whoosh-download-manager/releases/latest";

fn store_of(browser: Browser) -> Listing {
    let ids = ids();
    match browser {
        Browser::Edge if ids.edge.id.is_some() => Listing::EdgeAddOns,
        Browser::Firefox if ids.firefox.amo_slug.is_some() => Listing::Amo,
        Browser::Chrome | Browser::Chromium | Browser::Brave | Browser::Vivaldi | Browser::Edge
            if ids.chrome_web_store.id.is_some() =>
        {
            Listing::ChromeWebStore
        }
        _ => Listing::Github,
    }
}

/// The page that installs the extension in `browser`. Built only from the
/// compiled-in IDs, so the webview cannot make the app open anything else.
pub fn store_url(browser: Browser) -> String {
    let ids = ids();
    match store_of(browser) {
        Listing::EdgeAddOns => format!(
            "https://microsoftedge.microsoft.com/addons/detail/{}",
            ids.edge.id.as_deref().unwrap_or_default()
        ),
        Listing::ChromeWebStore => format!(
            "https://chromewebstore.google.com/detail/{}",
            ids.chrome_web_store.id.as_deref().unwrap_or_default()
        ),
        Listing::Amo => format!(
            "https://addons.mozilla.org/firefox/addon/{}/",
            ids.firefox.amo_slug.as_deref().unwrap_or_default()
        ),
        Listing::Github => RELEASES_URL.to_owned(),
    }
}

fn chromium_ids() -> Vec<String> {
    let ids = ids();
    let mut all = vec![ids.github.chromium_id.clone()];
    all.extend(ids.store_chromium_ids());
    all
}

fn firefox_ids() -> Vec<String> {
    vec![ids().firefox.id.clone()]
}

fn statuses() -> Vec<BrowserStatus> {
    detect::status(
        Os::current(),
        &Env::from_process(),
        &chromium_ids(),
        &firefox_ids(),
    )
}

#[tauri::command]
pub async fn browser_status() -> Vec<BrowserRow> {
    tauri::async_runtime::spawn_blocking(|| {
        statuses()
            .into_iter()
            .map(|s| BrowserRow {
                browser: s.browser.name(),
                in_use: s.in_use,
                host_registered: s.host_registered,
                extension_found: s.extension_found,
                store: store_of(s.browser),
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
    let bundled = exe
        .parent()
        .map(|dir| dir.join(HOST_EXE))
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("{HOST_EXE} is not installed next to the app"))?;
    if std::env::var_os("APPIMAGE").is_none() {
        return Ok(bundled);
    }
    let dir = crate::daemon::data_dir()?.join("bin");
    let copy = dir.join(HOST_EXE);
    copy_if_changed(&bundled, &copy).map_err(|e| format!("copying {HOST_EXE}: {e}"))?;
    Ok(copy)
}

fn copy_if_changed(from: &Path, to: &Path) -> std::io::Result<()> {
    let new = std::fs::read(from)?;
    if std::fs::read(to).is_ok_and(|old| old == new) {
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = to.with_extension("tmp");
    std::fs::write(&tmp, &new)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, to)
}

fn register_for(browsers: Option<Vec<Browser>>, offer: bool) -> Result<(), String> {
    let os = Os::current();
    let env = Env::from_process();
    let opts = register::Options {
        browsers,
        host_path: host_path()?,
        chromium_ids: chromium_ids(),
        firefox_ids: firefox_ids(),
    };
    register::register(os, &env, &opts).map_err(|e| e.to_string())?;
    if offer {
        external::offer(os, &env, ids(), false).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    crate::open_in_browser(&store_url(browser))
        .map_err(|e| format!("could not open the browser: {e}"))
}

/// A user-level manifest that points at a host which no longer exists (the
/// app was moved, or an AppImage copy was deleted).
fn stale_user_manifest(browser: Browser) -> bool {
    let Ok(targets) = register::targets(Os::current(), &Env::from_process(), browser) else {
        return false;
    };
    targets.iter().any(|t| {
        let path = match &t.location {
            Location::File(p) | Location::Registry { manifest: p, .. } => p,
        };
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(PathBuf::from))
            .is_some_and(|host| !host.is_file())
    })
}

/// At start: register the host for this user in each browser in use that
/// has no registration (or a stale one). On macOS, whose `.dmg` has no
/// installer to do it, the first run also asks the browsers to offer the
/// extension from their store. Quiet: failures are only logged, and the
/// "Connect your browser" page shows what is missing.
pub fn ensure_registered(first_run: bool) {
    let missing: Vec<Browser> = statuses()
        .into_iter()
        .filter(|s| s.in_use && !s.system_registered)
        .filter(|s| !s.host_registered || stale_user_manifest(s.browser))
        .map(|s| s.browser)
        .collect();
    if !missing.is_empty()
        && let Err(err) = register_for(Some(missing), false)
    {
        eprintln!("whoosh-desktop: browser integration: {err}");
    }
    if first_run
        && cfg!(target_os = "macos")
        && let Err(err) = external::offer(Os::current(), &Env::from_process(), ids(), false)
    {
        eprintln!("whoosh-desktop: store offers: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_pages_are_fixed_addresses() {
        for b in Browser::ALL {
            let url = store_url(b);
            assert!(
                url == RELEASES_URL
                    || url.starts_with("https://microsoftedge.microsoft.com/addons/detail/")
                    || url.starts_with("https://chromewebstore.google.com/detail/")
                    || url.starts_with("https://addons.mozilla.org/firefox/addon/"),
                "{url}"
            );
        }
    }
}
