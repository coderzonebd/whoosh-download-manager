// SPDX-License-Identifier: GPL-3.0-or-later
//! `wdm-tray`: the tray icon of the WDM desktop app (see the library docs
//! for how it splits the work with `wdm-desktop`).
//!
//! One per user (`tray.lock`). It starts the daemon and keeps it running,
//! shows the tray icon (Show / Quit), and from its own connection to the
//! daemon keeps the tooltip up to date and, while no window of the app is
//! open, sends the notifications and opens the prompt window. Quit stops
//! the daemon and everything with it.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;

fn main() {
    let Some(dir) = wdm_tray::data_dir() else {
        eprintln!("wdm-tray: no data directory");
        std::process::exit(1);
    };
    // Another tray is already there: nothing to do.
    let Some(_lock) = wdm_tray::locks::take(&dir, wdm_tray::locks::TRAY) else {
        return;
    };
    app::run(dir);
}
