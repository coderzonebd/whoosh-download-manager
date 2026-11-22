// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray's own connection to the daemon, on a thread of its own.
//!
//! * The daemon is started when it is not running (at login, or after a
//!   crash), unless the user quit WDM (the `app-quit` mark), which ends the
//!   tray too.
//! * `event.stats` keeps the tooltip up to date, in the UI's language.
//! * While no window of the app is open (`app.lock` free), what the app
//!   would say comes from here: the notifications for downloads that end,
//!   and the prompt window (started as `wdm-desktop --prompt`) when the
//!   browser hands over a download, offers a page, or a download the prompt
//!   window follows ends. With a window open the app does it itself.
//! * Once a day it has the app look for an update (`wdm-desktop
//!   --update-check`, with no window), while no window is open.
//!
//! It connects as "desktop", so the daemon counts an app there and the
//! browser's host does not start one itself: this asks for the prompt.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use wdm_client::daemon::{self, Found};
use wdm_proto::{ClientInfo, event, method};
use wdm_tray::{i18n, locks, prefs};

use super::{Proxy, UserEvent, notify};

/// How long to wait before connecting again.
const RETRY: Duration = Duration::from_secs(3);
/// The prompt window is asked for at most this often (it takes a moment
/// to start, and a second start only brings it to the front).
const PROMPT_GAP: Duration = Duration::from_secs(3);
/// The first update check waits this long after the tray starts.
const UPDATE_FIRST_DELAY: Duration = Duration::from_secs(90);
/// How often the update schedule is looked at.
const UPDATE_TICK: Duration = Duration::from_secs(60 * 60);
const DAY: u64 = 24 * 60 * 60;

fn client_info() -> ClientInfo {
    ClientInfo {
        name: "desktop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Writes (`quit`) or removes the mark that the user quit WDM
/// (`wdm_proto::runtime::APP_QUIT_FILE`): while it is there the browser's
/// host leaves downloads to the browser instead of starting the app.
pub fn mark_quit(dir: &Path, quit: bool) {
    let path = dir.join(wdm_proto::runtime::APP_QUIT_FILE);
    let result = if quit {
        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, b""))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    if let Err(err) = result {
        eprintln!("wdm-tray: could not update {}: {err}", path.display());
    }
}

fn quit_marked(dir: &Path) -> bool {
    dir.join(wdm_proto::runtime::APP_QUIT_FILE).exists()
}

/// Quit: the daemon stops, and the mark tells the browser's host and a
/// running app (which then closes) that WDM is closed.
pub fn quit_everything(dir: &Path) {
    mark_quit(dir, true);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async {
        if let Ok(Found::Running(c)) = daemon::connect_running(dir, &client_info()).await
            && let Err(err) = daemon::stop(*c, dir).await
        {
            eprintln!("wdm-tray: could not stop the daemon: {err}");
        }
    });
}

/// A window of the app is open: it speaks for itself.
fn app_running(dir: &Path) -> bool {
    locks::held(dir, locks::APP)
}

