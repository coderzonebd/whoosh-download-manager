// SPDX-License-Identifier: GPL-3.0-or-later
//! "Start Whoosh Download Manager when I log in", from the native Settings.
//!
//! The same login item the app's `tauri-plugin-autostart` writes (through
//! the same `auto-launch` crate): named after the product, starting
//! `wdm-desktop --autostart` (from an AppImage, the image itself), which
//! becomes the tray. So turning it on or off here and in the web Settings
//! is one switch.

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

const APP_NAME: &str = "Whoosh Download Manager";

fn item() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let app = match std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        Some(image) => std::path::PathBuf::from(image),
        None => wdm_tray::launch::app_exe_next_to(&exe, std::path::Path::is_file)?,
    };
    #[cfg(target_os = "macos")]
    let app = app.canonicalize().unwrap_or(app);
    let mut b = AutoLaunchBuilder::new();
    b.set_app_name(APP_NAME)
        .set_app_path(&app.display().to_string())
        .set_args(&[wdm_tray::AUTOSTART_ARG]);
    #[cfg(target_os = "macos")]
    b.set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent);
    b.build().ok()
}

/// Whether it is on; `None` when this copy cannot have one (no app next to it).
pub fn enabled() -> Option<bool> {
    item()?.is_enabled().ok()
}

pub fn set(on: bool) -> Result<(), String> {
    let item = item().ok_or("the app is not next to this program")?;
    let r = if on { item.enable() } else { item.disable() };
    r.map_err(|e| e.to_string())
}
