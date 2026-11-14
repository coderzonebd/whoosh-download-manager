//! Where the daemon publishes its port and token (PROTOCOL.md §1), shared by
//! the daemon, the CLI and `wdm-nmhost` so the three always agree.
//!
//! [`read_runtime`] refuses a file that another user could have written: a
//! symlink, a file not owned by the current user or readable by anyone else,
//! or one in a directory that others can write to. Otherwise a local attacker
//! could point clients at a port of their choosing. On Unix that is the
//! owner and the mode bits; on Windows the owner and the DACL, with the same
//! rules (see [`crate::winacl`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Overrides the data directory. Meant for tests, portable installs and
/// running a second daemon side by side.
pub const DATA_DIR_ENV: &str = "WDM_DATA_DIR";

/// The runtime file's name inside the data directory.
pub const RUNTIME_FILE: &str = "runtime.json";

/// The contents of `runtime.json`. `token` is a secret: `Debug` redacts it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
}

impl std::fmt::Debug for RuntimeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeInfo")
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("pid", &self.pid)
            .field("version", &self.version)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("no runtime file at {0} (is the daemon running?)")]
    Missing(PathBuf),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a valid runtime file: {source}")]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The file or its directory could have been written by someone else.
    #[error("refusing {path}: {reason}")]
    Insecure { path: PathBuf, reason: String },
    #[error("cannot determine the data directory: {0} is not set")]
    NoBaseDir(&'static str),
}

/// The environment variables that decide the data directory, captured once so
/// the resolution itself is a pure function (and testable for every OS).
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub wdm_data_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            wdm_data_dir: var(DATA_DIR_ENV),
            home: var("HOME").or_else(|| var("USERPROFILE")),
            xdg_data_home: var("XDG_DATA_HOME").filter(|p| p.is_absolute()),
            xdg_config_home: var("XDG_CONFIG_HOME").filter(|p| p.is_absolute()),
            local_app_data: var("LOCALAPPDATA"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            // Linux and the BSDs share the XDG layout.
            Os::Linux
        }
    }
}

/// The data directory's name under the platform's base directory.
const DIR_NAME: &str = "wdm";

/// Its name before the internal names were renamed to `wdm` (2026-11-14).
/// [`default_data_dir`] moves a directory of that name to [`DIR_NAME`].
pub const LEGACY_DIR_NAME: &str = "whoosh";

/// The daemon's data directory:
/// Linux `$XDG_DATA_HOME/wdm` (default `~/.local/share/wdm`),
/// macOS `~/Library/Application Support/wdm`, Windows `%LOCALAPPDATA%\wdm`.
/// `WDM_DATA_DIR` overrides it everywhere.
pub fn data_dir(os: Os, env: &Env) -> Result<PathBuf, RuntimeError> {
    if let Some(dir) = &env.wdm_data_dir {
        return Ok(dir.clone());
    }
    named_data_dir(os, env, DIR_NAME)
}

/// Where [`data_dir`] was before the rename: the same base directory with
/// [`LEGACY_DIR_NAME`]. `None` when `WDM_DATA_DIR` chooses the directory.
pub fn legacy_data_dir(os: Os, env: &Env) -> Option<PathBuf> {
    if env.wdm_data_dir.is_some() {
        return None;
    }
    named_data_dir(os, env, LEGACY_DIR_NAME).ok()
}

fn named_data_dir(os: Os, env: &Env, name: &str) -> Result<PathBuf, RuntimeError> {
    let home = || env.home.clone().ok_or(RuntimeError::NoBaseDir("HOME"));
    Ok(match os {
        Os::Linux => match &env.xdg_data_home {
            Some(xdg) => xdg.join(name),
            None => home()?.join(".local/share").join(name),
        },
        Os::MacOs => home()?.join("Library/Application Support").join(name),
        Os::Windows => env
            .local_app_data
            .clone()
            .ok_or(RuntimeError::NoBaseDir("LOCALAPPDATA"))?
            .join(name),
    })
}

/// [`data_dir`] for this process's OS and environment, after moving a data
/// directory left under the old name there ([`migrate_data_dir`]). The
/// daemon, the CLI, the native host and the desktop app all resolve it this
/// way, so whichever runs first after an update moves it, and all of them
/// agree on the result.
pub fn default_data_dir() -> Result<PathBuf, RuntimeError> {
    let (os, env) = (Os::current(), Env::from_process());
    let dir = data_dir(os, &env)?;
    Ok(match legacy_data_dir(os, &env) {
        Some(legacy) => migrate_data_dir(&dir, &legacy),
        None => dir,
    })
}

