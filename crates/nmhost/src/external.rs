//! "External extensions": asking a browser to offer our extension from its
//! store (ADR 0007, amendment 1, *Connecting the browsers*). The browser then
//! asks the user once whether to enable it. No enterprise policies.
//!
//! Per user, as `--register --offer-extension` writes them:
//!
//! * Windows: `HKCU\Software\Microsoft\Edge\Extensions\<edge id>` and, once
//!   the Chrome Web Store switch is on, `HKCU\Software\Google\Chrome\Extensions\<id>`,
//!   each with an `update_url` value. (Microsoft documents the `HKLM` form,
//!   which the MSI writes; Chromium reads `HKCU` too.)
//! * macOS: `~/Library/Application Support/<browser>/External Extensions/<id>.json`.
//! * Linux: nothing per user; the `.deb` and `.rpm` install the files under
//!   `/usr/share/<browser>/extensions/` (packaging/browsers/generate.mjs).
//!
//! Chrome accepts only Web Store-hosted extensions here, so nothing is
//! offered to Chrome, Brave or Vivaldi until a Chrome Web Store ID exists
//! and `chromeWebStore.externalInstall` is switched on.

use std::io;
use std::path::PathBuf;

use crate::ids::{CHROME_WEB_STORE_UPDATE_URL, EDGE_UPDATE_URL, ExtensionIds};
use crate::register::{Browser, RegisterError, targets};
use crate::runtime::{Env, Os};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferLocation {
    /// A JSON file `{"external_update_url": …}` (macOS).
    File(PathBuf),
    /// `HKCU\<key>` with an `update_url` value (Windows).
    Registry(String),
}

impl std::fmt::Display for OfferLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OfferLocation::File(path) => write!(f, "{}", path.display()),
            OfferLocation::Registry(key) => write!(f, r"HKCU\{key}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub browser: Browser,
    pub id: String,
    pub update_url: &'static str,
    pub location: OfferLocation,
    /// The browser's profile directory: it has been used by this user.
    pub profile_dir: PathBuf,
}

/// Every offer `ids` allows on `os` for this user.
pub fn offers(os: Os, env: &Env, ids: &ExtensionIds) -> Result<Vec<Offer>, RegisterError> {
    let mut wanted: Vec<(Browser, &str, &'static str)> = Vec::new();
    if let Some(id) = ids.edge.id.as_deref() {
        wanted.push((Browser::Edge, id, EDGE_UPDATE_URL));
    }
    if let Some(id) = ids.chrome_web_store_offer() {
        for browser in [
            Browser::Chrome,
            Browser::Chromium,
            Browser::Brave,
            Browser::Vivaldi,
        ] {
            wanted.push((browser, id, CHROME_WEB_STORE_UPDATE_URL));
        }
    }

    let mut all = Vec::new();
    for (browser, id, update_url) in wanted {
        let profile_dir = targets(os, env, browser)?
            .into_iter()
            .next()
            .map(|t| t.profile_dir)
            .unwrap_or_default();
        let location = match os {
            Os::Linux => continue,
            Os::MacOs => {
                OfferLocation::File(profile_dir.join(format!("External Extensions/{id}.json")))
            }
            Os::Windows => {
                let prefix = match browser {
                    Browser::Edge => r"Software\Microsoft\Edge",
                    // Chromium's external-extension key; Brave and Vivaldi
                    // read Chrome's (not verified on Windows).
                    Browser::Chrome => r"Software\Google\Chrome",
                    _ => continue,
                };
                OfferLocation::Registry(format!(r"{prefix}\Extensions\{id}"))
            }
        };
        all.push(Offer {
            browser,
            id: id.to_owned(),
            update_url,
            location,
            profile_dir,
        });
    }
    Ok(all)
}

/// Writes the offers for the browsers this user has (every browser with
/// `all`). Returns what was written.
pub fn offer(
    os: Os,
    env: &Env,
    ids: &ExtensionIds,
    all: bool,
) -> Result<Vec<Offer>, RegisterError> {
    let mut done = Vec::new();
    for offer in offers(os, env, ids)? {
        // Registry keys cost nothing and make a browser installed later
        // find the extension; files go only into browsers in use.
        let wanted = all
            || matches!(offer.location, OfferLocation::Registry(_))
            || offer.profile_dir.is_dir();
        if !wanted {
            continue;
        }
        let err = |action, source| RegisterError::Io {
            action,
            target: offer.location.to_string(),
            source,
        };
        match &offer.location {
            OfferLocation::File(path) => {
                let text = serde_json::to_string_pretty(&serde_json::json!({
                    "external_update_url": offer.update_url,
                }))
                .expect("JSON serialises")
                    + "\n";
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| err("creating", e))?;
                }
                std::fs::write(path, text).map_err(|e| err("writing", e))?;
            }
            OfferLocation::Registry(key) => {
                registry::set(key, offer.update_url).map_err(|e| err("setting", e))?;
            }
        }
        done.push(offer);
    }
    Ok(done)
}

