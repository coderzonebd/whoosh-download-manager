// SPDX-License-Identifier: GPL-3.0-or-later
//! Tauri's build step. (The strings the shell shows itself are embedded by
//! `crates/tray`, which both the app and the tray process use.)

fn main() {
    tauri_build::build();
}
