// SPDX-License-Identifier: GPL-3.0-or-later
//! "Open" and "Open folder" for a finished download (the prompt window's
//! "Download complete").
//!
//! A window names a download, never a path: the file is the one the daemon
//! says that download saved, opened only when the download completed and
//! its name is a plain file name in an absolute folder. So a window cannot
//! become a way to open arbitrary files or programs on this computer.

use std::path::{Path, PathBuf};

use wdm_proto::DownloadState;

/// The file of a download in `state`, saved as `filename` in `directory`:
/// only a completed download's, only an absolute path, only a plain name.
pub fn file_of(state: DownloadState, directory: &str, filename: &str) -> Result<PathBuf, String> {
    if state != DownloadState::Completed {
        return Err("the download has not finished".into());
    }
    let dir = Path::new(directory);
    let plain = !filename.is_empty()
        && filename != "."
        && filename != ".."
        && !filename.contains(['/', '\\']);
    if !dir.is_absolute() || !plain {
        return Err("the download has no usable file name".into());
    }
    Ok(dir.join(filename))
}

fn spawn(mut cmd: std::process::Command) -> Result<(), String> {
    // From an AppImage, the file's app must not load the image's bundled libraries.
    wdm_proto::outside_appimage!(cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Reaped on a thread so no zombie is left behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The file in its usual app.
pub fn open_file(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path);
    spawn(cmd)
}

/// A web page in the user's browser. Only for addresses the program
/// builds itself (an extension's store page), never one a window names.
pub fn open_web_page(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("not a web page".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url);
    spawn(cmd)
}

/// The file's folder, with the file selected where the system can.
pub fn reveal_file(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg("-R").arg(path);
        spawn(cmd)
    }
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.arg(arg);
        spawn(cmd)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let dir = path.parent().ok_or("the file has no folder")?;
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(dir);
        spawn(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const DIR: &str = "/Users/u/Downloads";
    #[cfg(windows)]
    const DIR: &str = "C:\\Users\\u\\Downloads";

    #[test]
    fn only_a_completed_download_with_a_plain_name_is_opened() {
        assert_eq!(
            file_of(DownloadState::Completed, DIR, "a.mkv").unwrap(),
            Path::new(DIR).join("a.mkv")
        );
        assert!(file_of(DownloadState::Downloading, DIR, "a.mkv").is_err());
        assert!(file_of(DownloadState::Completed, "Downloads", "a.mkv").is_err());
        for bad in ["", ".", "..", "../x", "a/b", "a\\b"] {
            assert!(
                file_of(DownloadState::Completed, DIR, bad).is_err(),
                "{bad:?}"
            );
        }
    }
}