/// One-time move of the data directory from its old name: when `legacy`
/// exists and `dir` does not, renames `legacy` to `dir` (same parent, so a
/// plain rename: downloads, history, settings, journal and token move as
/// they are). Returns the directory to use: `dir`, or `legacy` when the
/// rename failed and `legacy` is still there (on Windows, a daemon of the
/// old version still running holds files open in it; the next start tries
/// again). Never copies and never deletes.
pub fn migrate_data_dir(dir: &Path, legacy: &Path) -> PathBuf {
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    if exists(dir) || !exists(legacy) {
        return dir.to_path_buf();
    }
    match std::fs::rename(legacy, dir) {
        Ok(()) => dir.to_path_buf(),
        // Another process may have moved it at the same moment.
        Err(_) if exists(dir) => dir.to_path_buf(),
        Err(_) if exists(legacy) => legacy.to_path_buf(),
        Err(_) => dir.to_path_buf(),
    }
}

pub fn runtime_file(os: Os, env: &Env) -> Result<PathBuf, RuntimeError> {
    Ok(data_dir(os, env)?.join(RUNTIME_FILE))
}

/// Reads the runtime file. Read it on every connection attempt: the daemon may
/// have restarted on another port with a new token. A file that passes these
/// checks may still be stale (left by a crash); [`crate::auth`] catches that.
pub fn read_runtime(path: &Path) -> Result<RuntimeInfo, RuntimeError> {
    check_runtime_file(path)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RuntimeError::Missing(path.to_owned()));
        }
        Err(source) => {
            return Err(RuntimeError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes).map_err(|source| RuntimeError::Invalid {
        path: path.to_owned(),
        source,
    })
}

/// Checks that `dir` is a directory owned by the current user that no one
/// else can write to. The daemon calls this on its data directory at start,
/// after tightening a directory it owns.
pub fn check_private_dir(dir: &Path) -> Result<(), RuntimeError> {
    imp::check_dir(dir)
}

/// Checks that `path` is a regular file owned by the current user that no
/// one else can read or write: what [`read_runtime`] demands of
/// `runtime.json` itself.
pub fn check_private_file(path: &Path) -> Result<(), RuntimeError> {
    imp::check_file(path)
}

