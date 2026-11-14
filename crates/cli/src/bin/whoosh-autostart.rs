//! `whoosh-autostart [--data-dir DIR]`: starts the whoosh daemon in the
//! background and exits. `whoosh daemon autostart on` registers it to run
//! at logon (see `whoosh_client::autostart`).
//!
//! On Windows it is a GUI-subsystem program: started from the Run key it
//! gets no console window, and the daemon it starts is detached, so nothing
//! appears on screen. It does not wait for the daemon; if one is already
//! running for the data directory, the new one sees that and exits. Errors
//! go to `<data dir>/logs/daemon-start.log`, as when `whoosh` starts the
//! daemon.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut data_dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--data-dir") => match args.next() {
                Some(dir) => data_dir = Some(PathBuf::from(dir)),
                None => return ExitCode::from(2),
            },
            _ => return ExitCode::from(2),
        }
    }
    let data_dir = match data_dir {
        Some(dir) => dir,
        None => match whoosh_proto::runtime::default_data_dir() {
            Ok(dir) => dir,
            Err(_) => return ExitCode::from(1),
        },
    };
    let exe = whoosh_client::daemon::daemon_exe();
    match whoosh_client::daemon::spawn_detached(&data_dir, &exe, &[]) {
        Ok(_child) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}
