// SPDX-License-Identifier: GPL-3.0-or-later
//! The app is a client of `whoosh-daemon` (ADR 0007). The UI asks for the
//! daemon's address and token with [`daemon_connection`] on every
//! (re)connection; that starts the daemon first if none is running, so a
//! daemon that crashed or was stopped comes back on the next attempt.
//!
//! The daemon executable is the one bundled next to this app (Tauri's
//! `externalBin` puts it there), else `$WHOOSH_DAEMON_BIN`, else `PATH`; in a
//! development build it is the sibling in `target/<profile>/` once
//! `cargo build -p whoosh-daemon` has run.
//!
//! The installers ship yt-dlp and ffmpeg in the app's resources
//! (`media-tools/`, see crates/media's `bundle` module); the daemon is told
//! where with `--media-tools-dir` and copies them into its data directory.
//!
//! The UI's reconnect loop is not enough on its own: while the window is
//! hidden in the tray the webview's timers are throttled, so a daemon that
//! crashed there would stay down and its downloads with it. [`supervise`]
//! watches from the shell instead.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, Runtime};
use whoosh_client::daemon::{self, Found};
use whoosh_proto::ClientInfo;

/// What the UI needs to open its WebSocket.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub url: String,
    pub token: String,
    pub daemon_version: String,
}

pub fn client_info() -> ClientInfo {
    ClientInfo {
        name: "desktop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// `$WHOOSH_DATA_DIR`, else the platform's data directory (PROTOCOL.md §1).
pub fn data_dir() -> Result<PathBuf, String> {
    whoosh_proto::runtime::default_data_dir().map_err(|e| e.to_string())
}

/// Where the installer put yt-dlp and ffmpeg: `media-tools/` in the app's
/// resources, when it has a manifest (development builds have none).
pub fn media_tools_dir<R: Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    let dir = app.path().resource_dir().ok()?.join(MEDIA_TOOLS);
    dir.join("manifest.json").is_file().then_some(dir)
}

/// The resource directory with the bundled media tools.
const MEDIA_TOOLS: &str = "media-tools";

/// The daemon's arguments for `media_tools`.
fn daemon_args(media_tools: Option<&Path>) -> Vec<String> {
    match media_tools {
        Some(dir) => vec!["--media-tools-dir".into(), dir.display().to_string()],
        None => Vec::new(),
    }
}

pub async fn connection(
    data_dir: &Path,
    exe: &Path,
    media_tools: Option<&Path>,
) -> Result<Connection, String> {
    let args = daemon_args(media_tools);
    let connected = daemon::ensure_with_args(data_dir, true, &client_info(), exe, &args)
        .await
        .map_err(|e| e.to_string())?;
    let info = connected.info;
    // The UI makes its own connection; this one only proved the daemon is
    // there and knows the token.
    connected.client.close().await;
    Ok(Connection {
        url: format!("ws://127.0.0.1:{}{}", info.port, whoosh_proto::RPC_PATH),
        token: info.token,
        daemon_version: info.version,
    })
}

/// Asks a running daemon to stop and waits until it has; nothing to do if
/// none is running.
pub async fn shutdown(data_dir: &Path) -> Result<(), String> {
    match daemon::connect_running(data_dir, &client_info())
        .await
        .map_err(|e| e.to_string())?
    {
        Found::Running(connected) => daemon::stop(*connected, data_dir)
            .await
            .map_err(|e| e.to_string()),
        Found::NotRunning(_) => Ok(()),
    }
}

/// How often the shell looks for the daemon.
const WATCH_EVERY: Duration = Duration::from_secs(5);

/// Whether the daemon `data_dir` describes is up: its runtime file names a
/// port and something accepts connections there. A cheap look (no hello, so
/// the daemon's log stays quiet); [`connection`] does the real check when
/// this says no.
fn looks_alive(data_dir: &Path) -> bool {
    let Ok(whoosh_client::runtime::Lookup::Found(info)) = whoosh_client::runtime::read(data_dir)
    else {
        return false;
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

/// Starts the daemon again whenever it is found gone, until `stop` is set
/// (the app quitting, which may stop the daemon on purpose). Runs on its own
/// thread for the life of the app.
pub fn supervise<R: Runtime>(app: &tauri::AppHandle<R>, stop: &'static AtomicBool) {
    let media_tools = media_tools_dir(app);
    std::thread::spawn(move || {
        let Ok(dir) = data_dir() else { return };
        let exe = daemon::daemon_exe();
        loop {
            std::thread::sleep(WATCH_EVERY);
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if looks_alive(&dir) {
                continue;
            }
            match tauri::async_runtime::block_on(connection(&dir, &exe, media_tools.as_deref())) {
                Ok(_) => eprintln!("whoosh-desktop: the daemon was not running; started it again"),
                Err(err) => eprintln!(
                    "whoosh-desktop: the daemon is not running and could not be started: {err}"
                ),
            }
        }
    });
}

/// Called by the UI before every (re)connection.
#[tauri::command]
pub async fn daemon_connection<R: Runtime>(app: tauri::AppHandle<R>) -> Result<Connection, String> {
    let media_tools = media_tools_dir(&app);
    connection(&data_dir()?, &daemon::daemon_exe(), media_tools.as_deref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_runtime_file_is_not_alive() {
        let dir = std::env::temp_dir().join(format!("whoosh-supervise-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!looks_alive(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identifies_as_desktop() {
        let who = client_info();
        assert_eq!(who.name, "desktop");
        assert!(!who.version.is_empty());
    }

    #[test]
    fn media_tools_reach_the_daemon_only_when_bundled() {
        assert!(daemon_args(None).is_empty());
        let dir = Path::new("/opt/whoosh/media-tools");
        assert_eq!(
            daemon_args(Some(dir)),
            vec!["--media-tools-dir".to_owned(), dir.display().to_string()]
        );
    }

    #[test]
    fn connection_is_camel_case_for_the_ui() {
        let c = Connection {
            url: "ws://127.0.0.1:1/rpc".into(),
            token: "t".into(),
            daemon_version: "0.1.0".into(),
        };
        let v = serde_json::to_value(c).unwrap();
        assert_eq!(v["daemonVersion"], "0.1.0");
        assert_eq!(v["url"], "ws://127.0.0.1:1/rpc");
    }
}
