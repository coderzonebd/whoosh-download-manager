//! What the desktop app's "Connect your browser" page shows: for each
//! browser, whether this user has used it, whether the host is registered
//! for it (per user or system-wide), and whether our extension appears in one
//! of its profiles. Best effort and read-only: a file we cannot read counts
//! as "not found".

use std::path::{Path, PathBuf};

use crate::HOST_NAME;
use crate::register::{Browser, Family, Location, targets};
use crate::runtime::{Env, Os};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserStatus {
    pub browser: Browser,
    /// The browser's profile directory exists for this user.
    pub in_use: bool,
    /// A host manifest is in place: this user's, or one for every user.
    pub host_registered: bool,
    /// Registered system-wide (by the `.deb`/`.rpm` or the MSI).
    pub system_registered: bool,
    /// One of `extension_ids` is installed in a profile.
    pub extension_found: bool,
}

/// The status of every browser, `chromium_ids` / `firefox_ids` being the
/// extension IDs to look for.
pub fn status(
    os: Os,
    env: &Env,
    chromium_ids: &[String],
    firefox_ids: &[String],
) -> Vec<BrowserStatus> {
    Browser::ALL
        .into_iter()
        .map(|browser| {
            let found = targets(os, env, browser).unwrap_or_default();
            let in_use = found.iter().any(|t| t.profile_dir.is_dir());
            let user = found.iter().any(|t| user_registered(&t.location));
            let system = system_registered(os, browser);
            let ids = match browser.family() {
                Family::Chromium => chromium_ids,
                Family::Firefox => firefox_ids,
            };
            let profile_roots: Vec<PathBuf> = match browser.family() {
                Family::Chromium => found
                    .first()
                    .map(|t| {
                        if os == Os::Windows {
                            t.profile_dir.join("User Data")
                        } else {
                            t.profile_dir.clone()
                        }
                    })
                    .into_iter()
                    .collect(),
                Family::Firefox => firefox_profile_roots(os, env),
            };
            BrowserStatus {
                browser,
                in_use,
                host_registered: user || system,
                system_registered: system,
                extension_found: profile_roots
                    .iter()
                    .any(|root| extension_in(browser.family(), root, ids)),
            }
        })
        .collect()
}

fn user_registered(location: &Location) -> bool {
    match location {
        Location::File(path) => path.is_file(),
        Location::Registry { key, manifest } => manifest.is_file() && registry::exists(false, key),
    }
}

/// Directories where a package installs the host manifest for every user.
/// `packaging/browsers/generate.mjs` installs into the Linux ones.
pub fn system_manifest_dirs(os: Os, browser: Browser) -> &'static [&'static str] {
    match (os, browser) {
        (Os::Linux, Browser::Chrome | Browser::Brave | Browser::Vivaldi) => {
            &["/etc/opt/chrome/native-messaging-hosts"]
        }
        (Os::Linux, Browser::Chromium) => &["/etc/chromium/native-messaging-hosts"],
        (Os::Linux, Browser::Edge) => &["/etc/opt/edge/native-messaging-hosts"],
        (Os::Linux, Browser::Firefox) => &[
            "/usr/lib/mozilla/native-messaging-hosts",
            "/usr/lib64/mozilla/native-messaging-hosts",
        ],
        (Os::MacOs, Browser::Chrome | Browser::Brave | Browser::Vivaldi) => {
            &["/Library/Google/Chrome/NativeMessagingHosts"]
        }
        (Os::MacOs, Browser::Chromium) => {
            &["/Library/Application Support/Chromium/NativeMessagingHosts"]
        }
        (Os::MacOs, Browser::Edge) => &["/Library/Microsoft/Edge/NativeMessagingHosts"],
        (Os::MacOs, Browser::Firefox) => {
            &["/Library/Application Support/Mozilla/NativeMessagingHosts"]
        }
        (Os::Windows, _) => &[],
    }
}

/// `HKLM` keys the MSI writes (`packaging/browsers/generate.mjs`).
pub fn system_registry_key(browser: Browser) -> String {
    let prefix = match browser {
        Browser::Chrome | Browser::Brave | Browser::Vivaldi => r"Software\Google\Chrome",
        Browser::Chromium => r"Software\Chromium",
        Browser::Edge => r"Software\Microsoft\Edge",
        Browser::Firefox => r"Software\Mozilla",
    };
    format!(r"{prefix}\NativeMessagingHosts\{HOST_NAME}")
}

/// The host is registered for every user of this machine.
pub fn system_registered(os: Os, browser: Browser) -> bool {
    match os {
        Os::Windows => registry::exists(true, &system_registry_key(browser)),
        _ => system_manifest_dirs(os, browser)
            .iter()
            .any(|dir| Path::new(dir).join(format!("{HOST_NAME}.json")).is_file()),
    }
}

