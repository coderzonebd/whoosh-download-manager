//! Programs started from inside an AppImage must not inherit it. The AppImage's start script
//! points the dynamic loader and friends at the libraries bundled in the image
//! (`LD_LIBRARY_PATH`, `XDG_DATA_DIRS`, `GIO_MODULE_DIR`, `PATH`…); a system program started
//! with that environment — `xdg-open`, the file manager, `cut` in a shell script, yt-dlp —
//! loads the image's `libsystemd` or GLib instead of its own and fails
//! (`version LIBSYSTEMD_254 not found`).
//!
//! [`child_env`] says what to change for such a program: in every variable that names a path
//! inside the image (`$APPDIR`), those entries go; a variable left empty is removed. Nothing
//! changes outside an AppImage.

use std::ffi::{OsStr, OsString};

/// For a program started from this process: `(name, Some(value))` to set, `(name, None)` to
/// remove. Empty outside an AppImage.
pub fn child_env() -> Vec<(OsString, Option<OsString>)> {
    match (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR")) {
        (Some(_), Some(appdir)) => child_env_from(&appdir, std::env::vars_os()),
        _ => Vec::new(),
    }
}

fn child_env_from(
    appdir: &OsStr,
    vars: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, Option<OsString>)> {
    let Some(appdir) = appdir.to_str().map(|d| d.trim_end_matches('/')) else {
        return Vec::new();
    };
    if appdir.is_empty() || appdir == "/" {
        return Vec::new();
    }
    let inside = |entry: &str| entry == appdir || entry.starts_with(&format!("{appdir}/"));
    let mut out = Vec::new();
    for (name, value) in vars {
        // The AppImage's own variables say where the image is; they harm nothing.
        if matches!(name.to_str(), Some("APPDIR" | "APPIMAGE" | "ARGV0" | "OWD")) {
            continue;
        }
        let Some(v) = value.to_str() else { continue };
        if !v.contains(appdir) {
            continue;
        }
        let kept: Vec<&str> = v
            .split(':')
            .filter(|e| !e.is_empty() && !inside(e))
            .collect();
        out.push((
            name,
            (!kept.is_empty()).then(|| OsString::from(kept.join(":"))),
        ));
    }
    out
}

/// Applies [`child_env`] to a command (std or tokio, through its `env`/`env_remove`).
#[macro_export]
macro_rules! outside_appimage {
    ($cmd:expr) => {
        for (name, value) in $crate::appimage::child_env() {
            match value {
                Some(v) => {
                    $cmd.env(name, v);
                }
                None => {
                    $cmd.env_remove(name);
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(list: &[(&str, &str)]) -> impl Iterator<Item = (OsString, OsString)> {
        list.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_images_paths_are_dropped_from_a_childs_environment() {
        let app = "/tmp/.mount_WhooshAb12";
        let got = child_env_from(
            OsStr::new(app),
            vars(&[
                (
                    "LD_LIBRARY_PATH",
                    "/tmp/.mount_WhooshAb12/usr/lib:/tmp/.mount_WhooshAb12/usr/lib/x86_64-linux-gnu",
                ),
                (
                    "PATH",
                    "/tmp/.mount_WhooshAb12/usr/bin:/usr/local/bin:/usr/bin",
                ),
                (
                    "XDG_DATA_DIRS",
                    "/tmp/.mount_WhooshAb12/usr/share:/usr/share",
                ),
                ("HOME", "/home/a"),
                ("APPDIR", app),
                ("OTHER", "/tmp/.mount_WhooshAb12x/not-ours"),
            ]),
        );
        assert_eq!(
            got,
            vec![
                (OsString::from("LD_LIBRARY_PATH"), None),
                (
                    OsString::from("PATH"),
                    Some(OsString::from("/usr/local/bin:/usr/bin"))
                ),
                (
                    OsString::from("XDG_DATA_DIRS"),
                    Some(OsString::from("/usr/share"))
                ),
                // A similar prefix is not the image.
                (
                    OsString::from("OTHER"),
                    Some(OsString::from("/tmp/.mount_WhooshAb12x/not-ours"))
                ),
            ]
        );
    }

    #[test]
    fn nothing_changes_outside_an_appimage() {
        assert!(child_env_from(OsStr::new("/"), vars(&[("PATH", "/usr/bin")])).is_empty());
    }
}
