// SPDX-License-Identifier: GPL-3.0-or-later
//! WDM for Android. A phone has no room for a second process the system would
//! keep alive, so the daemon (`wdm-daemon`'s library) runs inside the app: it
//! listens on 127.0.0.1 as on the desktop, and the shared UI connects to it
//! over WebSocket with the token from `daemon_connection`. Other apps on the
//! phone can reach the port but not the token (owner-only `runtime.json` in
//! the app's private data directory).
//!
//! It starts on the first `daemon_connection` and stays up for the life of
//! the process.
//!
//! Video and audio sites work as on the desktop: yt-dlp, ffmpeg and QuickJS
//! come inside the app (youtubedl-android's Python build, unpacked by
//! `MediaTools.kt`), and the media service runs them as separate programs
//! with the paths and environment Kotlin hands over ([`media_config`]).
//!
//! While something downloads, [`watch_downloads`] keeps Android's foreground
//! service running (`WdmPlugin.setActive`, Kotlin), so the process — and the
//! engine in it — lives on with the app in the background. [`android_update`]
//! tells the UI about a newer release on GitHub (the app is not in a store).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Manager;
use tokio::sync::OnceCell;
use wdm_daemon::{DaemonConfig, RunningDaemon};
use wdm_proto::service::Service;

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

async fn start(
    data_dir: PathBuf,
    download_dir: PathBuf,
    app: tauri::AppHandle,
) -> Result<RunningDaemon, String> {
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
    let tools = media_tools(&app).await;
    let service = match tools {
        Some(tools) => {
            let media = media_config(&data_dir, &net, tools);
            wdm_daemon::engine::start_with_media(
                &data_dir,
                net,
                download_dir.to_string_lossy().into_owned(),
                &mut store_ms,
                media,
            )
            .await
        }
        None => {
            wdm_daemon::engine::start(
                &data_dir,
                net,
                download_dir.to_string_lossy().into_owned(),
                &mut store_ms,
                None,
            )
            .await
        }
    }
    .map_err(|e| format!("cannot start the download engine: {e}"))?;
    let daemon = claim
        .serve(service.clone())
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(port = daemon.port(), store_ms, "in-process daemon ready");
    tauri::async_runtime::spawn(watch_downloads(service, app));
    Ok(daemon)
}

/// What `WdmPlugin.mediaTools` (Kotlin) answers once youtubedl-android has unpacked Python and
/// ffmpeg: the programs (all in the app's native library directory, the only place Android lets
/// an app run programs from) and the environment Python needs.
#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MediaTools {
    python: String,
    yt_dlp: String,
    ffmpeg: String,
    quickjs: String,
    env: std::collections::BTreeMap<String, String>,
}

/// The phone's media tools, or `None` (then the app downloads files only). The first start
/// after an install unpacks about 40 MB, a few seconds.
#[cfg(target_os = "android")]
async fn media_tools(app: &tauri::AppHandle) -> Option<MediaTools> {
    let app = app.clone();
    let got = tauri::async_runtime::spawn_blocking(move || {
        let plugin = app.try_state::<AndroidPlugin>()?;
        match plugin.0.run_mobile_plugin::<MediaTools>("mediaTools", ()) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("video and audio sites unavailable: {e}");
                None
            }
        }
    })
    .await
    .ok()
    .flatten()?;
    let usable = [&got.python, &got.yt_dlp, &got.ffmpeg]
        .iter()
        .all(|p| std::path::Path::new(p).is_file());
    if !usable {
        tracing::warn!(
            ?got,
            "video and audio sites unavailable: a media program is missing"
        );
        return None;
    }
    Some(got)
}

#[cfg(not(target_os = "android"))]
async fn media_tools(_app: &tauri::AppHandle) -> Option<MediaTools> {
    None
}

