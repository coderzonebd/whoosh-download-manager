//! Minimal logging to stderr. Stdout is the native messaging channel, so
//! nothing else may ever be printed there. Chrome copies a host's stderr to
//! its own log when started with `--enable-logging`; Firefox shows it in the
//! Browser Console.
//!
//! `WHOOSH_NMHOST_LOG=debug` enables debug lines; warnings are always shown.

use std::sync::OnceLock;

pub const LOG_ENV: &str = "WHOOSH_NMHOST_LOG";

fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(LOG_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("debug")))
}

pub fn warn(message: &str) {
    eprintln!("whoosh-nmhost: {message}");
}

pub fn debug(message: &str) {
    if debug_enabled() {
        eprintln!("whoosh-nmhost: debug: {message}");
    }
}
