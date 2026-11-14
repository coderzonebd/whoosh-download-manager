// SPDX-License-Identifier: GPL-3.0-or-later
//! The prompt window: a small window, apart from the main one, that asks
//! about what the browser hands over, like IDM's "Download File Info". It
//! shows, one at a time, the browser downloads that wait for confirmation
//! (PROTOCOL.md `download.confirm`) and the video pages offered by the
//! extension's "Download with WDM" button (`media.offer`, with the add
//! dialog's format picker), and closes itself when nothing is left. The main
//! window never comes up for them.
//!
//! The shell owns the single `prompt` window: opening it again only brings
//! it to the front, whichever UI asked. A UI asks with [`open_prompt`]; when
//! no app is running, `wdm-nmhost` starts it with [`PROMPT_ARG`], and the
//! app then stays in the tray and opens only this window. A running app
//! gets the argument from a second launch through the single-instance
//! plugin.

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

/// Start (or, through single-instance, ask the running app) to open the
/// prompt window and keep the main window hidden. Must match
/// `wdm_nmhost::launch::PROMPT_ARG`.
pub const PROMPT_ARG: &str = "--prompt";

/// The prompt window's label (`capabilities/prompt.json`).
pub const PROMPT_WINDOW: &str = "prompt";

/// What the frontend loads; `main.ts` mounts the prompt view for it.
const PROMPT_URL: &str = "index.html?view=prompt";

const WIDTH: f64 = 520.0;
const HEIGHT: f64 = 440.0;

/// Whether a command line asks for the prompt window.
pub fn wants_prompt<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|a| a.as_ref() == PROMPT_ARG)
}

/// Opens the prompt window, or brings it to the front when it is open.
pub fn open<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(PROMPT_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    // The activation policy is left alone (macOS): an app that lives in the
    // menu bar does not get a Dock icon just for this window.
    let window = WebviewWindowBuilder::new(app, PROMPT_WINDOW, WebviewUrl::App(PROMPT_URL.into()))
        .title("Whoosh Download Manager")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .maximizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .center()
        .focused(true)
        // Also says which view to mount, should the query string be lost.
        .initialization_script("window.__WDM_VIEW__ = \"prompt\";")
        .build()?;
    let _ = window.set_focus();
    Ok(())
}

/// A UI has something to ask (a waiting download, an offered page).
/// Async: creating a window from a synchronous command can deadlock on
/// Windows.
#[tauri::command]
pub async fn open_prompt<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    open(&app).map_err(|e| e.to_string())
}

/// The prompt window has nothing left to ask.
#[tauri::command]
pub async fn close_prompt<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    match app.get_webview_window(PROMPT_WINDOW) {
        Some(window) => window.close().map_err(|e| e.to_string()),
        None => Ok(()),
    }
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
