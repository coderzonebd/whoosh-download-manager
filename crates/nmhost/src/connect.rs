// SPDX-License-Identifier: GPL-3.0-or-later
//! "Connect your browser" (ADR 0007, amendment 1), shared by the desktop
//! app's web page and its native main window: which browsers this user
//! has and how far each is connected ([`rows`]), linking the host to them
//! ([`register_for`]) and the page that installs the extension
//! ([`store_url`]), built only from the compiled-in IDs so a window cannot
//! make the app open anything else.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::detect::{self, BrowserStatus};
use crate::external;
use crate::ids::ids;
use crate::register::{self, Browser, Location};
use crate::runtime::{Env, Os};

/// The host executable's file name.
pub const HOST_EXE: &str = if cfg!(windows) {
    "wdm-nmhost.exe"
} else {
    "wdm-nmhost"
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

impl Listing {
    /// The name the UI's strings use (`browsers.store.<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Listing::EdgeAddOns => "edgeAddOns",
            Listing::ChromeWebStore => "chromeWebStore",
            Listing::Amo => "amo",
            Listing::Github => "github",
        }
    }
}

/// One browser on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub browser: Browser,
    pub in_use: bool,
    pub host_registered: bool,
    pub extension_found: bool,
    pub store: Listing,
}

pub const RELEASES_URL: &str =
    "https://github.com/coderzonebd/whoosh-download-manager/releases/latest";

pub fn store_of(browser: Browser) -> Listing {
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

/// The page that installs the extension in `browser`.
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

/// Every browser's status for this user (reads the disk: off the UI thread).
pub fn statuses() -> Vec<BrowserStatus> {
    detect::status(
        Os::current(),
        &Env::from_process(),
        &chromium_ids(),
        &firefox_ids(),
    )
}

/// The page's rows, one per browser WDM knows.
pub fn rows() -> Vec<Row> {
    statuses()
        .into_iter()
        .map(|s| Row {
            browser: s.browser,
            in_use: s.in_use,
            host_registered: s.host_registered,
            extension_found: s.extension_found,
            store: store_of(s.browser),
        })
        .collect()
}

/// The host executable to register: the one installed next to `exe` (the
/// app), or its copy in `<data_dir>/bin/` when the app runs from an
/// AppImage, whose files move on every run.
pub fn host_path(exe: &Path, data_dir: Option<&Path>) -> Result<PathBuf, String> {
    let bundled = exe
        .parent()
        .map(|dir| dir.join(HOST_EXE))
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("{HOST_EXE} is not installed next to the app"))?;
    if std::env::var_os("APPIMAGE").is_none() {
        return Ok(bundled);
    }
    let dir = data_dir
        .ok_or("no data directory for the host's copy")?
        .join("bin");
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

/// Registers the host at `host_path` for `browsers` (every browser in use
/// when `None`); with `offer`, also asks those browsers to offer the
/// extension from their store.
pub fn register_for(
    browsers: Option<Vec<Browser>>,
    host_path: PathBuf,
    offer: bool,
) -> Result<(), String> {
    let os = Os::current();
    let env = Env::from_process();
    let opts = register::Options {
        browsers,
        host_path,
        chromium_ids: chromium_ids(),
        firefox_ids: firefox_ids(),
    };
    register::register(os, &env, &opts).map_err(|e| e.to_string())?;
    if offer {
        external::offer(os, &env, ids(), false).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A user-level manifest that points at a host which no longer exists (the
/// app was moved, or an AppImage copy was deleted) or at a verbatim Windows
/// path that browsers cannot start.
pub fn stale_user_manifest(browser: Browser) -> bool {
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
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_owned))
            // 1.0.0-beta.2 and earlier wrote a verbatim path on Windows, which
            // browsers cannot launch: rewrite it.
            .is_some_and(|host| register::is_verbatim(&host) || !Path::new(&host).is_file())
    })
}

/// Asks the browsers to offer the extension from their store (macOS's
/// first run, where no installer did).
pub fn offer_extension() -> Result<(), String> {
    external::offer(Os::current(), &Env::from_process(), ids(), false)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
            assert_eq!(
                serde_json::to_value(store_of(b)).unwrap(),
                store_of(b).name()
            );
        }
    }
}
