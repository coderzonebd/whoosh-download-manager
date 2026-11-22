// SPDX-License-Identifier: GPL-3.0-or-later
//! `wdm-prompt`: the prompt window of the WDM desktop app, like IDM's
//! "Download File Info": a small native window (Slint, software rendered,
//! no web view) that asks only about what the browser handed over, one
//! thing at a time, and then shows the progress and "Download complete" of
//! a download started there. It replaces the web prompt window of
//! `wdm-desktop`, so a browser download no longer starts a web view.
//!
//! The tray starts it when a browser download waits for confirmation, when
//! the extension offers a page, and when a followed download ends; the app
//! and the native host start it too (`wdm-desktop --prompt`). One at a
//! time (`prompt.lock`): a second one ends at once, the first sees the same
//! thing from its own connection to the daemon. It ends when nothing is
//! left to show.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod daemon;
mod media;
mod model;
mod ui;
#[cfg(test)]
mod ui_tests;

mod generated {
    #![allow(missing_debug_implementations, clippy::all)]
    slint::include_modules!();
}
pub use generated::*;

use wdm_tray::locks;

fn main() {
    let Some(dir) = wdm_tray::data_dir() else {
        eprintln!("wdm-prompt: no data directory");
        std::process::exit(1);
    };
    // Another prompt window is open: it shows this too.
    let Some(lock) = locks::take(&dir, locks::PROMPT) else {
        return;
    };
    // The tray keeps the daemon running and sends the notifications.
    if !locks::held(&dir, locks::TRAY)
        && let Err(err) = wdm_tray::launch::start_tray()
    {
        eprintln!("wdm-prompt: could not start the tray: {err}");
    }
    let reporter = wdm_crash::init("prompt", env!("CARGO_PKG_VERSION"), &dir);
    let window = match PromptWindow::new() {
        Ok(w) => w,
        Err(err) => {
            eprintln!("wdm-prompt: no window: {err}");
            std::process::exit(1);
        }
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    daemon::start(dir.clone(), rx, |msg| {
        let _ = slint::invoke_from_event_loop(move || ui::with(|c| c.on_msg(msg)));
    });
    ui::Controller::install(&window, dir, tx, lock);
    if let Err(err) = slint::run_event_loop_until_quit() {
        eprintln!("wdm-prompt: {err}");
    }
    drop(window);
    drop(reporter);
}