/// The media service over the app's own tools: yt-dlp is Python running the yt-dlp file, every
/// program gets Python's environment (its libraries, certificates, home). They are fixed: the
/// app updates yt-dlp itself (`MediaTools.kt`), the rest with the app.
fn media_config(
    data_dir: &std::path::Path,
    net: &wdm_net::NetConfig,
    t: MediaTools,
) -> wdm_media::MediaConfig {
    let env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        t.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let tool = |program: &str, prefix: Vec<std::ffi::OsString>| wdm_media::Tool {
        program: program.into(),
        prefix,
        env: env.clone(),
    };
    let mut config = wdm_media::MediaConfig::new(data_dir.join("media"));
    config.system_tools = false;
    config.auto_update = false;
    config.yt_dlp = Some(tool(&t.python, vec![t.yt_dlp.clone().into()]));
    config.ffmpeg = Some(tool(&t.ffmpeg, Vec::new()));
    if std::path::Path::new(&t.quickjs).is_file() {
        config.quickjs = Some(tool(&t.quickjs, Vec::new()));
    }
    config.fetcher = Some(Arc::new(wdm_media::NetFetcher::new(
        wdm_net::HttpSourceFactory::new(net.clone()),
    )));
    config
}

/// `WdmPlugin.setActive`'s arguments.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Active {
    active: u64,
    title: String,
    text: String,
}

/// "3.4 MB/s"
fn speed(bps: u64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let mut v = bps as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bps} B/s")
    } else {
        format!("{v:.1} {}/s", units[u])
    }
}

fn active_of(stats: &wdm_proto::Stats) -> Active {
    let active = u64::from(stats.active) + u64::from(stats.queued);
    Active {
        active,
        title: if stats.active == 1 {
            "Downloading 1 file".into()
        } else {
            format!("Downloading {} files", stats.active)
        },
        text: if stats.queued > 0 {
            format!("{} · {} waiting", speed(stats.download_bps), stats.queued)
        } else {
            speed(stats.download_bps)
        },
    }
}

/// Every two seconds: something downloading or waiting keeps the foreground service (and its
/// notification) up to date; nothing left stops it.
async fn watch_downloads(service: Arc<dyn Service>, app: tauri::AppHandle) {
    let mut last: Option<Active> = None;
    loop {
        if let Ok(stats) = service.stats().await {
            let now = active_of(&stats);
            if last.as_ref() != Some(&now) {
                set_active(&app, &now);
                last = Some(now);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(target_os = "android")]
fn set_active(app: &tauri::AppHandle, active: &Active) {
    if let Some(plugin) = app.try_state::<AndroidPlugin>()
        && let Err(e) = plugin
            .0
            .run_mobile_plugin::<()>("setActive", active.clone())
    {
        tracing::warn!("download service: {e}");
    }
}

#[cfg(not(target_os = "android"))]
fn set_active(_app: &tauri::AppHandle, _active: &Active) {}

#[cfg(target_os = "android")]
struct AndroidPlugin(tauri::plugin::PluginHandle<tauri::Wry>);

/// The Kotlin side (`WdmPlugin`), registered when the app starts.
fn android_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("wdm")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin("com.coderzonebd.whoosh", "WdmPlugin")?;
                _app.manage(AndroidPlugin(handle));
            }
            Ok(())
        })
        .build()
}

/// A newer release than this app, for the UI to offer.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    /// The release's page on GitHub.
    url: String,
    /// The release's APK and the SHA-256 GitHub lists for it, for the app to install itself
    /// (`AppUpdate.kt`); absent when GitHub lists no checksum.
    #[serde(skip_serializing_if = "Option::is_none")]
    apk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

const RELEASE_DOWNLOADS: &str =
    "https://github.com/coderzonebd/whoosh-download-manager/releases/download/";

/// The release's `_android.apk` asset: its address and SHA-256, if GitHub lists one.
fn apk_asset(release: &serde_json::Value) -> Option<(String, String)> {
    let asset = release["assets"].as_array()?.iter().find(|x| {
        x["name"]
            .as_str()
            .is_some_and(|n| n.ends_with("_android.apk"))
    })?;
    let url = asset["browser_download_url"]
        .as_str()
        .filter(|u| u.starts_with(RELEASE_DOWNLOADS))?;
    let sha = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))?;
    Some((url.to_owned(), sha.to_ascii_lowercase()))
}