fn firefox_profile_roots(os: Os, env: &Env) -> Vec<PathBuf> {
    let home = env.home.clone();
    match os {
        Os::Linux => {
            let mut roots: Vec<PathBuf> = home.iter().map(|h| h.join(".mozilla/firefox")).collect();
            let config = env
                .xdg_config_home
                .clone()
                .or_else(|| home.map(|h| h.join(".config")));
            roots.extend(config.map(|c| c.join("mozilla/firefox")));
            roots
        }
        Os::MacOs => home
            .map(|h| h.join("Library/Application Support/Firefox/Profiles"))
            .into_iter()
            .collect(),
        Os::Windows => std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(r"AppData\Roaming")))
            .map(|d| d.join(r"Mozilla\Firefox\Profiles"))
            .into_iter()
            .collect(),
    }
}

/// Whether one of `ids` is installed in a profile under `root`.
pub fn extension_in(family: Family, root: &Path, ids: &[String]) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let profile = entry.path();
        if !profile.is_dir() {
            continue;
        }
        let found = match family {
            // Store installs unpack into Extensions/<id>; an unpacked
            // (developer mode) install is only named in the preferences.
            Family::Chromium => ids.iter().any(|id| {
                profile.join("Extensions").join(id).is_dir()
                    || mentions(&profile.join("Secure Preferences"), id)
                    || mentions(&profile.join("Preferences"), id)
            }),
            Family::Firefox => ids.iter().any(|id| {
                profile
                    .join("extensions")
                    .join(format!("{id}.xpi"))
                    .is_file()
                    || mentions(&profile.join("extensions.json"), id)
            }),
        };
        if found {
            return true;
        }
    }
    false
}

fn mentions(file: &Path, id: &str) -> bool {
    std::fs::read_to_string(file).is_ok_and(|text| text.contains(&format!("\"{id}\"")))
}

#[cfg(windows)]
mod registry {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

    pub fn exists(machine: bool, key: &str) -> bool {
        let root = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        };
        RegKey::predef(root)
            .open_subkey_with_flags(key, KEY_READ)
            .is_ok()
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn exists(_machine: bool, _key: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEV_CHROMIUM_EXTENSION_ID;

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("whoosh-detect-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_store_and_unpacked_chromium_installs() {
        let t = Temp::new("chromium");
        let id = DEV_CHROMIUM_EXTENSION_ID.to_owned();
        assert!(!extension_in(
            Family::Chromium,
            &t.0,
            std::slice::from_ref(&id)
        ));
        std::fs::create_dir_all(t.0.join("Profile 2")).unwrap();
        std::fs::write(
            t.0.join("Profile 2/Preferences"),
            format!(r#"{{"x":{{"{id}":{{}}}}}}"#),
        )
        .unwrap();
        assert!(extension_in(
            Family::Chromium,
            &t.0,
            std::slice::from_ref(&id)
        ));
        let other = Temp::new("chromium-store");
        std::fs::create_dir_all(other.0.join(format!("Default/Extensions/{id}/1.0"))).unwrap();
        assert!(extension_in(Family::Chromium, &other.0, &[id]));
    }

    #[test]
    fn finds_firefox_installs() {
        let t = Temp::new("firefox");
        let id = "whoosh@coderzonebd.com".to_owned();
        std::fs::create_dir_all(t.0.join("abc.default-release/extensions")).unwrap();
        assert!(!extension_in(
            Family::Firefox,
            &t.0,
            std::slice::from_ref(&id)
        ));
        std::fs::write(
            t.0.join(format!("abc.default-release/extensions/{id}.xpi")),
            b"zip",
        )
        .unwrap();
        assert!(extension_in(Family::Firefox, &t.0, &[id]));
    }

    #[test]
    fn status_sees_a_user_registration_and_a_profile() {
        let t = Temp::new("status");
        let env = Env {
            home: Some(t.0.clone()),
            ..Env::default()
        };
        let os = if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        };
        if cfg!(windows) {
            return; // registry-based; covered by the register tests' layout
        }
        let chrome = &targets(os, &env, Browser::Chrome).unwrap()[0];
        std::fs::create_dir_all(chrome.profile_dir.join("Default")).unwrap();
        let before = status(os, &env, &[], &[]);
        let c = before
            .iter()
            .find(|s| s.browser == Browser::Chrome)
            .unwrap();
        assert!(c.in_use);
        assert_eq!(c.host_registered, c.system_registered);
        let manifest = chrome.location.manifest();
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(manifest, "{}").unwrap();
        let after = status(os, &env, &[], &[]);
        assert!(
            after
                .iter()
                .find(|s| s.browser == Browser::Chrome)
                .unwrap()
                .host_registered
        );
        let firefox = after
            .iter()
            .find(|s| s.browser == Browser::Firefox)
            .unwrap();
        assert!(!firefox.in_use && !firefox.extension_found);
    }
}
