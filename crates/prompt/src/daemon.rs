// SPDX-License-Identifier: GPL-3.0-or-later
//! The prompt window's connection to `wdm-daemon`, on a thread of its own
//! (a small Tokio runtime) so the window never waits for the network.
//!
//! The window sends [`Cmd`]s; what comes back ([`Msg`]: the list, the
//! daemon's events, the answers to calls) is handed to the window's thread
//! with `post`. A daemon that goes away is connected to again (and started,
//! like the tray does, if none runs).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use wdm_client::daemon;
use wdm_proto::media::{MediaAddParams, MediaOffer};
use wdm_proto::queue::Queue;
use wdm_proto::{ClientInfo, DownloadSummary, ListParams, Settings, method};

/// How long to wait before connecting again.
const RETRY: Duration = Duration::from_secs(2);

/// What a call was for, so its answer finds its way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    /// "Start download" / "Download later" on a waiting download.
    Confirm {
        id: String,
        start: bool,
    },
    /// "Cancel" on a waiting or a followed download (removes it).
    Remove(String),
    /// The add dialog checked a link (its `seq`, to drop stale answers).
    Probe(u64),
    MediaProbe(u64),
    /// The add dialog added a download (`start`: and started it).
    Add {
        start: bool,
    },
    /// Pause, Resume, Retry: their answer only matters when it fails.
    Quiet,
}

#[derive(Debug)]
pub enum Cmd {
    Call {
        method: &'static str,
        params: Value,
        tag: Tag,
    },
    /// "Whole playlist": one `media.add` per video, in order.
    AddPlaylist(Vec<MediaAddParams>),
}

#[derive(Debug)]
pub enum Msg {
    Connected,
    Disconnected,
    List(Vec<DownloadSummary>),
    Settings(Settings),
    /// The daemon's queues; `None` when it has none.
    Queues(Option<Vec<Queue>>),
    /// The daemon downloads from video sites.
    Media(bool),
    /// A page the extension offered, now this window's.
    Offer(MediaOffer),
    Event(String, Value),
    Reply(Tag, Result<Value, String>),
    /// While a playlist is added: how many are in, of how many.
    PlaylistProgress(usize, usize),
    /// The playlist is in (how many), or stopped after some with an error.
    PlaylistDone(usize, Option<String>),
}

pub fn client_info() -> ClientInfo {
    // As the app's windows: the daemon counts a window that can ask the
    // user, so the native host need not start the app.
    ClientInfo {
        name: "desktop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Runs the connection until the window's side of `cmds` closes.
pub fn start(
    dir: PathBuf,
    cmds: mpsc::UnboundedReceiver<Cmd>,
    post: impl Fn(Msg) + Send + Sync + 'static,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                eprintln!("wdm-prompt: no runtime: {err}");
                return;
            }
        };
        rt.block_on(run(dir, cmds, Arc::new(post)));
    });
}

type Post = Arc<dyn Fn(Msg) + Send + Sync>;

async fn run(dir: PathBuf, mut cmds: mpsc::UnboundedReceiver<Cmd>, post: Post) {
    let exe = daemon::daemon_exe();
    loop {
        let media = wdm_tray::prefs::load().media_tools_dir;
        let args: Vec<String> = if media.is_empty() {
            Vec::new()
        } else {
            vec!["--media-tools-dir".into(), media]
        };
        let client = match daemon::ensure_with_args(&dir, true, &client_info(), &exe, &args).await {
            Ok(c) => Arc::new(c.client),
            Err(err) => {
                eprintln!("wdm-prompt: no daemon: {err}");
                // Calls made meanwhile fail at once rather than hang.
                let wait = tokio::time::sleep(RETRY);
                tokio::pin!(wait);
                loop {
                    tokio::select! {
                        () = &mut wait => break,
                        cmd = cmds.recv() => match cmd {
                            Some(cmd) => fail(&post, cmd, "WDM is not running"),
                            None => return,
                        },
                    }
                }
                continue;
            }
        };
        post(Msg::Connected);
        load(&client, &post).await;
        loop {
            tokio::select! {
                ev = client.next_event() => match ev {
                    Some(ev) => on_event(&client, &post, ev.method, ev.params),
                    None => break,
                },
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => handle(&client, &post, cmd),
                    None => return,
                },
            }
        }
        post(Msg::Disconnected);
        tokio::time::sleep(RETRY).await;
    }
}

fn fail(post: &Post, cmd: Cmd, why: &str) {
    match cmd {
        Cmd::Call { tag, .. } => post(Msg::Reply(tag, Err(why.to_owned()))),
        Cmd::AddPlaylist(_) => post(Msg::PlaylistDone(0, Some(why.to_owned()))),
    }
}

/// Everything the window needs, at start and after a reconnect.
async fn load(client: &wdm_client::Client, post: &Post) {
    if let Ok(s) = client
        .call::<_, Settings>(method::SETTINGS_GET, Value::Null)
        .await
    {
        post(Msg::Settings(s));
    }
    post(Msg::Queues(
        client
            .call::<_, Vec<Queue>>(method::QUEUE_LIST, Value::Null)
            .await
            .ok(),
    ));
    post(Msg::Media(
        client
            .call_value(method::MEDIA_TOOLS, Value::Null)
            .await
            .is_ok(),
    ));
    // Offers before the list: the window may close once the list is in.
    if let Ok(offers) = client
        .call::<_, Vec<MediaOffer>>(method::MEDIA_OFFERS, Value::Null)
        .await
    {
        for o in offers {
            take(client, post, &o.id).await;
        }
    }
    match client
        .call::<_, Vec<DownloadSummary>>(method::DOWNLOAD_LIST, ListParams::default())
        .await
    {
        Ok(list) => post(Msg::List(list)),
        Err(err) => eprintln!("wdm-prompt: download.list: {err}"),
    }
}

async fn take(client: &wdm_client::Client, post: &Post, id: &str) {
    // Taken by another window meanwhile, or expired: nothing to show.
    if let Ok(offer) = client
        .call::<_, MediaOffer>(method::MEDIA_TAKE_OFFER, serde_json::json!({ "id": id }))
        .await
    {
        post(Msg::Offer(offer));
    }
}

fn on_event(client: &Arc<wdm_client::Client>, post: &Post, method: String, params: Value) {
    if method == wdm_proto::event::MEDIA_OFFERED
        && let Some(id) = params.get("id").and_then(Value::as_str)
    {
        let (client, post, id) = (client.clone(), post.clone(), id.to_owned());
        tokio::spawn(async move { take(&client, &post, &id).await });
        return;
    }
    post(Msg::Event(method, params));
}

fn handle(client: &Arc<wdm_client::Client>, post: &Post, cmd: Cmd) {
    let (client, post) = (client.clone(), post.clone());
    match cmd {
        Cmd::Call {
            method,
            params,
            tag,
        } => {
            tokio::spawn(async move {
                let answer = client
                    .call_value(method, params)
                    .await
                    .map_err(|e| e.to_string());
                post(Msg::Reply(tag, answer));
            });
        }
        Cmd::AddPlaylist(list) => {
            tokio::spawn(async move {
                let total = list.len();
                for (done, params) in list.into_iter().enumerate() {
                    if let Err(err) = client
                        .call_value(
                            method::MEDIA_ADD,
                            serde_json::to_value(params).unwrap_or_default(),
                        )
                        .await
                    {
                        post(Msg::PlaylistDone(done, Some(err.to_string())));
                        return;
                    }
                    post(Msg::PlaylistProgress(done + 1, total));
                }
                post(Msg::PlaylistDone(total, None));
            });
        }
    }
}
