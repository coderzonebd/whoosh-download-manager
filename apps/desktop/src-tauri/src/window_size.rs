// SPDX-License-Identifier: GPL-3.0-or-later
//! Window size persistence that is immune to client-side decorations.
//!
//! On GTK/Wayland with client-side decorations, `Window::inner_size()`
//! includes the header bar and shadow while `set_size()` sets the content
//! area, so saving one and restoring the other grows the window on every
//! launch (+40×77 px measured here). The webview's own size *is* the content
//! area on every platform, so that is what we save, in logical pixels.
//! `tauri-plugin-window-state` keeps handling position and maximised state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, LogicalSize, Manager, Runtime, Webview, WebviewWindow};

const FILE: &str = "window-size.json";
/// Ignore saved sizes smaller than this (corrupt file, or a window that was
/// shrunk to nothing on a since-removed monitor).
const MIN: (f64, f64) = (480.0, 320.0);

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Saved {
    width: f64,
    height: f64,
}

fn path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(FILE))
}

fn sane(s: &Saved) -> bool {
    s.width.is_finite() && s.height.is_finite() && s.width >= MIN.0 && s.height >= MIN.1
}

/// Apply the saved content size, if there is a sane one.
pub fn restore<R: Runtime>(window: &WebviewWindow<R>) {
    let Some(file) = path(window.app_handle()) else {
        return;
    };
    let Ok(bytes) = std::fs::read(file) else {
        return;
    };
    match serde_json::from_slice::<Saved>(&bytes) {
        Ok(saved) if sane(&saved) => {
            if let Err(err) = window.set_size(LogicalSize::new(saved.width, saved.height)) {
                eprintln!("vayu-desktop: could not restore window size: {err}");
            }
        }
        _ => {}
    }
}

/// Remember the content size (not while maximised, minimised or fullscreen).
pub fn save<R: Runtime>(window: &WebviewWindow<R>) {
    let special = window.is_maximized().unwrap_or(false)
        || window.is_minimized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false);
    if special {
        return;
    }
    let webview: &Webview<R> = window.as_ref();
    let (Ok(size), Ok(scale)) = (webview.size(), window.scale_factor()) else {
        return;
    };
    let logical = size.to_logical::<f64>(scale);
    let saved = Saved {
        width: logical.width.round(),
        height: logical.height.round(),
    };
    if !sane(&saved) {
        return;
    }
    let Some(file) = path(window.app_handle()) else {
        return;
    };
    let result = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| serde_json::to_vec(&saved).map_err(std::io::Error::other))
        .and_then(|json| std::fs::write(&file, json));
    if let Err(err) = result {
        eprintln!("vayu-desktop: could not save window size: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_tiny_or_broken_sizes() {
        assert!(sane(&Saved {
            width: 1240.0,
            height: 780.0
        }));
        assert!(!sane(&Saved {
            width: 10.0,
            height: 780.0
        }));
        assert!(!sane(&Saved {
            width: f64::NAN,
            height: 780.0
        }));
    }

    #[test]
    fn round_trips_as_json() {
        let s = Saved {
            width: 980.0,
            height: 640.0,
        };
        let json = serde_json::to_string(&s).expect("serialise");
        assert_eq!(json, r#"{"width":980.0,"height":640.0}"#);
        assert_eq!(serde_json::from_str::<Saved>(&json).expect("parse"), s);
    }
}
