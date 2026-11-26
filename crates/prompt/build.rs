// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    // Debug builds keep what the UI tests need to find elements by their
    // accessible name and click them (src/ui_tests.rs, "by hand"); release
    // builds do not carry it.
    let debug = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).expect("ui/app.slint");
}
