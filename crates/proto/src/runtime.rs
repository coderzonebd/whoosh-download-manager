//! Where the daemon publishes its port and token (PROTOCOL.md §1), shared by
//! the daemon, the CLI and `whoosh-nmhost` so the three always agree.
//!
//! On Unix, [`read_runtime`] refuses a file that another user could have
//! written: a symlink, a file not owned by the current user or readable by
//! anyone else, or one in a directory that others can write to. Otherwise a
//! local attacker could point clients at a port of their choosing. On Windows
//! the data directory under `%LOCALAPPDATA%` is private by inheritance and no
//! check is made.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Overrides the data directory. Meant for tests, portable installs and
/// running a second daemon side by side.
pub const DATA_DIR_ENV: &str = "WHOOSH_DATA_DIR";

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
    pub whoosh_data_dir: Option<PathBuf>,
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
            whoosh_data_dir: var(DATA_DIR_ENV),
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

/// The daemon's data directory:
/// Linux `$XDG_DATA_HOME/whoosh` (default `~/.local/share/whoosh`),
/// macOS `~/Library/Application Support/whoosh`, Windows `%LOCALAPPDATA%\whoosh`.
/// `WHOOSH_DATA_DIR` overrides it everywhere.
pub fn data_dir(os: Os, env: &Env) -> Result<PathBuf, RuntimeError> {
    if let Some(dir) = &env.whoosh_data_dir {
        return Ok(dir.clone());
    }
    let home = || env.home.clone().ok_or(RuntimeError::NoBaseDir("HOME"));
    Ok(match os {
        Os::Linux => match &env.xdg_data_home {
            Some(xdg) => xdg.join("whoosh"),
            None => home()?.join(".local/share/whoosh"),
        },
        Os::MacOs => home()?.join("Library/Application Support/whoosh"),
        Os::Windows => env
            .local_app_data
            .clone()
            .ok_or(RuntimeError::NoBaseDir("LOCALAPPDATA"))?
            .join("whoosh"),
    })
}

/// [`data_dir`] for this process's OS and environment.
pub fn default_data_dir() -> Result<PathBuf, RuntimeError> {
    data_dir(Os::current(), &Env::from_process())
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
/// else can write to (Unix; always Ok on Windows). The daemon calls this on
/// its data directory at start, after tightening a directory it owns.
pub fn check_private_dir(dir: &Path) -> Result<(), RuntimeError> {
    imp::check_dir(dir)
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

#[cfg(not(unix))]
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
            PathBuf::from("/home/u/.local/share/whoosh")
        );
        assert_eq!(
            data_dir(Os::MacOs, &env()).unwrap(),
            PathBuf::from("/home/u/Library/Application Support/whoosh")
        );
        assert_eq!(
            data_dir(Os::Windows, &env()).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Local").join("whoosh")
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
            PathBuf::from("/data/whoosh")
        );
    }

    #[test]
    fn override_wins_everywhere() {
        let env = Env {
            whoosh_data_dir: Some("/portable".into()),
            ..env()
        };
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert_eq!(data_dir(os, &env).unwrap(), PathBuf::from("/portable"));
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
        let err = read_runtime(Path::new("/nonexistent/whoosh/runtime.json")).unwrap_err();
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
                    "whoosh-proto-rt-{tag}-{}-{:?}",
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
}
