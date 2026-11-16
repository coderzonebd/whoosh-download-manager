// SPDX-License-Identifier: GPL-3.0-or-later
// A desktop run of the mobile shell, for development (`tauri dev`); the
// Android app enters through `run` in lib.rs.
fn main() {
    wdm_mobile_lib::run();
}
