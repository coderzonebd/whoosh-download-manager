// SPDX-License-Identifier: GPL-3.0-or-later
//! WDM for Android. A phone has no room for a second process the system would
//! keep alive, so the daemon (`wdm-daemon`'s library) runs inside the app: it
//! listens on 127.0.0.1 as on the desktop, and the shared UI connects to it
//! over WebSocket with the token from `daemon_connection`. Other apps on the
//! phone can reach the port but not the token (owner-only `runtime.json` in
//! the app's private data directory).
//!
//! It starts on the first `daemon_connection` and stays up for the life of
//! the process. Media downloads (yt-dlp/ffmpeg) are not part of this build.

use std::path::PathBuf;

use serde::Serialize;
use tauri::Manager;
use tokio::sync::OnceCell;
use wdm_daemon::{DaemonConfig, RunningDaemon};

/// What the UI needs to open its WebSocket (the same shape as the desktop's).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    url: String,
    token: String,
    daemon_version: String,
}

struct Daemon {
    data_dir: PathBuf,
    download_dir: PathBuf,
    running: OnceCell<RunningDaemon>,
}

async fn start(data_dir: PathBuf, download_dir: PathBuf) -> Result<RunningDaemon, String> {
    let mut config = DaemonConfig::new(&data_dir);
    // Any free port: another app may hold the desktop default.
    config.port = 0;
    let claim = wdm_daemon::claim(config).await.map_err(|e| e.to_string())?;

    let net = wdm_net::NetConfig {
        platform_roots: false,
        extra_root_certs: webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .map(|c| c.as_ref().to_vec())
            .collect(),
        ..wdm_net::NetConfig::default()
    };
    let mut store_ms = 0.0;
    let service = wdm_daemon::engine::start(
        &data_dir,
        net,
        download_dir.to_string_lossy().into_owned(),
        &mut store_ms,
        None,
    )
    .await
    .map_err(|e| format!("cannot start the download engine: {e}"))?;
    let daemon = claim.serve(service).await.map_err(|e| e.to_string())?;
    tracing::info!(port = daemon.port(), store_ms, "in-process daemon ready");
    Ok(daemon)
}

#[tauri::command]
async fn daemon_connection(state: tauri::State<'_, Daemon>) -> Result<Connection, String> {
    let daemon = state
        .running
        .get_or_try_init(|| start(state.data_dir.clone(), state.download_dir.clone()))
        .await?;
    let info = daemon.runtime_info();
    Ok(Connection {
        url: daemon.rpc_url(),
        token: info.token.clone(),
        daemon_version: info.version.clone(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt().with_ansi(false).try_init();
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let paths = app.path();
            let data_dir = paths.app_data_dir()?;
            // Android: the shared Download folder, where the app may create its own files
            // (Android 11+); elsewhere the user's downloads folder.
            let download_dir = paths
                .download_dir()
                .unwrap_or_else(|_| data_dir.join("downloads"));
            app.manage(Daemon {
                data_dir,
                download_dir,
                running: OnceCell::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![daemon_connection])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!("cannot start WDM: {e}");
            std::process::exit(1);
        });
}
