//! Where the daemon publishes its port and token (`docs/api/PROTOCOL.md` §1).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Overrides the data directory. Meant for tests, portable installs and
/// running a second daemon side by side; the daemon must honour the same
/// variable for the two to meet.
pub const DATA_DIR_ENV: &str = "VAYU_DATA_DIR";

pub const RUNTIME_FILE: &str = "runtime.json";

/// The part of `runtime.json` the host needs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RuntimeInfo {
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub version: Option<String>,
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
    #[error("cannot determine the data directory: {0} is not set")]
    NoBaseDir(&'static str),
}

/// The environment variables that decide the data directory, captured once so
/// the resolution itself is a pure function (and testable for every OS).
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub vayu_data_dir: Option<PathBuf>,
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
            vayu_data_dir: var(DATA_DIR_ENV),
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
/// Linux `$XDG_DATA_HOME/vayu` (default `~/.local/share/vayu`),
/// macOS `~/Library/Application Support/vayu`, Windows `%LOCALAPPDATA%\vayu`.
pub fn data_dir(os: Os, env: &Env) -> Result<PathBuf, RuntimeError> {
    if let Some(dir) = &env.vayu_data_dir {
        return Ok(dir.clone());
    }
    let home = || env.home.clone().ok_or(RuntimeError::NoBaseDir("HOME"));
    Ok(match os {
        Os::Linux => match &env.xdg_data_home {
            Some(xdg) => xdg.join("vayu"),
            None => home()?.join(".local/share/vayu"),
        },
        Os::MacOs => home()?.join("Library/Application Support/vayu"),
        Os::Windows => env
            .local_app_data
            .clone()
            .ok_or(RuntimeError::NoBaseDir("LOCALAPPDATA"))?
            .join("vayu"),
    })
}

pub fn runtime_file(os: Os, env: &Env) -> Result<PathBuf, RuntimeError> {
    Ok(data_dir(os, env)?.join(RUNTIME_FILE))
}

/// Reads the runtime file. Read on every connection attempt: the daemon may
/// have restarted on another port with a new token.
pub fn read_runtime(path: &Path) -> Result<RuntimeInfo, RuntimeError> {
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
            PathBuf::from("/home/u/.local/share/vayu")
        );
        assert_eq!(
            data_dir(Os::MacOs, &env()).unwrap(),
            PathBuf::from("/home/u/Library/Application Support/vayu")
        );
        assert_eq!(
            data_dir(Os::Windows, &env()).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Local").join("vayu")
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
            PathBuf::from("/data/vayu")
        );
    }

    #[test]
    fn override_wins_everywhere() {
        let env = Env {
            vayu_data_dir: Some("/portable".into()),
            ..env()
        };
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert_eq!(data_dir(os, &env).unwrap(), PathBuf::from("/portable"));
        }
    }

    #[test]
    fn parses_runtime_file_and_ignores_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("vayu-nmhost-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RUNTIME_FILE);
        std::fs::write(
            &path,
            r#"{"port":47210,"token":"t","pid":42,"version":"0.1.0","extra":true}"#,
        )
        .unwrap();
        let info = read_runtime(&path).unwrap();
        assert_eq!(info.port, 47210);
        assert_eq!(info.token, "t");
        assert_eq!(info.pid, Some(42));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_runtime_file_is_reported_as_such() {
        let err = read_runtime(Path::new("/nonexistent/vayu/runtime.json")).unwrap_err();
        assert!(matches!(err, RuntimeError::Missing(_)));
    }
}
