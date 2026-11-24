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
mod fit;
mod login;
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

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();
    let Some(dir) = wdm_tray::data_dir() else {
        eprintln!("wdm-prompt: no data directory");
        std::process::exit(1);
    };
    let args: Vec<String> = std::env::args().collect();
    let main_window = args.iter().any(|a| a == wdm_tray::MAIN_ARG);
    // `--page browsers`: the page to open the main window on.
    let page = args
        .iter()
        .position(|a| a == wdm_tray::PAGE_ARG)
        .and_then(|i| args.get(i + 1))
        .filter(|p| ui::Page::parse(p).is_some())
        .cloned()
        .unwrap_or_default();
    // Another one runs: it shows the prompt itself, and the main window
    // (on that page) when asked (`ui::SHOW_MAIN_FILE`).
    let Some(lock) = locks::take(&dir, locks::PROMPT) else {
        if main_window {
            let _ = std::fs::write(dir.join(ui::SHOW_MAIN_FILE), page.as_bytes());
        }
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
    if main_window {
        ui::with(|c| {
            c.show_main();
            if let Some(page) = ui::Page::parse(&page) {
                c.open_page(page);
            }
        });
    }
    // Profiling: `DHAT_SECONDS=20` closes the program after that long, so
    // the profile is written.
    #[cfg(feature = "dhat-heap")]
    let _quit = std::env::var("DHAT_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|secs| {
            let t = slint::Timer::default();
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_secs(secs),
                || {
                    let _ = slint::quit_event_loop();
                },
            );
            t
        });
    if let Err(err) = slint::run_event_loop_until_quit() {
        eprintln!("wdm-prompt: {err}");
    }
    drop(window);
    drop(reporter);
}
