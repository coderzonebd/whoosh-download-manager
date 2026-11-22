// SPDX-License-Identifier: GPL-3.0-or-later
//! The app's own watch on the daemon, while it runs (a window is open):
//! the prompt window, and the notifications for downloads that end.
//!
//! The main window could open the prompt window by itself, but in the
//! background its page may be suspended by the system while its connection
//! to the daemon stays up. So this side listens to the daemon's events and
//! opens the prompt window when a browser download waits for confirmation,
//! when the extension offers a page, and when a download the prompt window
//! follows (`set_followed`) ends, so its "Download complete" comes up even
//! after the user hid its progress. A download that finishes or fails while
//! the main window is not in front gets a notification (in front, its own
//! toast says it).
//!
//! With no window open the tray process does all this (`crates/tray`); it
//! reads the followed downloads from [`wdm_tray::FOLLOWED_FILE`]. And when
//! the tray's Quit stopped the daemon, this ends the app too.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};
use wdm_client::daemon::{self, Found};
use wdm_proto::{event, method};

use wdm_tray::i18n::Notice;

use crate::daemon::{client_info, data_dir};

/// How long to wait before connecting again (no daemon yet, or it went away).
const RETRY: Duration = Duration::from_secs(3);

/// Downloads the prompt window follows; it keeps the list and sends it here.
#[derive(Default)]
pub struct Followed(Mutex<HashSet<String>>);

impl Followed {
    fn contains(&self, id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }
}

/// The prompt window's followed downloads changed.
#[tauri::command]
pub fn set_followed(state: tauri::State<'_, Followed>, ids: Vec<String>) {
    let ids: HashSet<String> = ids
        .into_iter()
        .filter(|id| !id.is_empty() && id.len() <= 200)
        .take(200)
        .collect();
    // For the tray, which opens the prompt window when no window is open.
    if let Ok(dir) = data_dir() {
        let mut list: Vec<&String> = ids.iter().collect();
        list.sort();
        if let Ok(json) = serde_json::to_vec(&list) {
            let _ = std::fs::write(dir.join(wdm_tray::FOLLOWED_FILE), json);
        }
    }
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = ids;
}

/// What an event asks of the prompt window.
fn wants_prompt(method: &str, params: &Value, followed: &dyn Fn(&str) -> bool) -> bool {
    match method {
        event::MEDIA_OFFERED => true,
        event::DOWNLOAD_ADDED | event::DOWNLOAD_UPDATED => waits(params),
        event::DOWNLOAD_STATE => {
            let ended = matches!(
                params.get("state").and_then(Value::as_str),
                Some("completed" | "failed")
            );
            ended
                && params
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(followed)
        }
        _ => false,
    }
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

/// Says that a download ended, unless the main window is in front.
async fn notify_ended<R: Runtime>(
    app: &AppHandle<R>,
    client: &wdm_client::Client,
    id: &str,
    ok: bool,
) {
    if crate::main_window_in_front(app) {
        return;
    }
    let detail: Result<Value, _> = client
        .call(method::DOWNLOAD_GET, serde_json::json!({ "id": id }))
        .await;
    let name = detail
        .ok()
        .and_then(|d| d.get("filename").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let notice = if ok {
        Notice::Completed(&name)
    } else {
        Notice::Failed(&name)
    };
    crate::notify(app, &notice);
}

/// The user quit WDM (`wdm_proto::runtime::APP_QUIT_FILE`).
fn quit_marked() -> bool {
    data_dir().is_ok_and(|d| d.join(wdm_proto::runtime::APP_QUIT_FILE).exists())
}

/// Something already waits when the shell connects (it started after it arrived).
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

fn open<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    // Windows are made on the main thread.
    let _ = app.run_on_main_thread(move || {
        if let Err(err) = crate::prompt::open(&handle) {
            eprintln!("wdm-desktop: could not open the prompt window: {err}");
        }
    });
}

/// Watches the daemon until `stop`, connecting again whenever it goes away.
pub fn start<R: Runtime>(app: &AppHandle<R>, stop: &'static AtomicBool) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        while !stop.load(Ordering::SeqCst) {
            let found = match data_dir() {
                Ok(dir) => daemon::connect_running(&dir, &client_info()).await.ok(),
                Err(_) => None,
            };
            let Some(Found::Running(connected)) = found else {
                tokio::time::sleep(RETRY).await;
                continue;
            };
            let client = connected.client;
            // At start the main window asks too; after a reconnect only this does.
            if !first && waiting_now(&client).await {
                open(&app);
            }
            first = false;
            while let Some(ev) = client.next_event().await {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let followed = |id: &str| app.state::<Followed>().contains(id);
                if wants_prompt(&ev.method, &ev.params, &followed) {
                    open(&app);
                }
                if ev.method == event::DOWNLOAD_STATE
                    && let Some((id, ok)) = ended(&ev.params)
                {
                    notify_ended(&app, &client, id, ok).await;
                }
            }
            client.close().await;
            // The tray's Quit stopped the daemon: the app goes too.
            if quit_marked() && !stop.load(Ordering::SeqCst) {
                stop.store(true, Ordering::SeqCst);
                crate::exit_cleanly(&app).await;
                return;
            }
            tokio::time::sleep(RETRY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tells_which_downloads_ended() {
        assert_eq!(
            ended(&json!({ "id": "dl_1", "state": "completed" })),
            Some(("dl_1", true))
        );
        assert_eq!(
            ended(&json!({ "id": "dl_2", "state": "failed", "error": "x" })),
            Some(("dl_2", false))
        );
        assert_eq!(ended(&json!({ "id": "dl_3", "state": "paused" })), None);
        assert_eq!(ended(&json!({ "state": "completed" })), None);
    }

    #[test]
    fn opens_for_waiting_downloads_offers_and_followed_downloads_that_end() {
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
        assert!(wants_prompt(
            event::DOWNLOAD_STATE,
            &json!({ "id": "dl_1", "state": "failed" }),
            &only
        ));

        assert!(!wants_prompt(
            event::DOWNLOAD_ADDED,
            &json!({ "id": "dl_9", "state": "downloading" }),
            &none
        ));
        assert!(!wants_prompt(
            event::DOWNLOAD_UPDATED,
            &json!({ "id": "dl_9", "state": "downloading", "awaitingConfirmation": true }),
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
        assert!(!wants_prompt(
            event::DOWNLOAD_PROGRESS,
            &json!({ "items": [] }),
            &only
        ));
    }
}
