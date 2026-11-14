// SPDX-License-Identifier: GPL-3.0-or-later
// No console window behind the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wdm_desktop_lib::run();
}
