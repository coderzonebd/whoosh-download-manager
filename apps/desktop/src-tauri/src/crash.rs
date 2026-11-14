// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in crash reports for the shell and its web view ([`whoosh_crash`]).
//!
//! The choice ("Send crash reports" in Settings, off by default) lives in
//! the daemon's data directory, so the daemon honours the same switch the
//! next time it starts. The web view never talks to the crash-report
//! service itself (its CSP allows only the local API): uncaught errors come
//! here through [`report_ui_error`] and leave scrubbed, like the shell's own
//! panics. A build without `WHOOSH_CRASH_DSN` reports `available: false`,
//! the UI hides the switch, and nothing is ever sent.

use std::sync::{Mutex, MutexGuard};

use serde::Serialize;

use crate::daemon;

static REPORTER: Mutex<Option<whoosh_crash::Reporter>> = Mutex::new(None);

/// Longest message and stack we accept from the web view.
const MAX_MESSAGE: usize = 2_000;
const MAX_STACK: usize = 16_000;

fn reporter() -> MutexGuard<'static, Option<whoosh_crash::Reporter>> {
    REPORTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// At startup: starts reporting when this build can and the user opted in.
pub fn init() {
    if let Ok(dir) = daemon::data_dir() {
        *reporter() = whoosh_crash::init("desktop", env!("CARGO_PKG_VERSION"), &dir);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashReports {
    /// This build has a DSN; without one the switch is hidden.
    available: bool,
    enabled: bool,
}

fn state() -> Result<CrashReports, String> {
    let dir = daemon::data_dir()?;
    Ok(CrashReports {
        available: whoosh_crash::available(),
        enabled: whoosh_crash::consent(&dir),
    })
}

#[tauri::command]
pub fn crash_reports() -> Result<CrashReports, String> {
    state()
}

/// Saves the choice. Turning it on starts the shell's reporter now (the
/// daemon follows when it next starts); turning it off stops both at once,
/// since every report checks the choice before it is sent.
#[tauri::command]
pub fn set_crash_reports(enabled: bool) -> Result<CrashReports, String> {
    let dir = daemon::data_dir()?;
    whoosh_crash::set_consent(&dir, enabled)
        .map_err(|e| format!("could not save the setting: {e}"))?;
    let mut current = reporter();
    if enabled && current.is_none() {
        *current = whoosh_crash::init("desktop", env!("CARGO_PKG_VERSION"), &dir);
    } else if !enabled {
        current.take();
    }
    drop(current);
    state()
}

fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// An uncaught error or rejected promise in the web view. A no-op unless
/// reporting runs; the message and stack are scrubbed before they leave.
#[tauri::command]
pub fn report_ui_error(kind: String, message: String, stack: String) {
    whoosh_crash::report_ui_error(&kind, clip(&message, MAX_MESSAGE), clip(&stack, MAX_STACK));
}

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn clips_on_char_boundaries() {
        assert_eq!(clip("বাংলা", 2), "বা");
        assert_eq!(clip("abc", 10), "abc");
    }
}
