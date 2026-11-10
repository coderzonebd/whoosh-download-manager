//! The data directory rule of PROTOCOL.md §1.
//!
//! `$VAYU_DATA_DIR` if set; otherwise Linux `$XDG_DATA_HOME/vayu` (default
//! `~/.local/share/vayu`), macOS `~/Library/Application Support/vayu`,
//! Windows `%LOCALAPPDATA%\vayu`.
//!
//! This is the same rule, with the same shape, as `vayu-nmhost`'s
//! `runtime` module (a test checks that both agree for every OS). Both are
//! meant to move into `vayu-proto` so there is a single copy.

use std::path::PathBuf;

/// Overrides the data directory (tests, portable installs, a second daemon).
pub const DATA_DIR_ENV: &str = "VAYU_DATA_DIR";

/// The environment variables that decide the data directory, captured once
/// so that resolving it is a pure function, testable for every OS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    pub vayu_data_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
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
            // The XDG spec says relative paths are invalid and must be ignored.
            xdg_data_home: var("XDG_DATA_HOME").filter(|p| p.is_absolute()),
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot determine the data directory: {0} is not set (set VAYU_DATA_DIR)")]
pub struct NoBaseDir(pub &'static str);

/// Resolves the data directory for `os` from `env`.
pub fn data_dir(os: Os, env: &Env) -> Result<PathBuf, NoBaseDir> {
    if let Some(dir) = &env.vayu_data_dir {
        return Ok(dir.clone());
    }
    let home = || env.home.clone().ok_or(NoBaseDir("HOME"));
    Ok(match os {
        Os::Linux => match &env.xdg_data_home {
            Some(xdg) => xdg.join("vayu"),
            None => home()?.join(".local/share/vayu"),
        },
        Os::MacOs => home()?.join("Library/Application Support/vayu"),
        Os::Windows => env
            .local_app_data
            .clone()
            .ok_or(NoBaseDir("LOCALAPPDATA"))?
            .join("vayu"),
    })
}

/// The data directory of this process, from its environment.
pub fn default_data_dir() -> Result<PathBuf, NoBaseDir> {
    data_dir(Os::current(), &Env::from_process())
}

/// `<data dir>/logs`.
pub fn log_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("logs")
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
    fn per_os_defaults() {
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
    fn xdg_and_override() {
        let xdg = Env {
            xdg_data_home: Some("/data".into()),
            ..env()
        };
        assert_eq!(
            data_dir(Os::Linux, &xdg).unwrap(),
            PathBuf::from("/data/vayu")
        );
        // XDG is a Linux rule only.
        assert_eq!(
            data_dir(Os::MacOs, &xdg).unwrap(),
            PathBuf::from("/home/u/Library/Application Support/vayu")
        );
        let portable = Env {
            vayu_data_dir: Some("/portable".into()),
            ..xdg
        };
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert_eq!(data_dir(os, &portable).unwrap(), PathBuf::from("/portable"));
        }
    }

    #[test]
    fn missing_base_dirs_are_errors() {
        assert_eq!(data_dir(Os::Linux, &Env::default()), Err(NoBaseDir("HOME")));
        assert_eq!(
            data_dir(Os::Windows, &Env::default()),
            Err(NoBaseDir("LOCALAPPDATA"))
        );
    }
}