fn check_runtime_file(path: &Path) -> Result<(), RuntimeError> {
    imp::check_file(path)?;
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => imp::check_dir(dir),
        _ => Ok(()),
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use super::RuntimeError;

    fn insecure(path: &Path, reason: impl Into<String>) -> RuntimeError {
        RuntimeError::Insecure {
            path: path.to_owned(),
            reason: reason.into(),
        }
    }

    fn metadata(path: &Path) -> Result<std::fs::Metadata, RuntimeError> {
        std::fs::symlink_metadata(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                RuntimeError::Missing(path.to_owned())
            } else {
                RuntimeError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })
    }

    fn check_owner(path: &Path, meta: &std::fs::Metadata) -> Result<(), RuntimeError> {
        let me = rustix::process::geteuid().as_raw();
        if meta.uid() != me {
            return Err(insecure(
                path,
                format!("owned by uid {}, not by this user (uid {me})", meta.uid()),
            ));
        }
        Ok(())
    }

    pub(super) fn check_file(path: &Path) -> Result<(), RuntimeError> {
        let meta = metadata(path)?;
        if !meta.file_type().is_file() {
            return Err(insecure(path, "not a regular file"));
        }
        check_owner(path, &meta)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(insecure(
                path,
                format!("mode {mode:o} lets other users read or write it (want 600)"),
            ));
        }
        Ok(())
    }

    pub(super) fn check_dir(dir: &Path) -> Result<(), RuntimeError> {
        let meta = metadata(dir)?;
        if !meta.file_type().is_dir() {
            return Err(insecure(dir, "not a directory"));
        }
        check_owner(dir, &meta)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o022 != 0 {
            return Err(insecure(
                dir,
                format!("mode {mode:o} lets other users write to it (want 700)"),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use super::RuntimeError;
    use crate::winacl::{self, CheckError, Kind};

    fn check(path: &Path, kind: Kind) -> Result<(), RuntimeError> {
        winacl::check_private(path, kind).map_err(|e| match e {
            CheckError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
                RuntimeError::Missing(path.to_owned())
            }
            CheckError::Io(source) => RuntimeError::Io {
                path: path.to_owned(),
                source,
            },
            CheckError::Insecure(reason) => RuntimeError::Insecure {
                path: path.to_owned(),
                reason,
            },
        })
    }

    pub(super) fn check_file(path: &Path) -> Result<(), RuntimeError> {
        check(path, Kind::File)
    }

    pub(super) fn check_dir(dir: &Path) -> Result<(), RuntimeError> {
        check(dir, Kind::Dir)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use super::RuntimeError;

    pub(super) fn check_file(_path: &Path) -> Result<(), RuntimeError> {
        Ok(())
    }

    pub(super) fn check_dir(_dir: &Path) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            home: Some("/home/u".into()),
            local_app_data: Some(r"C:\Users\u\AppData\Local".into()),
            ..Env::default()
        }
    }

    #[test]
    fn data_dir_per_os() {
        assert_eq!(
            data_dir(Os::Linux, &env()).unwrap(),
            PathBuf::from("/home/u/.local/share/wdm")
        );
        assert_eq!(
            data_dir(Os::MacOs, &env()).unwrap(),
            PathBuf::from("/home/u/Library/Application Support/wdm")
        );
        assert_eq!(
            data_dir(Os::Windows, &env()).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Local").join("wdm")
        );
    }

    #[test]
    fn xdg_data_home_wins_on_linux() {
        let env = Env {
            xdg_data_home: Some("/data".into()),
            ..env()
        };
        assert_eq!(
            data_dir(Os::Linux, &env).unwrap(),
            PathBuf::from("/data/wdm")
        );
    }

    #[test]
    fn override_wins_everywhere() {
        let env = Env {
            wdm_data_dir: Some("/portable".into()),
            ..env()
        };
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert_eq!(data_dir(os, &env).unwrap(), PathBuf::from("/portable"));
        }
    }

    #[test]
    fn legacy_data_dir_is_the_old_name_in_the_same_place() {
        assert_eq!(
            legacy_data_dir(Os::Linux, &env()),
            Some(PathBuf::from("/home/u/.local/share/whoosh"))
        );
        assert_eq!(
            legacy_data_dir(Os::MacOs, &env()),
            Some(PathBuf::from("/home/u/Library/Application Support/whoosh"))
        );
        assert_eq!(
            legacy_data_dir(Os::Windows, &env()),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local").join("whoosh"))
        );
        let xdg = Env {
            xdg_data_home: Some("/data".into()),
            ..env()
        };
        assert_eq!(
            legacy_data_dir(Os::Linux, &xdg),
            Some(PathBuf::from("/data/whoosh"))
        );
        // An explicit directory is never migrated.
        let portable = Env {
            wdm_data_dir: Some("/portable".into()),
            ..env()
        };
        assert_eq!(legacy_data_dir(Os::Linux, &portable), None);
    }

    mod migrate {
        use super::*;

        struct Base(PathBuf);
        impl Base {
            fn new(tag: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "wdm-proto-migrate-{tag}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }
        }
        impl Drop for Base {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn moves_the_old_directory_with_everything_in_it() {
            let base = Base::new("move");
            let (dir, legacy) = (base.0.join("wdm"), base.0.join("whoosh"));
            std::fs::create_dir_all(legacy.join("journal")).unwrap();
            std::fs::write(legacy.join("state.db"), b"db").unwrap();
            std::fs::write(legacy.join("journal/1"), b"j").unwrap();

            assert_eq!(migrate_data_dir(&dir, &legacy), dir);
            assert!(!legacy.exists());
            assert_eq!(std::fs::read(dir.join("state.db")).unwrap(), b"db");
            assert_eq!(std::fs::read(dir.join("journal/1")).unwrap(), b"j");
            // Once moved, nothing more happens.
            assert_eq!(migrate_data_dir(&dir, &legacy), dir);
        }

        #[test]
        fn leaves_both_alone_when_the_new_one_exists() {
            let base = Base::new("both");
            let (dir, legacy) = (base.0.join("wdm"), base.0.join("whoosh"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir_all(&legacy).unwrap();
            std::fs::write(legacy.join("state.db"), b"old").unwrap();

            assert_eq!(migrate_data_dir(&dir, &legacy), dir);
            assert!(legacy.join("state.db").exists());
            assert!(!dir.join("state.db").exists());
        }

        #[test]
        fn nothing_to_move_is_the_new_directory() {
            let base = Base::new("none");
            let (dir, legacy) = (base.0.join("wdm"), base.0.join("whoosh"));
            assert_eq!(migrate_data_dir(&dir, &legacy), dir);
            assert!(!dir.exists() && !legacy.exists());
        }

        #[test]
        fn keeps_using_the_old_one_when_it_cannot_be_moved() {
            let base = Base::new("stuck");
            // The new name's parent does not exist: the rename fails.
            let (dir, legacy) = (base.0.join("missing/wdm"), base.0.join("whoosh"));
            std::fs::create_dir_all(&legacy).unwrap();
            assert_eq!(migrate_data_dir(&dir, &legacy), legacy);
            assert!(legacy.exists());
        }
    }

    #[test]
    fn debug_redacts_the_token() {
        let info = RuntimeInfo {
            port: 1,
            token: "s3cret".into(),
            pid: 2,
            version: "0".into(),
        };
        assert!(!format!("{info:?}").contains("s3cret"));
    }

    #[test]
    fn missing_runtime_file_is_reported_as_such() {
        let err = read_runtime(Path::new("/nonexistent/wdm/runtime.json")).unwrap_err();
        assert!(matches!(err, RuntimeError::Missing(_)), "{err}");
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        struct TempDir(PathBuf);
        impl TempDir {
            fn new(tag: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "wdm-proto-rt-{tag}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                chmod(&dir, 0o700);
                Self(dir)
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn chmod(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        fn write(dir: &Path, mode: u32) -> PathBuf {
            let path = dir.join(RUNTIME_FILE);
            std::fs::write(
                &path,
                r#"{"port":47210,"token":"t","pid":42,"version":"0.1.0","extra":true}"#,
            )
            .unwrap();
            chmod(&path, mode);
            path
        }

        #[test]
        fn reads_a_private_file_and_ignores_unknown_fields() {
            let dir = TempDir::new("ok");
            let info = read_runtime(&write(&dir.0, 0o600)).unwrap();
            assert_eq!((info.port, info.token.as_str(), info.pid), (47210, "t", 42));
        }

        #[test]
        fn refuses_a_file_others_can_read() {
            let dir = TempDir::new("mode");
            let err = read_runtime(&write(&dir.0, 0o644)).unwrap_err();
            assert!(matches!(err, RuntimeError::Insecure { .. }), "{err}");
        }

        #[test]
        fn refuses_a_directory_others_can_write() {
            let dir = TempDir::new("dir");
            let path = write(&dir.0, 0o600);
            chmod(&dir.0, 0o775);
            let err = read_runtime(&path).unwrap_err();
            assert!(matches!(err, RuntimeError::Insecure { .. }), "{err}");
            assert!(check_private_dir(&dir.0).is_err());
            chmod(&dir.0, 0o700);
            assert!(check_private_dir(&dir.0).is_ok());
        }

        #[test]
        fn refuses_a_symlink() {
            let dir = TempDir::new("link");
            let real = dir.0.join("real.json");
            std::fs::rename(write(&dir.0, 0o600), &real).unwrap();
            std::os::unix::fs::symlink(&real, dir.0.join(RUNTIME_FILE)).unwrap();
            let err = read_runtime(&dir.0.join(RUNTIME_FILE)).unwrap_err();
            assert!(matches!(err, RuntimeError::Insecure { .. }), "{err}");
        }
    }
    #[cfg(windows)]
    mod windows {
        use std::io::Write;
        use std::process::Command;

        use super::*;
        use crate::winacl;

        struct TempDir(PathBuf);
        impl TempDir {
            fn new(tag: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "wdm-proto-rt-{tag}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                winacl::create_private_dir(&dir).unwrap();
                Self(dir)
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// Grants Everyone modify access, as `chmod 0666`/`0777` would.
        fn loosen(path: &Path) {
            let out = Command::new("icacls")
                .arg(path)
                .args(["/grant", "*S-1-1-0:M"])
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }

        fn write(dir: &Path) -> PathBuf {
            let path = dir.join(RUNTIME_FILE);
            let mut f = winacl::create_private_file(&path).unwrap();
            f.write_all(br#"{"port":47210,"token":"t","pid":42,"version":"0.1.0","extra":true}"#)
                .unwrap();
            path
        }

        #[test]
        fn reads_a_private_file_and_ignores_unknown_fields() {
            let dir = TempDir::new("ok");
            let info = read_runtime(&write(&dir.0)).unwrap();
            assert_eq!((info.port, info.token.as_str(), info.pid), (47210, "t", 42));
            check_private_file(&dir.0.join(RUNTIME_FILE)).unwrap();
        }

        #[test]
        fn refuses_a_file_others_can_open() {
            let dir = TempDir::new("acl");
            let path = write(&dir.0);
            loosen(&path);
            let err = read_runtime(&path).unwrap_err();
            assert!(matches!(err, RuntimeError::Insecure { .. }), "{err}");
            winacl::make_private(&path).unwrap();
            read_runtime(&path).unwrap();
        }

        #[test]
        fn refuses_a_directory_others_can_write() {
            let dir = TempDir::new("dir");
            let path = write(&dir.0);
            loosen(&dir.0);
            let err = read_runtime(&path).unwrap_err();
            assert!(matches!(err, RuntimeError::Insecure { .. }), "{err}");
            assert!(check_private_dir(&dir.0).is_err());
            winacl::make_private(&dir.0).unwrap();
            assert!(check_private_dir(&dir.0).is_ok());
            read_runtime(&path).unwrap();
        }
    }
}
