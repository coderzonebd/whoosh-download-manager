// SPDX-License-Identifier: GPL-3.0-or-later
//! "Check for a new version" from the native main window. Updating needs
//! the app's updater (Tauri's, with the signing key), so the window starts
//! `wdm-desktop --update-now check` (or `install`) and reads the answer it
//! leaves in the data directory ([`FILE`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `wdm-desktop --update-now <check|install>`.
pub const ARG: &str = "--update-now";
/// The answer, in the data directory.
pub const FILE: &str = "update-state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// This is the newest version.
    Current,
    /// A newer version can be installed (`version`).
    Available,
    /// Downloading and installing it.
    Installing,
    /// Installed: the window closes so the new version can open.
    Installed,
    /// Checking or installing failed (`error`).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// When it was written (Unix milliseconds): older answers are not this request's.
    pub at: i64,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A package manager updates this copy (Flatpak, Scoop, pacman), and its command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Writes the answer (whole, through a temporary file).
pub fn write(dir: &Path, state: &State) -> std::io::Result<()> {
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(state).unwrap_or_default())?;
    std::fs::rename(tmp, dir.join(FILE))
}

pub fn read(dir: &Path) -> Option<State> {
    serde_json::from_slice(&std::fs::read(dir.join(FILE)).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_goes_through_the_file() {
        let dir = std::env::temp_dir().join(format!("wdm-update-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir).is_none());
        let s = State {
            at: 5,
            kind: Kind::Available,
            version: Some("1.0.0".into()),
            error: None,
            managed_by: None,
            command: None,
        };
        write(&dir, &s).unwrap();
        assert_eq!(read(&dir), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
