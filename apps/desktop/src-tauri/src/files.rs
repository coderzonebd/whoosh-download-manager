// SPDX-License-Identifier: GPL-3.0-or-later
//! "Open" and "Open folder" for a finished download (the prompt window's
//! "Download complete").
//!
//! The UI names a download, never a path: the shell asks the daemon where
//! that download's file is and opens it only when the download completed
//! and the file is there. So the webview cannot become a way to open
//! arbitrary files or programs on this computer.

use std::path::PathBuf;

use wdm_client::daemon::{self, Found};
use wdm_proto::{DownloadDetail, IdParams, method};
use wdm_tray::files::{file_of, open_file, reveal_file};

use crate::daemon::{client_info, data_dir};

/// Where a completed download's file is, as the daemon says.
async fn completed_file(id: String) -> Result<PathBuf, String> {
    let connected = match daemon::connect_running(&data_dir()?, &client_info())
        .await
        .map_err(|e| e.to_string())?
    {
        Found::Running(c) => c,
        Found::NotRunning(why) => return Err(why),
    };
    let detail: Result<DownloadDetail, _> = connected
        .client
        .call(method::DOWNLOAD_GET, IdParams { id })
        .await;
    connected.client.close().await;
    let s = detail.map_err(|e| e.to_string())?.summary;
    file_of(s.state, &s.directory, &s.filename)
}

/// Opens a completed download's file (`reveal`: shows it in its folder).
#[tauri::command]
pub async fn open_download(id: String, reveal: bool) -> Result<(), String> {
    let path = completed_file(id).await?;
    if !path.exists() {
        return Err(format!("{} is not there any more", path.display()));
    }
    if reveal {
        reveal_file(&path)
    } else {
        open_file(&path)
    }
}
