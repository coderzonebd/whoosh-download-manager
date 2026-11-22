// SPDX-License-Identifier: GPL-3.0-or-later
//! What the prompt window knows from the daemon, and which one thing it
//! shows: like `PromptApp.svelte` and `AppState` in the web UI, which it
//! replaces on the desktop.
//!
//! In order: a browser download that waits for confirmation (oldest
//! first), else a page the extension offered (the add dialog), else a
//! download started here that it follows: one that ended first, so
//! "Download complete" never waits behind a running one, else the first
//! running one the user did not hide. When none is left the window closes.

use std::collections::HashMap;

use serde_json::Value;
use wdm_proto::media::{MediaOffer, MediaToolsStatus};
use wdm_proto::queue::{MAIN_QUEUE, Queue};
use wdm_proto::{
    DownloadState, DownloadSummary, ProgressEvent, RemovedEvent, Settings, StateEvent, event,
};
use wdm_tray::followed::Followed;

/// The one thing the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A browser download waiting for "Start download" (its id), with its
    /// 1-based place among the waiting ones and how many wait.
    Confirm {
        id: String,
        index: usize,
        total: usize,
    },
    /// A page the extension offered (the offer's id).
    Offer(String),
    /// A download started here (its id).
    Follow(String),
    /// Nothing left.
    Empty,
}

impl Item {
    /// Which dialog, and for what: the form starts afresh when this changes.
    pub fn key(&self) -> String {
        match self {
            Item::Confirm { id, .. } => format!("c:{id}"),
            Item::Offer(id) => format!("o:{id}"),
            Item::Follow(id) => format!("f:{id}"),
            Item::Empty => String::new(),
        }
    }
}

pub fn ended(state: DownloadState) -> bool {
    matches!(state, DownloadState::Completed | DownloadState::Failed)
}

pub fn pausable(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Queued | DownloadState::Probing | DownloadState::Downloading
    )
}

pub fn resumable(state: DownloadState) -> bool {
    matches!(state, DownloadState::Paused | DownloadState::Failed)
}

/// The queue a download is in ("main" when it names none).
pub fn queue_of(d: &DownloadSummary) -> &str {
    d.queue
        .as_deref()
        .filter(|q| !q.is_empty())
        .unwrap_or(MAIN_QUEUE)
}

/// What an event asks the caller to do besides showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// The extension offered a page: take it (`media.takeOffer`).
    TakeOffer(String),
    /// The followed downloads changed: save them for the tray.
    SaveFollowed,
}

#[derive(Debug, Default)]
pub struct Model {
    pub downloads: HashMap<String, DownloadSummary>,
    pub followed: Vec<Followed>,
    pub offers: Vec<MediaOffer>,
    pub settings: Option<Settings>,
    /// Named queues (not "main"); `None` when the daemon has none.
    pub queues: Option<Vec<String>>,
    /// The daemon can download from video sites (yt-dlp).
    pub media: bool,
    /// The download list loaded at least once: only then may the window close.
    pub loaded: bool,
}

impl Model {
    pub fn new(followed: Vec<Followed>) -> Self {
        Self {
            followed,
            ..Self::default()
        }
    }

    /// Browser downloads waiting for the user, oldest first.
    pub fn waiting(&self) -> Vec<&DownloadSummary> {
        let mut list: Vec<&DownloadSummary> = self
            .downloads
            .values()
            .filter(|d| d.awaiting_confirmation && d.state == DownloadState::Paused)
            .collect();
        list.sort_by(|a, b| a.added_at.cmp(&b.added_at).then_with(|| a.id.cmp(&b.id)));
        list
    }

    /// The followed download to show, if any.
    fn following(&self) -> Option<&str> {
        let known: Vec<(&Followed, &DownloadSummary)> = self
            .followed
            .iter()
            .filter_map(|f| self.downloads.get(&f.id).map(|d| (f, d)))
            .collect();
        known
            .iter()
            .find(|(_, d)| ended(d.state))
            .or_else(|| known.iter().find(|(f, _)| !f.hidden))
            .map(|(f, _)| f.id.as_str())
    }

    pub fn current(&self) -> Item {
        let waiting = self.waiting();
        if let Some(first) = waiting.first() {
            return Item::Confirm {
                id: first.id.clone(),
                index: 1,
                total: waiting.len(),
            };
        }
        if let Some(offer) = self.offers.first() {
            return Item::Offer(offer.id.clone());
        }
        match self.following() {
            Some(id) => Item::Follow(id.to_owned()),
            None => Item::Empty,
        }
    }

    /// Nothing is left to ask, and the list loaded: the window may close.
    pub fn done(&self) -> bool {
        self.loaded && self.current() == Item::Empty
    }

    /// The whole list, at start and after a reconnect.
    pub fn replace_all(&mut self, list: Vec<DownloadSummary>) -> Effect {
        self.downloads = list.into_iter().map(|d| (d.id.clone(), d)).collect();
        self.loaded = true;
        // Followed downloads removed meanwhile are forgotten.
        let before = self.followed.len();
        let downloads = &self.downloads;
        self.followed.retain(|f| downloads.contains_key(&f.id));
        if self.followed.len() == before {
            Effect::None
        } else {
            Effect::SaveFollowed
        }
    }

