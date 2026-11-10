//! Where to find the daemon: the data directory rule and `runtime.json`
//! (PROTOCOL.md §1). The same rule as `vayu-daemon::paths` and
//! `vayu-nmhost::runtime`; a test checks the CLI agrees with the host. All
//! three are meant to share one copy in `vayu-proto`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const RUNTIME_FILE: &str = "runtime.json";

#[derive(Debug, Clone, Default)]
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
            vayu_data_dir: var("VAYU_DATA_DIR"),
            home: var("HOME").or_else(|| var("USERPROFILE")),
            xdg_data_home: var("XDG_DATA_HOME").filter(|p| p.is_absolute()),
            local_app_data: var("LOCALAPPDATA"),
        }
    }
}

// Same names as `vayu-nmhost`'s `runtime::Os`, for the planned move to proto.
#[allow(clippy::enum_variant_names)]
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
            Os::Linux
        }
    }
}

pub fn data_dir(os: Os, env: &Env) -> Result<PathBuf, String> {
    if let Some(dir) = &env.vayu_data_dir {
        return Ok(dir.clone());
    }
    let home = || {
        env.home
            .clone()
            .ok_or_else(|| "HOME is not set (set VAYU_DATA_DIR)".to_owned())
    };
    Ok(match os {
        Os::Linux => match &env.xdg_data_home {
            Some(xdg) => xdg.join("vayu"),
            None => home()?.join(".local/share/vayu"),
        },
        Os::MacOs => home()?.join("Library/Application Support/vayu"),
        Os::Windows => env
            .local_app_data
            .clone()
            .ok_or_else(|| "LOCALAPPDATA is not set (set VAYU_DATA_DIR)".to_owned())?
            .join("vayu"),
    })
}

/// `port` and `token` are what the CLI needs; `pid` and `version` are for
/// `vayu daemon status|stop`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct RuntimeInfo {
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub version: Option<String>,
}

impl std::fmt::Debug for RuntimeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeInfo")
            .field("port", &self.port)
            .field("pid", &self.pid)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// `Ok(None)` when there is no runtime file, or it cannot be parsed (a
/// half-written file from an old build, or garbage: treated as "no daemon").
pub fn read(data_dir: &Path) -> std::io::Result<Option<RuntimeInfo>> {
    match std::fs::read(data_dir.join(RUNTIME_FILE)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI and the native messaging host must find the same daemon.
    #[test]
    fn agrees_with_the_native_host() {
        use vayu_nmhost::runtime as nm;
        let opt = |s: &str| (!s.is_empty()).then(|| PathBuf::from(s));
        for (vayu, home, xdg, local) in [
            ("", "/home/u", "", r"C:\Users\u\AppData\Local"),
            ("/portable", "/home/u", "/xdg", ""),
            ("", "/home/u", "/xdg", ""),
            ("", "", "/xdg", ""),
            ("", "", "", ""),
        ] {
            let ours = Env {
                vayu_data_dir: opt(vayu),
                home: opt(home),
                xdg_data_home: opt(xdg),
                local_app_data: opt(local),
            };
            let theirs = nm::Env {
                vayu_data_dir: opt(vayu),
                home: opt(home),
                xdg_data_home: opt(xdg),
                xdg_config_home: None,
                local_app_data: opt(local),
            };
            for (os, nm_os) in [
                (Os::Linux, nm::Os::Linux),
                (Os::MacOs, nm::Os::MacOs),
                (Os::Windows, nm::Os::Windows),
            ] {
                assert_eq!(
                    data_dir(os, &ours).ok(),
                    nm::data_dir(nm_os, &theirs).ok(),
                    "{os:?} {vayu:?} {home:?} {xdg:?} {local:?}"
                );
            }
        }
    }

    #[test]
    fn unreadable_runtime_files_mean_no_daemon() {
        let dir = std::env::temp_dir().join(format!("vayu-cli-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir).unwrap().is_none());
        std::fs::write(dir.join(RUNTIME_FILE), b"{\"port\":").unwrap();
        assert!(read(&dir).unwrap().is_none());
        std::fs::write(
            dir.join(RUNTIME_FILE),
            br#"{"port":1,"token":"t","pid":2,"version":"v","extra":1}"#,
        )
        .unwrap();
        let info = read(&dir).unwrap().unwrap();
        assert_eq!((info.port, info.pid), (1, Some(2)));
        assert!(!format!("{info:?}").contains("\"t\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
