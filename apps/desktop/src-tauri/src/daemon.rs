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

use std::path::{Path, PathBuf};

use serde::Serialize;
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

pub async fn connection(data_dir: &Path, exe: &Path) -> Result<Connection, String> {
    let connected = daemon::ensure(data_dir, true, &client_info(), exe)
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

/// Called by the UI before every (re)connection.
#[tauri::command]
pub async fn daemon_connection() -> Result<Connection, String> {
    connection(&data_dir()?, &daemon::daemon_exe()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_as_desktop() {
        let who = client_info();
        assert_eq!(who.name, "desktop");
        assert!(!who.version.is_empty());
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