    pub fn set_queues(&mut self, list: &[Queue]) {
        self.queues = Some(
            list.iter()
                .filter(|q| q.name != MAIN_QUEUE)
                .map(|q| q.name.clone())
                .collect(),
        );
    }

    /// Queue names to choose from: "main" first; empty when there is no choice.
    pub fn queue_names(&self) -> Vec<String> {
        match &self.queues {
            Some(named) if !named.is_empty() => std::iter::once(MAIN_QUEUE.to_owned())
                .chain(named.iter().cloned())
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn has_queue(&self, name: &str) -> bool {
        name == MAIN_QUEUE
            || self
                .queues
                .as_ref()
                .is_some_and(|q| q.iter().any(|n| n == name))
    }

    pub fn add_offer(&mut self, offer: MediaOffer) {
        if !self.offers.iter().any(|o| o.id == offer.id) {
            self.offers.push(offer);
        }
    }

    pub fn done_with_offer(&mut self, id: &str) {
        self.offers.retain(|o| o.id != id);
    }

    /// Started here: follow it (its progress, then how it ended).
    pub fn follow(&mut self, id: &str) {
        self.followed.retain(|f| f.id != id);
        self.followed.push(Followed {
            id: id.to_owned(),
            hidden: false,
        });
    }

    /// "Hide": its progress goes; the window comes back when it ends.
    pub fn hide(&mut self, id: &str) {
        for f in &mut self.followed {
            if f.id == id {
                f.hidden = true;
            }
        }
    }

    /// Done with it ("Close" on how it ended).
    pub fn unfollow(&mut self, id: &str) {
        self.followed.retain(|f| f.id != id);
    }

    /// A download the user answered or cancelled: shown as answered at once,
    /// before the daemon's own event.
    pub fn answered(&mut self, id: &str) {
        if let Some(d) = self.downloads.get_mut(id) {
            d.awaiting_confirmation = false;
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.downloads.remove(id);
    }

    /// A daemon event.
    pub fn apply(&mut self, method: &str, params: Value) -> Effect {
        match method {
            event::DOWNLOAD_ADDED | event::DOWNLOAD_UPDATED => {
                if let Ok(d) = serde_json::from_value::<DownloadSummary>(params) {
                    self.downloads.insert(d.id.clone(), d);
                }
            }
            event::DOWNLOAD_PROGRESS => {
                if let Ok(e) = serde_json::from_value::<ProgressEvent>(params) {
                    for p in e.items {
                        if let Some(d) = self.downloads.get_mut(&p.id) {
                            d.downloaded_bytes = p.downloaded_bytes;
                            if p.total_bytes.is_some() {
                                d.total_bytes = p.total_bytes;
                            }
                            d.speed_bps = p.speed_bps;
                            d.eta_secs = p.eta_secs;
                            d.connections = p.connections;
                        }
                    }
                }
            }
            event::DOWNLOAD_STATE => {
                if let Ok(e) = serde_json::from_value::<StateEvent>(params)
                    && let Some(d) = self.downloads.get_mut(&e.id)
                {
                    d.state = e.state;
                    if e.error.is_some() || e.state != DownloadState::Failed {
                        d.error = e.error;
                    }
                    if e.state != DownloadState::Downloading {
                        d.speed_bps = 0;
                    }
                }
            }
            event::DOWNLOAD_REMOVED => {
                if let Ok(e) = serde_json::from_value::<RemovedEvent>(params) {
                    for id in &e.ids {
                        self.downloads.remove(id);
                    }
                    let before = self.followed.len();
                    self.followed.retain(|f| !e.ids.contains(&f.id));
                    if self.followed.len() != before {
                        return Effect::SaveFollowed;
                    }
                }
            }
            event::SETTINGS => {
                if let Ok(s) = serde_json::from_value::<Settings>(params) {
                    self.settings = Some(s);
                }
            }
            event::QUEUES => {
                if let Ok(list) = serde_json::from_value::<Vec<Queue>>(params) {
                    self.set_queues(&list);
                }
            }
            event::MEDIA_TOOLS => {
                if serde_json::from_value::<MediaToolsStatus>(params).is_ok() {
                    self.media = true;
                }
            }
            event::MEDIA_OFFERED => {
                if let Some(id) = params.get("id").and_then(Value::as_str)
                    && !self.offers.iter().any(|o| o.id == id)
                {
                    return Effect::TakeOffer(id.to_owned());
                }
            }
            _ => {}
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dl(id: &str, state: &str, added: u64, waits: bool) -> Value {
        json!({
            "id": id, "url": format!("https://e.example/{id}.zip"), "filename": format!("{id}.zip"),
            "directory": "/home/u/Downloads", "state": state, "downloadedBytes": 0, "speedBps": 0,
            "connections": 0, "resumable": true, "addedAt": added, "awaitingConfirmation": waits,
        })
    }

    fn model(list: &[Value]) -> Model {
        let mut m = Model::new(Vec::new());
        m.replace_all(
            list.iter()
                .map(|v| serde_json::from_value(v.clone()).unwrap())
                .collect(),
        );
        m
    }

    fn offer(id: &str) -> MediaOffer {
        serde_json::from_value(json!({"id": id, "url": "https://v.example/w", "offeredAt": 1}))
            .unwrap()
    }

    #[test]
    fn waiting_downloads_come_first_oldest_first() {
        let mut m = model(&[
            dl("b", "paused", 20, true),
            dl("a", "paused", 10, true),
            dl("c", "downloading", 5, false),
        ]);
        m.add_offer(offer("of_1"));
        assert_eq!(
            m.current(),
            Item::Confirm {
                id: "a".into(),
                index: 1,
                total: 2
            }
        );
        m.answered("a");
        assert_eq!(
            m.current(),
            Item::Confirm {
                id: "b".into(),
                index: 1,
                total: 1
            }
        );
        m.answered("b");
        assert_eq!(m.current(), Item::Offer("of_1".into()));
        m.done_with_offer("of_1");
        assert_eq!(m.current(), Item::Empty);
        assert!(m.done());
    }

    #[test]
    fn a_followed_download_shows_until_hidden_and_again_when_it_ends() {
        let mut m = model(&[
            dl("a", "downloading", 1, false),
            dl("b", "downloading", 2, false),
        ]);
        m.follow("a");
        m.follow("b");
        assert_eq!(m.current(), Item::Follow("a".into()));
        m.hide("a");
        assert_eq!(m.current(), Item::Follow("b".into()));
        m.hide("b");
        assert_eq!(m.current(), Item::Empty);
        // "b" ends: it comes back, ahead of anything running.
        m.apply(
            event::DOWNLOAD_STATE,
            json!({"id": "b", "state": "completed"}),
        );
        assert_eq!(m.current(), Item::Follow("b".into()));
        m.unfollow("b");
        assert_eq!(m.current(), Item::Empty);
    }

    #[test]
    fn the_window_waits_for_the_list_before_it_closes() {
        let m = Model::new(Vec::new());
        assert_eq!(m.current(), Item::Empty);
        assert!(!m.done());
    }

    #[test]
    fn events_keep_the_list_and_the_followed_downloads() {
        let mut m = model(&[]);
        m.follow("a");
        assert_eq!(m.replace_all(Vec::new()), Effect::SaveFollowed);
        assert!(m.followed.is_empty());

        m.apply(event::DOWNLOAD_ADDED, dl("a", "downloading", 1, false));
        m.follow("a");
        m.apply(
            event::DOWNLOAD_PROGRESS,
            json!({"items": [{"id": "a", "downloadedBytes": 500, "totalBytes": 1000, "speedBps": 50, "etaSecs": 10, "connections": 4}]}),
        );
        let d = &m.downloads["a"];
        assert_eq!(
            (d.downloaded_bytes, d.total_bytes, d.connections),
            (500, Some(1000), 4)
        );
        m.apply(
            event::DOWNLOAD_STATE,
            json!({"id": "a", "state": "failed", "error": {"code": "x", "message": "Gone"}}),
        );
        assert_eq!(m.downloads["a"].error.as_ref().unwrap().message, "Gone");
        assert_eq!(m.downloads["a"].speed_bps, 0);
        assert_eq!(
            m.apply(event::DOWNLOAD_REMOVED, json!({"ids": ["a"]})),
            Effect::SaveFollowed
        );
        assert!(m.followed.is_empty() && m.downloads.is_empty());
    }

    #[test]
    fn offers_are_taken_once() {
        let mut m = model(&[]);
        assert_eq!(
            m.apply(
                event::MEDIA_OFFERED,
                json!({"id": "of_1", "url": "https://v.example", "offeredAt": 1})
            ),
            Effect::TakeOffer("of_1".into())
        );
        m.add_offer(offer("of_1"));
        m.add_offer(offer("of_1"));
        assert_eq!(m.offers.len(), 1);
        assert_eq!(
            m.apply(event::MEDIA_OFFERED, json!({"id": "of_1"})),
            Effect::None
        );
    }

    #[test]
    fn queues_offer_a_choice_only_when_there_are_named_ones() {
        let mut m = model(&[]);
        assert!(m.queue_names().is_empty());
        let q = |name: &str| {
            serde_json::from_value::<Queue>(json!({"name": name, "maxActive": 1, "running": false}))
                .unwrap()
        };
        m.set_queues(&[q("main")]);
        assert!(m.queue_names().is_empty());
        m.set_queues(&[q("main"), q("Night")]);
        assert_eq!(m.queue_names(), vec!["main".to_owned(), "Night".to_owned()]);
        assert!(m.has_queue("Night") && m.has_queue("main") && !m.has_queue("Day"));
    }
}
