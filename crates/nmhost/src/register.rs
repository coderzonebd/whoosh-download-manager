//! `--register` / `--unregister`: install the host manifest for the current
//! user so browsers can launch `whoosh-nmhost`.
//!
//! * Linux and macOS: one JSON manifest per browser in that browser's
//!   user-level `NativeMessagingHosts` directory.
//! * Windows: the manifests live under `%LOCALAPPDATA%\whoosh\nmhost\` and a
//!   per-browser key under `HKEY_CURRENT_USER` points at them.
//!
//! Linux is tested. macOS and Windows locations follow each browser's
//! documentation but have not been run on those systems yet.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::HOST_NAME;
use crate::runtime::{Env, Os};

const DESCRIPTION: &str = "whoosh download manager — browser integration";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Browser {
    Chrome,
    Chromium,
    Brave,
    Edge,
    Vivaldi,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 6] = [
        Browser::Chrome,
        Browser::Chromium,
        Browser::Brave,
        Browser::Edge,
        Browser::Vivaldi,
        Browser::Firefox,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome",
            Browser::Chromium => "chromium",
            Browser::Brave => "brave",
            Browser::Edge => "edge",
            Browser::Vivaldi => "vivaldi",
            Browser::Firefox => "firefox",
        }
    }

    pub fn parse(name: &str) -> Option<Browser> {
        Browser::ALL
            .into_iter()
            .find(|b| b.name().eq_ignore_ascii_case(name))
    }

    pub fn family(self) -> Family {
        match self {
            Browser::Firefox => Family::Firefox,
            _ => Family::Chromium,
        }
    }
}

/// Chromium-based browsers share one manifest format (`allowed_origins`),
/// Firefox uses another (`allowed_extensions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Chromium,
    Firefox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The manifest file itself (Linux, macOS).
    File(PathBuf),
    /// A manifest file plus an `HKCU\<key>` whose default value is its path
    /// (Windows).
    Registry { key: String, manifest: PathBuf },
}

