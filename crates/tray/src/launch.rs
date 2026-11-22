// SPDX-License-Identifier: GPL-3.0-or-later
//! How each process starts the other.
//!
//! * The installers put `wdm-tray` next to `wdm-desktop` (Tauri's
//!   `externalBin`), so each finds the other in its own directory.
//! * **macOS**: both are in `<bundle>/Contents/MacOS`. The app is started
//!   through Launch Services (`open <bundle> --args …`), so it is the app
//!   the system knows (Dock, menu bar, ⌘Tab); a running app is brought to
//!   the front instead, without the arguments.
//! * **AppImage**: its files exist only while a process started from it
//!   runs, so each is started from the image itself (`$APPIMAGE`); the tray
//!   as `$APPIMAGE --tray`, which `wdm-desktop` turns into the tray process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const APP_EXES: [&str; 2] = if cfg!(windows) {
    ["wdm-desktop.exe", "Whoosh Download Manager.exe"]
} else {
    ["wdm-desktop", "Whoosh Download Manager"]
};
const TRAY_EXE: &str = if cfg!(windows) {
    "wdm-tray.exe"
} else {
    "wdm-tray"
};

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// The `.app` bundle `exe` is inside (`<bundle>/Contents/MacOS/<exe>`).
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .filter(|b| exe.starts_with(b.join("Contents").join("MacOS")))
}

/// How to start the app with `args`, from a process at `exe`. `appimage`
/// is `$APPIMAGE`; `exists` tells whether a file is there (a parameter so
/// tests need no real files).
pub fn app_plan(
    exe: &Path,
    appimage: Option<&Path>,
    args: &[&str],
    exists: impl Fn(&Path) -> bool,
) -> Option<Plan> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    if let Some(image) = appimage {
        return Some(Plan {
            program: image.to_owned(),
            args,
        });
    }
    if cfg!(target_os = "macos")
        && let Some(bundle) = bundle_of(exe)
    {
        let mut open = vec![bundle.to_string_lossy().into_owned()];
        if !args.is_empty() {
            open.push("--args".into());
            open.extend(args);
        }
        return Some(Plan {
            program: PathBuf::from("/usr/bin/open"),
            args: open,
        });
    }
    let dir = exe.parent()?;
    APP_EXES
        .iter()
        .map(|name| dir.join(name))
        .find(|p| exists(p))
        .map(|program| Plan { program, args })
}

/// How to start the tray process, from the app at `exe`.
pub fn tray_plan(
    exe: &Path,
    appimage: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Option<Plan> {
    if let Some(image) = appimage {
        return Some(Plan {
            program: image.to_owned(),
            args: vec![crate::TRAY_ARG.into()],
        });
    }
    let program = exe.parent()?.join(TRAY_EXE);
    exists(&program).then(|| Plan {
        program,
        args: Vec::new(),
    })
}

/// The tray next to the app at `exe` (for `wdm-desktop --tray`).
pub fn tray_exe_next_to(exe: &Path) -> PathBuf {
    exe.with_file_name(TRAY_EXE)
}

fn this_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Starts `plan` on its own: no inherited stdin/stdout, reaped on a thread.
pub fn spawn(plan: &Plan) -> std::io::Result<()> {
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Starts the app with `args` (from the tray).
pub fn start_app(args: &[&str]) -> std::io::Result<()> {
    let exe = this_exe().ok_or_else(|| std::io::Error::other("no current executable"))?;
    let plan = app_plan(&exe, appimage().as_deref(), args, Path::is_file)
        .ok_or_else(|| std::io::Error::other("the app is not next to the tray"))?;
    spawn(&plan)
}

/// Starts the tray process (from the app).
pub fn start_tray() -> std::io::Result<()> {
    let exe = this_exe().ok_or_else(|| std::io::Error::other("no current executable"))?;
    let plan = tray_plan(&exe, appimage().as_deref(), Path::is_file)
        .ok_or_else(|| std::io::Error::other("wdm-tray is not next to the app"))?;
    spawn(&plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn each_finds_the_other_next_to_it() {
        if cfg!(any(windows, target_os = "macos")) {
            return;
        }
        let all = |_: &Path| true;
        assert_eq!(
            app_plan(&p("/usr/bin/wdm-tray"), None, &["--prompt"], all),
            Some(Plan {
                program: p("/usr/bin/wdm-desktop"),
                args: vec!["--prompt".into()]
            })
        );
        assert_eq!(
            tray_plan(&p("/usr/bin/wdm-desktop"), None, all),
            Some(Plan {
                program: p("/usr/bin/wdm-tray"),
                args: vec![]
            })
        );
        // A bundler that named the app after the product.
        let product = |f: &Path| f.ends_with("Whoosh Download Manager");
        assert_eq!(
            app_plan(&p("/opt/wdm/wdm-tray"), None, &[], product).map(|x| x.program),
            Some(p("/opt/wdm/Whoosh Download Manager"))
        );
        // Nothing there: nothing to start.
        let none = |_: &Path| false;
        assert_eq!(app_plan(&p("/usr/bin/wdm-tray"), None, &[], none), None);
        assert_eq!(tray_plan(&p("/usr/bin/wdm-desktop"), None, none), None);
    }

    #[test]
    fn an_appimage_starts_itself() {
        let image = p("/home/a/Apps/Whoosh.AppImage");
        let none = |_: &Path| false;
        assert_eq!(
            app_plan(
                &p("/tmp/.mount_x/usr/bin/wdm-tray"),
                Some(&image),
                &["--prompt"],
                none
            ),
            Some(Plan {
                program: image.clone(),
                args: vec!["--prompt".into()]
            })
        );
        assert_eq!(
            tray_plan(&p("/tmp/.mount_x/usr/bin/wdm-desktop"), Some(&image), none),
            Some(Plan {
                program: image,
                args: vec!["--tray".into()]
            })
        );
    }

    #[test]
    fn the_bundle_holds_both() {
        let exe = p("/Applications/Whoosh Download Manager.app/Contents/MacOS/wdm-tray");
        assert_eq!(
            bundle_of(&exe),
            Some(Path::new("/Applications/Whoosh Download Manager.app"))
        );
        assert_eq!(bundle_of(&p("/usr/bin/wdm-tray")), None);
    }
}