/// Removes every offer this build knows of. Returns what was removed.
pub fn withdraw(os: Os, env: &Env, ids: &ExtensionIds) -> Result<Vec<Offer>, RegisterError> {
    // Withdraw Chrome Web Store offers even with the switch off: an older
    // version may have made them.
    let mut ids = ids.clone();
    ids.chrome_web_store.external_install = ids.chrome_web_store.id.is_some();
    let mut removed = Vec::new();
    for offer in offers(os, env, &ids)? {
        let err = |action, source| RegisterError::Io {
            action,
            target: offer.location.to_string(),
            source,
        };
        let gone = match &offer.location {
            OfferLocation::File(path) => match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                Err(e) => return Err(err("removing", e)),
            },
            OfferLocation::Registry(key) => {
                registry::delete(key).map_err(|e| err("deleting", e))?
            }
        };
        if gone {
            removed.push(offer);
        }
    }
    Ok(removed)
}

#[cfg(windows)]
mod registry {
    use std::io;

    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    pub fn set(key: &str, update_url: &str) -> io::Result<()> {
        let (subkey, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(key)?;
        subkey.set_value("update_url", &update_url.to_owned())
    }

    pub fn delete(key: &str) -> io::Result<bool> {
        match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(key) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(not(windows))]
mod registry {
    use std::io;

    pub fn set(_key: &str, _update_url: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the registry exists only on Windows",
        ))
    }

    pub fn delete(_key: &str) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEV_CHROMIUM_EXTENSION_ID;

    const EDGE: &str = "aaaabbbbccccddddeeeeffffgggghhhh";
    const CWS: &str = "ppppoooonnnnmmmmllllkkkkjjjjiiii";

    fn ids(edge: Option<&str>, cws: Option<&str>, external: bool) -> ExtensionIds {
        ExtensionIds {
            firefox: crate::ids::Firefox {
                id: "a@b".into(),
                amo_slug: None,
            },
            edge: crate::ids::Edge {
                id: edge.map(Into::into),
            },
            chrome_web_store: crate::ids::ChromeWebStore {
                id: cws.map(Into::into),
                external_install: external,
            },
            github: crate::ids::Github {
                chromium_id: DEV_CHROMIUM_EXTENSION_ID.into(),
            },
        }
    }

    fn env(home: &str) -> Env {
        Env {
            home: Some(PathBuf::from(home)),
            local_app_data: Some(PathBuf::from(r"C:\Users\u\AppData\Local")),
            ..Env::default()
        }
    }

    #[test]
    fn nothing_without_store_ids() {
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert!(
                offers(os, &env("/home/u"), &ids(None, None, false))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn chrome_waits_for_the_switch() {
        let e = env("/Users/u");
        let off = offers(Os::MacOs, &e, &ids(Some(EDGE), Some(CWS), false)).unwrap();
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].browser, Browser::Edge);
        assert_eq!(off[0].update_url, EDGE_UPDATE_URL);
        assert_eq!(
            off[0].location,
            OfferLocation::File(PathBuf::from(format!(
                "/Users/u/Library/Application Support/Microsoft Edge/External Extensions/{EDGE}.json"
            )))
        );
        let on = offers(Os::MacOs, &e, &ids(Some(EDGE), Some(CWS), true)).unwrap();
        let browsers: Vec<_> = on.iter().map(|o| o.browser).collect();
        assert_eq!(
            browsers,
            [
                Browser::Edge,
                Browser::Chrome,
                Browser::Chromium,
                Browser::Brave,
                Browser::Vivaldi
            ]
        );
        assert!(
            on[1..]
                .iter()
                .all(|o| o.update_url == CHROME_WEB_STORE_UPDATE_URL && o.id == CWS)
        );
    }

    #[test]
    fn windows_uses_hkcu_keys_and_linux_nothing() {
        let all = ids(Some(EDGE), Some(CWS), true);
        let win = offers(Os::Windows, &env("C:/Users/u"), &all).unwrap();
        let keys: Vec<_> = win.iter().map(|o| o.location.to_string()).collect();
        assert_eq!(
            keys,
            [
                format!(r"HKCU\Software\Microsoft\Edge\Extensions\{EDGE}"),
                format!(r"HKCU\Software\Google\Chrome\Extensions\{CWS}"),
            ]
        );
        assert!(offers(Os::Linux, &env("/home/u"), &all).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn offer_and_withdraw_files_on_macos() {
        let home = std::env::temp_dir().join(format!("whoosh-offer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let e = Env {
            home: Some(home.clone()),
            ..Env::default()
        };
        let edge_profile = home.join("Library/Application Support/Microsoft Edge");
        std::fs::create_dir_all(&edge_profile).unwrap();
        let all = ids(Some(EDGE), Some(CWS), true);

        // Only Edge has been used: Chrome and the others get no file.
        let written = offer(Os::MacOs, &e, &all, false).unwrap();
        assert_eq!(written.len(), 1);
        let file = edge_profile.join(format!("External Extensions/{EDGE}.json"));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(json["external_update_url"], EDGE_UPDATE_URL);

        // The switch turned off later still withdraws a Chrome offer.
        assert_eq!(offer(Os::MacOs, &e, &all, true).unwrap().len(), 5);
        let removed = withdraw(Os::MacOs, &e, &ids(Some(EDGE), Some(CWS), false)).unwrap();
        assert_eq!(removed.len(), 5);
        assert!(!file.exists());
        assert!(withdraw(Os::MacOs, &e, &all).unwrap().is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
