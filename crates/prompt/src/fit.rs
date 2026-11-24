// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window's size when it opens: the default, or the one it had
//! when it was closed. (When it shows a window, Slint also keeps it inside
//! the layout's own limits; a layout that named a maximum width by mistake
//! once made it open as small as it goes, 760 px. `ui_tests` checks that
//! no state of the window has one.)
//!
//! That is more than many screens have once the taskbar is counted:
//! 1024×768, or a 1366×768 laptop at 125 % (~1093×614 to work with), and
//! Windows put the window where it puts new windows, its right side and
//! status bar off the screen (GitHub's 1024×768 Windows runner,
//! docs/bench/windows-handcheck.ps1). On Windows it takes what the work
//! area leaves, never less than its minimum, and opens in the middle of
//! the work area. Elsewhere the window manager places and bounds windows.

/// Room left around the window inside the work area, in logical pixels.
#[cfg_attr(not(windows), allow(dead_code))]
const MARGIN: f32 = 24.0;

/// The size to open at: `want`, or what `area` leaves around `MARGIN`, but
/// never less than `min` (all logical pixels).
#[cfg_attr(not(windows), allow(dead_code))]
fn fitted(want: (f32, f32), min: (f32, f32), area: (f32, f32)) -> (f32, f32) {
    let w = want.0.min(area.0 - 2.0 * MARGIN).max(min.0);
    let h = want.1.min(area.1 - 2.0 * MARGIN).max(min.1);
    (w, h)
}

/// Sizes `window` to fit the work area and centres it there, before it is
/// shown.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn fit(window: &slint::Window, want: (f32, f32), min: (f32, f32)) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};

    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT to the pointer it is given;
    // GetDpiForSystem takes nothing and returns the system DPI.
    let (ok, dpi) = unsafe {
        (
            SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut r as *mut RECT).cast(), 0),
            GetDpiForSystem(),
        )
    };
    if ok == 0 || r.right <= r.left || r.bottom <= r.top {
        return;
    }
    let scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
    let area = (
        (r.right - r.left) as f32 / scale,
        (r.bottom - r.top) as f32 / scale,
    );
    let (w, h) = fitted(want, min, area);
    window.set_size(slint::LogicalSize::new(w, h));
    let x = r.left + (((r.right - r.left) as f32 - w * scale) / 2.0).max(0.0) as i32;
    let y = r.top + (((r.bottom - r.top) as f32 - h * scale) / 2.0).max(0.0) as i32;
    window.set_position(slint::PhysicalPosition::new(x, y));
}

#[cfg(not(windows))]
pub fn fit(window: &slint::Window, want: (f32, f32), min: (f32, f32)) {
    window.set_size(slint::LogicalSize::new(
        want.0.max(min.0),
        want.1.max(min.1),
    ));
}

#[cfg(test)]
mod tests {
    use super::fitted;

    #[test]
    fn keeps_its_size_where_it_fits_and_shrinks_where_not() {
        let want = (1120.0, 700.0);
        let min = (760.0, 440.0);
        // 1920×1040 work area: as designed.
        assert_eq!(fitted(want, min, (1920.0, 1040.0)), want);
        // 1024×728 (1024×768 less the taskbar): narrower and shorter.
        assert_eq!(fitted(want, min, (1024.0, 728.0)), (976.0, 680.0));
        // 1366×768 at 125 % less the taskbar: ~1093×590.
        assert_eq!(fitted(want, min, (1093.0, 590.0)), (1045.0, 542.0));
        // Never below the minimum the layout needs.
        assert_eq!(fitted(want, min, (700.0, 400.0)), min);
    }
}