/// The downloads the prompt window follows, as the app last wrote them.
fn followed(dir: &Path) -> HashSet<String> {
    std::fs::read(dir.join(wdm_tray::FOLLOWED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// A download summary that waits for the user's confirmation.
fn waits(summary: &Value) -> bool {
    summary.get("awaitingConfirmation").and_then(Value::as_bool) == Some(true)
        && summary.get("state").and_then(Value::as_str) == Some("paused")
}

/// The id of a download that ended, and whether it completed (else it failed).
fn ended(params: &Value) -> Option<(&str, bool)> {
    let id = params.get("id").and_then(Value::as_str)?;
    match params.get("state").and_then(Value::as_str)? {
        "completed" => Some((id, true)),
        "failed" => Some((id, false)),
        _ => None,
    }
}

/// What an event asks of the prompt window.
fn wants_prompt(method: &str, params: &Value, followed: &dyn Fn(&str) -> bool) -> bool {
    match method {
        event::MEDIA_OFFERED => true,
        event::DOWNLOAD_ADDED | event::DOWNLOAD_UPDATED => waits(params),
        event::DOWNLOAD_STATE => ended(params).is_some_and(|(id, _)| followed(id)),
        _ => false,
    }
}

/// Something already waits when the tray connects.
async fn waiting_now(client: &wdm_client::Client) -> bool {
    let offers: Result<Value, _> = client.call(method::MEDIA_OFFERS, Value::Null).await;
    if offers.is_ok_and(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        return true;
    }
    let list: Result<Value, _> = client
        .call(method::DOWNLOAD_LIST, serde_json::json!({}))
        .await;
    list.is_ok_and(|v| v.as_array().is_some_and(|a| a.iter().any(waits)))
}

/// The language the UI last used, re-read when `desktop.json` changes.
struct Language {
    file: Option<PathBuf>,
    seen: Option<SystemTime>,
    tag: String,
}

impl Language {
    fn new(tag: String) -> Self {
        let file = prefs::path();
        let seen = file.as_deref().and_then(modified);
        Self { file, seen, tag }
    }

    /// The new language when it changed.
    fn changed(&mut self) -> Option<&str> {
        let file = self.file.as_deref()?;
        let now = modified(file);
        if now == self.seen {
            return None;
        }
        self.seen = now;
        let tag = prefs::load_from(file).language;
        if tag == self.tag {
            return None;
        }
        self.tag = tag;
        Some(&self.tag)
    }
}

fn modified(file: &Path) -> Option<SystemTime> {
    std::fs::metadata(file).and_then(|m| m.modified()).ok()
}

struct Watch {
    dir: PathBuf,
    proxy: Proxy,
    language: Language,
    tooltip: String,
    last_prompt: Option<Instant>,
}

impl Watch {
    fn prompt(&mut self) {
        if self.last_prompt.is_some_and(|t| t.elapsed() < PROMPT_GAP) {
            return;
        }
        self.last_prompt = Some(Instant::now());
        if let Err(err) = wdm_tray::launch::start_app(&[wdm_tray::PROMPT_ARG]) {
            eprintln!("wdm-tray: could not open the prompt window: {err}");
        }
    }

    fn set_tooltip(&mut self, stats: &Value) {
        if let Some(tag) = self.language.changed() {
            let _ = self.proxy.send_event(UserEvent::Language(tag.to_owned()));
        }
        let n = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
        let text = i18n::tooltip(&self.language.tag, n("downloadBps"), n("active"));
        if text != self.tooltip {
            self.tooltip.clone_from(&text);
            let _ = self.proxy.send_event(UserEvent::Tooltip(text));
        }
    }

    async fn on_event(&mut self, client: &wdm_client::Client, method: &str, params: &Value) {
        if method == event::STATS {
            self.set_tooltip(params);
            return;
        }
        let relevant = matches!(
            method,
            event::MEDIA_OFFERED
                | event::DOWNLOAD_ADDED
                | event::DOWNLOAD_UPDATED
                | event::DOWNLOAD_STATE
        );
        if !relevant || app_running(&self.dir) {
            return;
        }
        let followed = followed(&self.dir);
        if wants_prompt(method, params, &|id| followed.contains(id)) {
            self.prompt();
        }
        if method == event::DOWNLOAD_STATE
            && let Some((id, ok)) = ended(params)
        {
            let detail: Result<Value, _> = client
                .call(method::DOWNLOAD_GET, serde_json::json!({ "id": id }))
                .await;
            let name = detail
                .ok()
                .and_then(|d| d.get("filename").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            let notice = if ok {
                i18n::Notice::Completed(&name)
            } else {
                i18n::Notice::Failed(&name)
            };
            notify::show(&self.language.tag, &notice);
        }
    }

    async fn run(mut self) {
        let exe = daemon::daemon_exe();
        loop {
            if quit_marked(&self.dir) {
                // The user quit WDM (from the app): the tray goes too.
                let _ = self.proxy.send_event(UserEvent::Exit);
                return;
            }
            let media = prefs::load().media_tools_dir;
            let args: Vec<String> = if media.is_empty() {
                Vec::new()
            } else {
                vec!["--media-tools-dir".into(), media]
            };
            let connected =
                daemon::ensure_with_args(&self.dir, true, &client_info(), &exe, &args).await;
            let client = match connected {
                Ok(c) => c.client,
                Err(err) => {
                    eprintln!(
                        "wdm-tray: the daemon is not running and could not be started: {err}"
                    );
                    tokio::time::sleep(RETRY).await;
                    continue;
                }
            };
            if !app_running(&self.dir) && waiting_now(&client).await {
                self.prompt();
            }
            while let Some(ev) = client.next_event().await {
                self.on_event(&client, &ev.method, &ev.params).await;
            }
            client.close().await;
            tokio::time::sleep(RETRY).await;
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Once a day, the app looks for an update with no window. (With a window
/// open it does so itself.)
fn update_schedule(dir: PathBuf) {
    if cfg!(debug_assertions) {
        return;
    }
    std::thread::sleep(UPDATE_FIRST_DELAY);
    let mut last_try = 0u64;
    loop {
        let p = prefs::load();
        let due = now().saturating_sub(p.last_update_check) >= DAY;
        // A run that could not record its check is not repeated every hour.
        let tried_lately = now().saturating_sub(last_try) < DAY / 4;
        if p.auto_update_check && due && !tried_lately && !app_running(&dir) {
            last_try = now();
            if let Err(err) = wdm_tray::launch::start_app(&[wdm_tray::UPDATE_CHECK_ARG]) {
                eprintln!("wdm-tray: could not start the update check: {err}");
            }
        }
        std::thread::sleep(UPDATE_TICK);
    }
}

/// Starts the watch and the update schedule.
pub fn start(dir: PathBuf, proxy: Proxy, language: String) {
    let d = dir.clone();
    std::thread::spawn(move || update_schedule(d));
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                eprintln!("wdm-tray: no async runtime: {err}");
                return;
            }
        };
        rt.block_on(
            Watch {
                dir,
                proxy,
                language: Language::new(language),
                tooltip: String::new(),
                last_prompt: None,
            }
            .run(),
        );
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn asks_for_the_prompt_for_waiting_downloads_offers_and_followed_downloads_that_end() {
        let none = |_: &str| false;
        let only = |id: &str| id == "dl_1";
        let waiting = json!({ "id": "dl_9", "state": "paused", "awaitingConfirmation": true });
        assert!(wants_prompt(event::DOWNLOAD_ADDED, &waiting, &none));
        assert!(wants_prompt(event::DOWNLOAD_UPDATED, &waiting, &none));
        assert!(wants_prompt(
            event::MEDIA_OFFERED,
            &json!({ "id": "of_1" }),
            &none
        ));
        assert!(wants_prompt(
            event::DOWNLOAD_STATE,
            &json!({ "id": "dl_1", "state": "completed" }),
            &only
        ));
        assert!(!wants_prompt(
            event::DOWNLOAD_ADDED,
            &json!({ "id": "dl_9", "state": "downloading" }),
            &none
        ));
        assert!(!wants_prompt(
            event::DOWNLOAD_STATE,
            &json!({ "id": "dl_2", "state": "completed" }),
            &only
        ));
        assert!(!wants_prompt(
            event::DOWNLOAD_STATE,
            &json!({ "id": "dl_1", "state": "paused" }),
            &only
        ));
    }

    #[test]
    fn tells_which_downloads_ended() {
        assert_eq!(
            ended(&json!({ "id": "dl_1", "state": "completed" })),
            Some(("dl_1", true))
        );
        assert_eq!(
            ended(&json!({ "id": "dl_2", "state": "failed" })),
            Some(("dl_2", false))
        );
        assert_eq!(ended(&json!({ "id": "dl_3", "state": "paused" })), None);
    }

    #[test]
    fn the_quit_mark_comes_and_goes() {
        let dir = std::env::temp_dir().join(format!("wdm-tray-quit-{}", std::process::id()));
        assert!(!quit_marked(&dir));
        mark_quit(&dir, true);
        assert!(quit_marked(&dir));
        mark_quit(&dir, false);
        assert!(!quit_marked(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn followed_downloads_come_from_the_apps_file() {
        let dir = std::env::temp_dir().join(format!("wdm-tray-followed-{}", std::process::id()));
        assert!(followed(&dir).is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(wdm_tray::FOLLOWED_FILE), r#"["dl_1","dl_2"]"#).unwrap();
        assert_eq!(
            followed(&dir),
            HashSet::from(["dl_1".to_owned(), "dl_2".to_owned()])
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
