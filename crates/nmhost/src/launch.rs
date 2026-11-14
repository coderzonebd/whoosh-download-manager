//! Starting the desktop app when a browser download waits for the user.
//!
//! With "Ask before downloading from the browser" on, `download.add` from
//! the extension answers `awaitingConfirmation` and, when no desktop window
//! is connected to the daemon to ask, `needsApp` (PROTOCOL.md,
//! `download.confirm`). The relay then calls [`launch_app`] (at most once
//! every [`MIN_GAP`]), which starts the app that shipped with this host:
//!
//! * **macOS**: the `.app` bundle this host is inside
//!   (`<bundle>/Contents/MacOS/wdm-nmhost`) with `open`, which also
//!   brings an app that is already running to the front; a host outside a
//!   bundle asks Launch Services for the bundle id instead (`open -b`), which
//!   fails quietly when the app is not installed.
//! * **Windows and Linux**: `wdm-desktop` next to this host (the
//!   installers, `.deb`/`.rpm`, AUR and Scoop put both in one directory).
//!   A second start of the app hands over to the running one
//!   (single-instance), which shows its window. On Windows it is started
//!   outside the browser's job object, so closing the browser does not
//!   close the app.
//!
//! Nothing is started when no app is found (CLI-only installs, or an
//! AppImage, whose host runs from a copy in the data directory): the
//! download simply waits until the app is opened. Errors are logged, never
//! sent to the browser.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The app's bundle identifier (macOS) and native messaging host name.
pub const BUNDLE_ID: &str = crate::HOST_NAME;

/// The desktop app's executable names, in the order they are looked for:
/// the Cargo binary of `apps/desktop` (what every installer ships), then
/// the product name, in case a bundler renames it.
pub const APP_EXES: [&str; 2] = if cfg!(windows) {
    ["wdm-desktop.exe", "Whoosh Download Manager.exe"]
} else {
    ["wdm-desktop", "Whoosh Download Manager"]
};

/// The relay starts the app at most this often.
pub const MIN_GAP: Duration = Duration::from_secs(30);

/// How to start the app from a host at `host_exe`, or `None` when there is
/// nothing to start. `exists` decides whether a candidate file is there
/// (a parameter so tests need no real files).
pub fn plan(host_exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<(PathBuf, Vec<String>)> {
    if cfg!(target_os = "macos") {
        let bundle = host_exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .filter(|b| host_exe.starts_with(b.join("Contents").join("MacOS")));
        let args = match bundle {
            Some(b) => vec![b.to_string_lossy().into_owned()],
            None => vec!["-b".into(), BUNDLE_ID.into()],
        };
        return Some((PathBuf::from("/usr/bin/open"), args));
    }
    let dir = host_exe.parent()?;
    APP_EXES
        .iter()
        .map(|name| dir.join(name))
        .find(|app| exists(app))
        .map(|app| (app, Vec::new()))
}

/// Starts the desktop app (see the module docs). Never blocks for long and
/// never lets the child touch this host's stdin/stdout (the browser pipe).
pub fn launch_app() {
    let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(exe) => exe,
        Err(e) => {
            crate::log::warn(&format!(
                "cannot find this executable to start the app: {e}"
            ));
            return;
        }
    };
    let Some((program, args)) = plan(&exe, Path::is_file) else {
        crate::log::debug("no desktop app next to the host; the download waits for it");
        return;
    };
    let spawn = |detach: bool| {
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_from_browser(&mut cmd, detach);
        cmd.spawn()
    };
    // Breaking away from the browser's job can be refused; start it inside
    // the job then (it lives as long as the browser).
    let child = spawn(true).or_else(|_| spawn(false));
    match child {
        Ok(mut child) => {
            crate::log::debug(&format!("started {}", program.display()));
            // Reap it (`open` exits at once; the app outlives this thread
            // only on Windows and Linux, where nothing waits for it).
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => crate::log::warn(&format!("cannot start {}: {e}", program.display())),
    }
}

#[cfg(windows)]
fn detach_from_browser(cmd: &mut Command, breakaway: bool) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    if breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    cmd.creation_flags(flags);
}

#[cfg(unix)]
fn detach_from_browser(cmd: &mut Command, _breakaway: bool) {
    use std::os::unix::process::CommandExt;
    // Its own process group: a browser that kills the host's group on exit
    // leaves the app alone.
    cmd.process_group(0);
}

#[cfg(not(any(unix, windows)))]
fn detach_from_browser(_cmd: &mut Command, _breakaway: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_opens_the_bundle_the_host_is_in() {
        let host = Path::new("/Applications/Whoosh Download Manager.app/Contents/MacOS/wdm-nmhost");
        let (program, args) = plan(host, |_| false).unwrap();
        assert_eq!(program, Path::new("/usr/bin/open"));
        assert_eq!(args, ["/Applications/Whoosh Download Manager.app"]);
        let (_, args) = plan(Path::new("/opt/homebrew/bin/wdm-nmhost"), |_| false).unwrap();
        assert_eq!(args, ["-b", BUNDLE_ID]);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn starts_the_app_next_to_the_host_only_when_it_is_there() {
        let host = Path::new("/usr/bin/wdm-nmhost");
        let app = Path::new("/usr/bin").join(APP_EXES[0]);
        let (program, args) = plan(host, |p| p == app).unwrap();
        assert_eq!(program, app);
        assert!(args.is_empty());
        assert_eq!(plan(host, |_| false), None);
    }
}