/// `1.2.0-beta.3` → (1, 2, 0, false, 3): numbers, then a pre-release below its release.
fn version_key(v: &str) -> Option<(u64, u64, u64, bool, u64)> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let mut n = core.split('.').map(|x| x.parse::<u64>().ok());
    let (a, b, c) = (n.next()??, n.next()??, n.next()??);
    let pre_n = pre
        .and_then(|p| p.rsplit('.').next())
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    Some((a, b, c, pre.is_none(), pre_n))
}

/// The newest release that ships an Android app, if newer than this one. Pre-releases count
/// while this app is one (a beta tester gets the next beta).
fn newer_release(json: &str, current: &str) -> Option<UpdateInfo> {
    let list: serde_json::Value = serde_json::from_str(json).ok()?;
    let mine = version_key(current)?;
    let on_beta = current.contains('-');
    list.as_array()?
        .iter()
        .filter(|r| r["draft"] != true && (on_beta || r["prerelease"] != true))
        .filter(|r| {
            r["assets"].as_array().is_some_and(|a| {
                a.iter().any(|x| {
                    x["name"]
                        .as_str()
                        .is_some_and(|n| n.ends_with("_android.apk"))
                })
            })
        })
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let key = version_key(tag)?;
            let url = r["html_url"].as_str()?.to_owned();
            (key > mine && url.starts_with("https://github.com/")).then_some((key, tag, url, r))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, tag, url, r)| {
            let apk = apk_asset(r);
            UpdateInfo {
                version: tag.trim_start_matches('v').to_owned(),
                url,
                apk: apk.as_ref().map(|a| a.0.clone()),
                sha256: apk.map(|a| a.1),
            }
        })
}

const RELEASES: &str =
    "https://api.github.com/repos/coderzonebd/whoosh-download-manager/releases?per_page=10";

/// A newer release on GitHub, or `null`. Any failure (offline, no releases yet) is `null`.
#[tauri::command]
async fn android_update() -> Option<UpdateInfo> {
    use futures::StreamExt;
    use wdm_core::SourceFactory;
    let net = wdm_net::NetConfig {
        platform_roots: false,
        extra_root_certs: webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .map(|c| c.as_ref().to_vec())
            .collect(),
        ..wdm_net::NetConfig::default()
    };
    let factory = wdm_net::HttpSourceFactory::new(net);
    let source = factory.source(RELEASES, None).ok()?;
    let (_, mut stream) = source.start().await.ok()?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > 4 << 20 {
            return None;
        }
    }
    newer_release(std::str::from_utf8(&body).ok()?, env!("CARGO_PKG_VERSION"))
}

#[tauri::command]
async fn daemon_connection(
    app: tauri::AppHandle,
    state: tauri::State<'_, Daemon>,
) -> Result<Connection, String> {
    let daemon = state
        .running
        .get_or_try_init(|| start(state.data_dir.clone(), state.download_dir.clone(), app))
        .await?;
    let info = daemon.runtime_info();
    Ok(Connection {
        url: daemon.rpc_url(),
        token: info.token.clone(),
        daemon_version: info.version.clone(),
    })
}

