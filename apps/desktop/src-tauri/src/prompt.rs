// SPDX-License-Identifier: GPL-3.0-or-later
//! The prompt window: a small window, apart from the main one, that asks
//! about what the browser hands over, like IDM's "Download File Info". It
//! is `wdm-prompt` (`crates/prompt`), a native window with no web view, so
//! a browser download never starts a web page: this app only starts it.
//!
//! The main window asks for it with [`open_prompt`]; when no app is
//! running, `wdm-nmhost` starts `wdm-desktop --prompt` ([`PROMPT_ARG`]),
//! which becomes `wdm-prompt` at once (`become_prompt` in lib.rs).

/// Must match `wdm_nmhost::launch::PROMPT_ARG`.
pub const PROMPT_ARG: &str = "--prompt";

/// Whether a command line asks for the prompt window.
pub fn wants_prompt<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|a| a.as_ref() == PROMPT_ARG)
}

/// A UI has something to ask (a waiting download, an offered page): start
/// the prompt window (one already open shows it itself).
#[tauri::command]
pub async fn open_prompt() -> Result<(), String> {
    wdm_tray::launch::start_prompt().map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_argument_is_found_anywhere_on_the_command_line() {
        assert!(wants_prompt([
            "/Applications/x.app/Contents/MacOS/wdm-desktop",
            "--prompt"
        ]));
        assert!(wants_prompt(vec![
            "wdm-desktop".to_owned(),
            "--autostart".to_owned(),
            "--prompt".to_owned()
        ]));
        assert!(!wants_prompt(["wdm-desktop"]));
        assert!(!wants_prompt([
            "wdm-desktop",
            "--autostart",
            "whoosh://add?url=--prompt"
        ]));
        assert!(!wants_prompt(Vec::<String>::new()));
    }

    #[test]
    fn the_argument_is_the_one_the_native_host_passes() {
        assert_eq!(PROMPT_ARG, wdm_nmhost::launch::PROMPT_ARG);
    }
}
