// SPDX-License-Identifier: GPL-3.0-or-later
//! `desktop.json`, the desktop app's own settings, in its config directory
//! (`<config dir>/com.coderzonebd.whoosh/`, where Tauri's `app_config_dir`
//! is). The app writes it; the tray process reads it (the language, the
//! update check, where the media tools are).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "desktop.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    /// Closing the window leaves WDM in the tray; downloads go on.
    pub close_to_tray: bool,
    /// The "still running in the tray" notice has been shown once.
    pub tray_notice_shown: bool,
    /// The UI's language when it last ran, so the tray follows it. Empty:
    /// English.
    pub language: String,
    /// Look for a new version once a day. Never installs alone where that
    /// needs the password.
    pub auto_update_check: bool,
    /// When the app last looked for an update (Unix seconds; 0 = never).
    pub last_update_check: u64,
    /// A new version announced where installing needs the password, until
    /// it is installed; the main window shows it when it opens.
    pub announced_update: String,
    /// The "Connect your browser" page has been shown once (first run).
    pub browser_setup_shown: bool,
    /// Launch at login was turned on at the first start; never again, so
    /// turning it off in Settings sticks.
    pub login_item_set: bool,
    /// The bundled media tools, for the daemon the tray starts (written by
    /// the app, which knows its resource directory). Empty: none.
    pub media_tools_dir: String,
    /// The main window's details panel was closed (its x); the toolbar's
    /// Details button shows it again.
    pub hide_details: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            tray_notice_shown: false,
            language: String::new(),
            auto_update_check: true,
            last_update_check: 0,
            announced_update: String::new(),
            browser_setup_shown: false,
            login_item_set: false,
            media_tools_dir: String::new(),
            hide_details: false,
        }
    }
}

/// Where `desktop.json` is.
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(crate::IDENTIFIER).join(FILE))
}

/// The prefs from `file`; defaults when it is missing or unreadable.
pub fn load_from(file: &std::path::Path) -> Prefs {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The prefs; defaults when there are none yet.
pub fn load() -> Prefs {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Writes them to `file` (through a temporary file, so a reader never
/// sees half of it).
pub fn save_to(file: &std::path::Path, prefs: &Prefs) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(prefs).map_err(std::io::Error::other)?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let p = Prefs::default();
        assert!(p.close_to_tray);
        assert!(p.auto_update_check);
        assert!(!p.browser_setup_shown);
        // Not set yet: the first start of an installed build turns launch at login on.
        assert!(!p.login_item_set);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let p: Prefs = serde_json::from_str(r#"{"closeToTray": false}"#).unwrap();
        assert!(!p.close_to_tray);
        assert!(!p.tray_notice_shown);
        assert!(p.media_tools_dir.is_empty());
    }

    #[test]
    fn saved_prefs_read_back() {
        let dir = std::env::temp_dir().join(format!("wdm-prefs-{}", std::process::id()));
        let file = dir.join(FILE);
        let p = Prefs {
            language: "bn".into(),
            media_tools_dir: "/opt/wdm/media-tools".into(),
            ..Prefs::default()
        };
        save_to(&file, &p).unwrap();
        assert_eq!(load_from(&file), p);
        let _ = std::fs::remove_dir_all(dir);
    }
}