impl Location {
    pub fn manifest(&self) -> &Path {
        match self {
            Location::File(path) | Location::Registry { manifest: path, .. } => path,
        }
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::File(path) => write!(f, "{}", path.display()),
            Location::Registry { key, manifest } => {
                write!(f, r"HKCU\{key} -> {}", manifest.display())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub browser: Browser,
    pub location: Location,
    /// A directory that exists when the browser has been used by this user.
    pub profile_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("cannot locate the user's directories: {0} is not set")]
    NoBaseDir(&'static str),
    #[error("no supported browser profile found for this user; name one with --browser")]
    NoBrowserFound,
    #[error("{0:?} is not a Chromium extension ID (32 letters a-p)")]
    BadChromiumId(String),
    #[error("{0:?} is not a Firefox extension ID")]
    BadFirefoxId(String),
    #[error("the host path {0} is not absolute")]
    RelativeHostPath(PathBuf),
    #[error("{action} {target}: {source}")]
    Io {
        action: &'static str,
        target: String,
        source: io::Error,
    },
}

/// Every place a manifest can go for `browser` on `os`.
pub fn targets(os: Os, env: &Env, browser: Browser) -> Result<Vec<Target>, RegisterError> {
    let home = || env.home.clone().ok_or(RegisterError::NoBaseDir("HOME"));
    let file = |profile_dir: PathBuf, hosts_dir: PathBuf| Target {
        browser,
        location: Location::File(hosts_dir.join(format!("{HOST_NAME}.json"))),
        profile_dir,
    };

    Ok(match os {
        Os::Linux => {
            let config = match &env.xdg_config_home {
                Some(dir) => dir.clone(),
                None => home()?.join(".config"),
            };
            match browser {
                Browser::Firefox => {
                    let home = home()?;
                    vec![
                        file(
                            home.join(".mozilla"),
                            home.join(".mozilla/native-messaging-hosts"),
                        ),
                        // Firefox 147+ can keep its files under XDG_CONFIG_HOME.
                        file(
                            config.join("mozilla"),
                            config.join("mozilla/native-messaging-hosts"),
                        ),
                    ]
                }
                _ => {
                    let profile = config.join(match browser {
                        Browser::Chrome => "google-chrome",
                        Browser::Chromium => "chromium",
                        Browser::Brave => "BraveSoftware/Brave-Browser",
                        Browser::Edge => "microsoft-edge",
                        Browser::Vivaldi => "vivaldi",
                        Browser::Firefox => unreachable!(),
                    });
                    let hosts = profile.join("NativeMessagingHosts");
                    vec![file(profile, hosts)]
                }
            }
        }
        Os::MacOs => {
            let support = home()?.join("Library/Application Support");
            let (profile, hosts) = match browser {
                Browser::Chrome => ("Google/Chrome", "Google/Chrome/NativeMessagingHosts"),
                Browser::Chromium => ("Chromium", "Chromium/NativeMessagingHosts"),
                Browser::Brave => (
                    "BraveSoftware/Brave-Browser",
                    "BraveSoftware/Brave-Browser/NativeMessagingHosts",
                ),
                Browser::Edge => ("Microsoft Edge", "Microsoft Edge/NativeMessagingHosts"),
                Browser::Vivaldi => ("Vivaldi", "Vivaldi/NativeMessagingHosts"),
                Browser::Firefox => ("Firefox", "Mozilla/NativeMessagingHosts"),
            };
            vec![file(support.join(profile), support.join(hosts))]
        }
        Os::Windows => {
            let local = env
                .local_app_data
                .clone()
                .ok_or(RegisterError::NoBaseDir("LOCALAPPDATA"))?;
            let manifest_dir = local.join("whoosh").join("nmhost");
            let manifest = manifest_dir.join(match browser.family() {
                Family::Chromium => format!("{HOST_NAME}.chromium.json"),
                Family::Firefox => format!("{HOST_NAME}.firefox.json"),
            });
            let (key_prefix, profile) = match browser {
                Browser::Chrome => (r"Software\Google\Chrome", r"Google\Chrome"),
                Browser::Chromium => (r"Software\Chromium", "Chromium"),
                Browser::Brave => (
                    r"Software\BraveSoftware\Brave-Browser",
                    r"BraveSoftware\Brave-Browser",
                ),
                Browser::Edge => (r"Software\Microsoft\Edge", r"Microsoft\Edge"),
                Browser::Vivaldi => (r"Software\Vivaldi", "Vivaldi"),
                Browser::Firefox => (r"Software\Mozilla", r"Mozilla\Firefox"),
            };
            vec![Target {
                browser,
                location: Location::Registry {
                    key: format!(r"{key_prefix}\NativeMessagingHosts\{HOST_NAME}"),
                    manifest,
                },
                profile_dir: local.join(profile),
            }]
        }
    })
}

/// The manifest for Chromium-based browsers.
pub fn chromium_manifest(host_path: &Path, extension_ids: &[String]) -> Value {
    let origins: Vec<String> = extension_ids
        .iter()
        .map(|id| format!("chrome-extension://{id}/"))
        .collect();
    json!({
        "name": HOST_NAME,
        "description": DESCRIPTION,
        "path": host_path,
        "type": "stdio",
        "allowed_origins": origins,
    })
}

/// The manifest for Firefox.
pub fn firefox_manifest(host_path: &Path, extension_ids: &[String]) -> Value {
    json!({
        "name": HOST_NAME,
        "description": DESCRIPTION,
        "path": host_path,
        "type": "stdio",
        "allowed_extensions": extension_ids,
    })
}

pub fn is_chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

pub fn is_firefox_id(id: &str) -> bool {
    // Either an email-like ID or a {GUID}; we only reject obvious junk.
    !id.is_empty() && id.len() <= 255 && !id.chars().any(char::is_whitespace)
}

#[derive(Debug, Clone)]
pub struct Options {
    /// `None` = every browser this user has a profile for.
    pub browsers: Option<Vec<Browser>>,
    pub host_path: PathBuf,
    pub chromium_ids: Vec<String>,
    pub firefox_ids: Vec<String>,
}

/// What `register` / `unregister` did, for printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub browser: Browser,
    pub location: Location,
    pub done: bool,
}

pub fn register(os: Os, env: &Env, opts: &Options) -> Result<Vec<Change>, RegisterError> {
    if !opts.host_path.is_absolute() {
        return Err(RegisterError::RelativeHostPath(opts.host_path.clone()));
    }
    if let Some(bad) = opts.chromium_ids.iter().find(|id| !is_chromium_id(id)) {
        return Err(RegisterError::BadChromiumId(bad.clone()));
    }
    if let Some(bad) = opts.firefox_ids.iter().find(|id| !is_firefox_id(id)) {
        return Err(RegisterError::BadFirefoxId(bad.clone()));
    }

    let explicit = opts.browsers.is_some();
    let browsers = opts
        .browsers
        .clone()
        .unwrap_or_else(|| Browser::ALL.to_vec());
    let mut chosen = Vec::new();
    for browser in browsers {
        let all = targets(os, env, browser)?;
        if explicit {
            // Always write the primary location; extra ones only where the
            // browser actually keeps its files.
            let mut it = all.into_iter();
            chosen.extend(it.next());
            chosen.extend(it.filter(|t| t.profile_dir.is_dir()));
        } else {
            chosen.extend(all.into_iter().filter(|t| t.profile_dir.is_dir()));
        }
    }
    if chosen.is_empty() {
        return Err(RegisterError::NoBrowserFound);
    }

    let mut changes = Vec::new();
    for target in chosen {
        let manifest = match target.browser.family() {
            Family::Chromium => chromium_manifest(&opts.host_path, &opts.chromium_ids),
            Family::Firefox => firefox_manifest(&opts.host_path, &opts.firefox_ids),
        };
        let text = serde_json::to_string_pretty(&manifest).expect("manifest serialises") + "\n";
        write_atomically(target.location.manifest(), text.as_bytes()).map_err(|source| {
            RegisterError::Io {
                action: "writing",
                target: target.location.manifest().display().to_string(),
                source,
            }
        })?;
        if let Location::Registry { key, manifest } = &target.location {
            registry::set(key, manifest).map_err(|source| RegisterError::Io {
                action: "setting",
                target: format!(r"HKCU\{key}"),
                source,
            })?;
        }
        changes.push(Change {
            browser: target.browser,
            location: target.location,
            done: true,
        });
    }
    Ok(changes)
}

/// Removes the manifest from every location for every browser. `done` is
/// false where there was nothing to remove.
pub fn unregister(os: Os, env: &Env) -> Result<Vec<Change>, RegisterError> {
    let mut changes = Vec::new();
    for browser in Browser::ALL {
        for target in targets(os, env, browser)? {
            let path = target.location.manifest();
            let mut done = match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                Err(source) => {
                    return Err(RegisterError::Io {
                        action: "removing",
                        target: path.display().to_string(),
                        source,
                    });
                }
            };
            if let Location::Registry { key, .. } = &target.location {
                done |= registry::delete(key).map_err(|source| RegisterError::Io {
                    action: "deleting",
                    target: format!(r"HKCU\{key}"),
                    source,
                })?;
            }
            changes.push(Change {
                browser,
                location: target.location,
                done,
            });
        }
    }
    Ok(changes)
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(windows)]
mod registry {
    use std::io;
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    pub fn set(key: &str, manifest: &Path) -> io::Result<()> {
        let (subkey, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(key)?;
        subkey.set_value("", &manifest.to_string_lossy().into_owned())
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
    use std::path::Path;

    pub fn set(_key: &str, _manifest: &Path) -> io::Result<()> {
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
    use crate::{DEV_CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID};

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "whoosh-nmhost-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn env(&self) -> Env {
            Env {
                home: Some(self.0.clone()),
                ..Env::default()
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An absolute host path on every OS (on Windows one needs a drive).
    const HOST: &str = if cfg!(windows) {
        r"C:\Program Files\Whoosh\whoosh-nmhost.exe"
    } else {
        "/opt/whoosh/whoosh-nmhost"
    };

    fn opts(browsers: Option<Vec<Browser>>) -> Options {
        Options {
            browsers,
            host_path: PathBuf::from(HOST),
            chromium_ids: vec![DEV_CHROMIUM_EXTENSION_ID.to_owned()],
            firefox_ids: vec![FIREFOX_EXTENSION_ID.to_owned()],
        }
    }

    fn only_file(os: Os, env: &Env, browser: Browser) -> PathBuf {
        match &targets(os, env, browser).unwrap()[0].location {
            Location::File(path) => path.clone(),
            other => panic!("expected a file, got {other:?}"),
        }
    }

    #[test]
    fn linux_locations() {
        let env = Env {
            home: Some("/home/u".into()),
            ..Env::default()
        };
        let host = "com.coderzonebd.whoosh.json";
        let cases = [
            (
                Browser::Chrome,
                "/home/u/.config/google-chrome/NativeMessagingHosts",
            ),
            (
                Browser::Chromium,
                "/home/u/.config/chromium/NativeMessagingHosts",
            ),
            (
                Browser::Brave,
                "/home/u/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
            ),
            (
                Browser::Edge,
                "/home/u/.config/microsoft-edge/NativeMessagingHosts",
            ),
            (
                Browser::Vivaldi,
                "/home/u/.config/vivaldi/NativeMessagingHosts",
            ),
            (Browser::Firefox, "/home/u/.mozilla/native-messaging-hosts"),
        ];
        for (browser, dir) in cases {
            assert_eq!(
                only_file(Os::Linux, &env, browser),
                Path::new(dir).join(host)
            );
        }
    }

    #[test]
    fn linux_honours_xdg_config_home() {
        let env = Env {
            home: Some("/home/u".into()),
            xdg_config_home: Some("/cfg".into()),
            ..Env::default()
        };
        assert_eq!(
            only_file(Os::Linux, &env, Browser::Chromium),
            PathBuf::from("/cfg/chromium/NativeMessagingHosts/com.coderzonebd.whoosh.json")
        );
    }

    #[test]
    fn macos_locations() {
        let env = Env {
            home: Some("/Users/u".into()),
            ..Env::default()
        };
        let support = "/Users/u/Library/Application Support";
        assert_eq!(
            only_file(Os::MacOs, &env, Browser::Chrome),
            PathBuf::from(format!(
                "{support}/Google/Chrome/NativeMessagingHosts/com.coderzonebd.whoosh.json"
            ))
        );
        assert_eq!(
            only_file(Os::MacOs, &env, Browser::Edge),
            PathBuf::from(format!(
                "{support}/Microsoft Edge/NativeMessagingHosts/com.coderzonebd.whoosh.json"
            ))
        );
        assert_eq!(
            only_file(Os::MacOs, &env, Browser::Firefox),
            PathBuf::from(format!(
                "{support}/Mozilla/NativeMessagingHosts/com.coderzonebd.whoosh.json"
            ))
        );
    }

    #[test]
    fn windows_uses_hkcu_keys() {
        let env = Env {
            local_app_data: Some(r"C:\Users\u\AppData\Local".into()),
            ..Env::default()
        };
        let chrome = &targets(Os::Windows, &env, Browser::Chrome).unwrap()[0];
        let Location::Registry { key, manifest } = &chrome.location else {
            panic!("expected a registry location");
        };
        assert_eq!(
            key,
            r"Software\Google\Chrome\NativeMessagingHosts\com.coderzonebd.whoosh"
        );
        assert!(manifest.ends_with("com.coderzonebd.whoosh.chromium.json"));

        let firefox = &targets(Os::Windows, &env, Browser::Firefox).unwrap()[0];
        let Location::Registry { key, manifest } = &firefox.location else {
            panic!("expected a registry location");
        };
        assert_eq!(
            key,
            r"Software\Mozilla\NativeMessagingHosts\com.coderzonebd.whoosh"
        );
        assert!(manifest.ends_with("com.coderzonebd.whoosh.firefox.json"));
    }

    #[test]
    fn manifests_have_the_right_shape() {
        let host = Path::new(HOST);
        let chromium = chromium_manifest(host, &[DEV_CHROMIUM_EXTENSION_ID.to_owned()]);
        assert_eq!(chromium["name"], "com.coderzonebd.whoosh");
        assert_eq!(chromium["type"], "stdio");
        assert_eq!(chromium["path"], HOST);
        assert_eq!(
            chromium["allowed_origins"],
            json!(["chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl/"])
        );
        assert!(chromium.get("allowed_extensions").is_none());

        let firefox = firefox_manifest(host, &[FIREFOX_EXTENSION_ID.to_owned()]);
        assert_eq!(
            firefox["allowed_extensions"],
            json!(["whoosh@coderzonebd.com"])
        );
        assert!(firefox.get("allowed_origins").is_none());
    }

    #[test]
    fn dev_id_is_a_valid_chromium_id() {
        assert!(is_chromium_id(DEV_CHROMIUM_EXTENSION_ID));
        assert!(!is_chromium_id("not-an-id"));
        assert!(!is_chromium_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
        assert!(is_firefox_id(FIREFOX_EXTENSION_ID));
        assert!(!is_firefox_id("has space"));
    }

    #[test]
    fn register_detected_browsers_then_unregister() {
        let home = TempHome::new("reg");
        let env = home.env();
        // Pretend Chromium and Firefox have been used; Chrome has not.
        std::fs::create_dir_all(home.0.join(".config/chromium")).unwrap();
        std::fs::create_dir_all(home.0.join(".mozilla")).unwrap();

        let changes = register(Os::Linux, &env, &opts(None)).unwrap();
        let browsers: Vec<Browser> = changes.iter().map(|c| c.browser).collect();
        assert_eq!(browsers, vec![Browser::Chromium, Browser::Firefox]);

        let chromium_path = home
            .0
            .join(".config/chromium/NativeMessagingHosts/com.coderzonebd.whoosh.json");
        let written: Value =
            serde_json::from_slice(&std::fs::read(&chromium_path).unwrap()).unwrap();
        assert_eq!(
            written["allowed_origins"][0],
            "chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl/"
        );
        let firefox_path = home
            .0
            .join(".mozilla/native-messaging-hosts/com.coderzonebd.whoosh.json");
        let written: Value =
            serde_json::from_slice(&std::fs::read(&firefox_path).unwrap()).unwrap();
        assert_eq!(written["allowed_extensions"][0], "whoosh@coderzonebd.com");
        assert!(!home.0.join(".config/google-chrome").exists());

        let removed = unregister(Os::Linux, &env).unwrap();
        let done: Vec<Browser> = removed
            .iter()
            .filter(|c| c.done)
            .map(|c| c.browser)
            .collect();
        assert_eq!(done, vec![Browser::Chromium, Browser::Firefox]);
        assert!(!chromium_path.exists());
        assert!(!firefox_path.exists());
    }

    #[test]
    fn explicit_browser_is_written_even_without_a_profile() {
        let home = TempHome::new("explicit");
        let changes = register(Os::Linux, &home.env(), &opts(Some(vec![Browser::Chrome]))).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            home.0
                .join(".config/google-chrome/NativeMessagingHosts/com.coderzonebd.whoosh.json")
                .is_file()
        );
    }

    #[test]
    fn nothing_detected_is_an_error() {
        let home = TempHome::new("none");
        let err = register(Os::Linux, &home.env(), &opts(None)).unwrap_err();
        assert!(matches!(err, RegisterError::NoBrowserFound));
    }

    #[test]
    fn rejects_bad_input() {
        let home = TempHome::new("bad");
        let mut o = opts(Some(vec![Browser::Chrome]));
        o.chromium_ids.push("nope".into());
        assert!(matches!(
            register(Os::Linux, &home.env(), &o),
            Err(RegisterError::BadChromiumId(_))
        ));
        let mut o = opts(Some(vec![Browser::Chrome]));
        o.host_path = "relative/whoosh-nmhost".into();
        assert!(matches!(
            register(Os::Linux, &home.env(), &o),
            Err(RegisterError::RelativeHostPath(_))
        ));
    }
}