/// Where downloads go until the user picks a folder. On Android the phone's shared Download
/// folder, where every file manager and app looks (Android 11 and later let an app create and
/// rename its own files there without any permission); Tauri's own "download dir" there is the
/// app's private folder under `Android/data`, which people do not find.
fn default_download_dir(tauri_dir: Option<PathBuf>, data_dir: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "android")]
    {
        let _ = tauri_dir;
        let shared = PathBuf::from("/storage/emulated/0/Download");
        if shared.is_dir() {
            return shared;
        }
        data_dir.join("downloads")
    }
    #[cfg(not(target_os = "android"))]
    tauri_dir.unwrap_or_else(|| data_dir.join("downloads"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt().with_ansi(false).try_init();
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(android_plugin())
        .setup(|app| {
            let paths = app.path();
            let data_dir = paths.app_data_dir()?;
            let download_dir = default_download_dir(paths.download_dir().ok(), &data_dir);
            app.manage(Daemon {
                data_dir,
                download_dir,
                running: OnceCell::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![daemon_connection, android_update])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!("cannot start WDM: {e}");
            std::process::exit(1);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_releases_after_their_betas() {
        assert!(version_key("1.0.0").unwrap() > version_key("1.0.0-beta.3").unwrap());
        assert!(version_key("v1.0.0-beta.3").unwrap() > version_key("1.0.0-beta.2").unwrap());
        assert!(version_key("1.0.1").unwrap() > version_key("1.0.0").unwrap());
        assert!(version_key("nonsense").is_none());
    }

    #[test]
    fn offers_the_newest_release_with_an_android_app() {
        let json = r#"[
          {"tag_name":"v1.0.0-beta.2","prerelease":true,"draft":false,"html_url":"https://github.com/o/r/releases/tag/v1.0.0-beta.2",
           "assets":[{"name":"Whoosh-Download-Manager_1.0.0-beta.2_android.apk"}]},
          {"tag_name":"v0.9.0","prerelease":false,"draft":false,"html_url":"https://github.com/o/r/releases/tag/v0.9.0",
           "assets":[{"name":"Whoosh-Download-Manager_0.9.0_android.apk"}]},
          {"tag_name":"v2.0.0","prerelease":false,"draft":false,"html_url":"https://github.com/o/r/releases/tag/v2.0.0",
           "assets":[{"name":"something-else.zip"}]}
        ]"#;
        // A beta sees the next beta; a release does not.
        assert_eq!(
            newer_release(json, "1.0.0-beta.1").unwrap().version,
            "1.0.0-beta.2"
        );
        assert_eq!(newer_release(json, "0.8.0").unwrap().version, "0.9.0");
        assert!(
            newer_release(json, "1.0.0").is_none(),
            "v2.0.0 has no Android app"
        );
        assert!(newer_release("not json", "0.1.0").is_none());
    }

    #[test]
    fn hands_over_the_apk_only_from_this_repository_with_a_checksum() {
        let sha = "ab".repeat(32);
        let release = |url: &str, digest: &str| {
            format!(
                r#"[{{"tag_name":"v1.0.0","prerelease":false,"draft":false,"html_url":"https://github.com/o/r/releases/tag/v1.0.0",
                 "assets":[{{"name":"Whoosh-Download-Manager_1.0.0_android.apk","browser_download_url":"{url}","digest":"{digest}"}}]}}]"#
            )
        };
        let ours = "https://github.com/coderzonebd/whoosh-download-manager/releases/download/v1.0.0/Whoosh-Download-Manager_1.0.0_android.apk";
        let got = newer_release(&release(ours, &format!("sha256:{sha}")), "0.9.0").unwrap();
        assert_eq!(got.apk.as_deref(), Some(ours));
        assert_eq!(got.sha256.as_deref(), Some(sha.as_str()));
        // Elsewhere, or without a checksum: only the release page.
        let other = newer_release(
            &release(
                "https://example.com/x_android.apk",
                &format!("sha256:{sha}"),
            ),
            "0.9.0",
        )
        .unwrap();
        assert_eq!(other.apk, None);
        assert_eq!(
            newer_release(&release(ours, ""), "0.9.0").unwrap().apk,
            None
        );
    }

    #[test]
    fn says_what_downloads() {
        let s = |active, queued, bps| wdm_proto::Stats {
            download_bps: bps,
            upload_bps: 0,
            active,
            queued,
        };
        let a = active_of(&s(2, 1, 3_400_000));
        assert_eq!(
            (a.active, a.title.as_str(), a.text.as_str()),
            (3, "Downloading 2 files", "3.4 MB/s · 1 waiting")
        );
        assert_eq!(active_of(&s(0, 0, 0)).active, 0);
        assert_eq!(speed(950), "950 B/s");
    }
}
