//! The per-connection send queue, and the backpressure policy.
//!
//! One writer task drains the queue into the socket at whatever speed the
//! client reads. Everything else only pushes, so a slow client never slows
//! the service or other clients.
//!
//! * **Responses** and **lossless notifications** (`added`, `state`,
//!   `removed`, `settings`) are queued in order and always delivered.
//! * **Lossy notifications** (`Notification::is_lossy`: progress, stats) are
//!   coalesced. A new progress event is merged into the queued progress
//!   event, item by item (the newest values per download win), as long as
//!   nothing lossless was queued after it; otherwise it starts a new slot.
//!   Stats work the same way, newest wins. So a client that falls behind gets
//!   fewer, fresher updates, and never sees an update out of order with
//!   respect to the lossless events around it.
//! * If lossless notifications pile up beyond a limit, the client is too far
//!   behind to be useful: the queue is cleared and replaced by a close frame
//!   with [`CLOSE_TOO_SLOW`]. The client reconnects and re-reads the list.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio::sync::{Notify, OwnedSemaphorePermit};
use vayu_proto::service::Notification;
use vayu_proto::{ProgressEvent, ProgressItem, Stats};

/// Close code for a client that fell too far behind on lossless
/// notifications (proposed for PROTOCOL.md §1 next to 4401 and 4426).
pub const CLOSE_TOO_SLOW: u16 = 4429;

#[derive(Debug)]
pub enum Item {
    /// A serialised response; the permit limits in-flight requests until the
    /// response is actually written.
    Response(String, Option<OwnedSemaphorePermit>),
    Event(Notification),
    Progress(ProgressSlot),
    Stats(Stats),
    Close(u16, String),
}

#[derive(Debug, Default)]
pub struct ProgressSlot {
    items: Vec<ProgressItem>,
    index: HashMap<String, usize>,
}

impl ProgressSlot {
    fn merge(&mut self, event: ProgressEvent) {
        for item in event.items {
            match self.index.get(&item.id) {
                Some(&i) => {
                    let old = &mut self.items[i];
                    let segments = item.segments.or_else(|| old.segments.take());
                    *old = ProgressItem { segments, ..item };
                }
                None => {
                    self.index.insert(item.id.clone(), self.items.len());
                    self.items.push(item);
                }
            }
        }
    }

    pub fn into_event(self) -> ProgressEvent {
        ProgressEvent { items: self.items }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    Queued,
    Coalesced,
    /// The connection is closing; the item was discarded.
    Closed,
    /// Lossless backlog exceeded: the queue now ends in a close frame.
    Overflow,
}

#[derive(Debug)]
struct Inner {
    queue: VecDeque<(u64, Item)>,
    next_seq: u64,
    /// Sequence number of the newest ordering barrier (response or lossless
    /// event); lossy slots queued before it are no longer merge targets.
    last_barrier: Option<u64>,
    progress_slot: Option<u64>,
    stats_slot: Option<u64>,
    lossless_queued: usize,
    closing: bool,
    coalesced: u64,
}

#[derive(Debug)]
pub struct Outbox {
    inner: Mutex<Inner>,
    notify: Notify,
    max_lossless: usize,
}

impl Outbox {
    pub fn new(max_lossless: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                next_seq: 0,
                last_barrier: None,
                progress_slot: None,
                stats_slot: None,
                lossless_queued: 0,
                closing: false,
                coalesced: 0,
            }),
            notify: Notify::new(),
            max_lossless: max_lossless.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding this lock cannot leave the queue logically
        // broken (every operation is a few pushes/pops), so recover.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push_response(&self, text: String, permit: Option<OwnedSemaphorePermit>) -> Pushed {
        let mut inner = self.lock();
        if inner.closing {
            return Pushed::Closed;
        }
        let seq = inner.push(Item::Response(text, permit));
        inner.last_barrier = Some(seq);
        drop(inner);
        self.notify.notify_one();
        Pushed::Queued
    }

    pub fn push_notification(&self, n: Notification) -> Pushed {
        let mut inner = self.lock();
        if inner.closing {
            return Pushed::Closed;
        }
        let pushed = match n {
            Notification::DownloadProgress(event) => match inner.mergeable(inner.progress_slot) {
                Some(i) => {
                    if let (_, Item::Progress(slot)) = &mut inner.queue[i] {
                        slot.merge(event);
                    }
                    inner.coalesced += 1;
                    Pushed::Coalesced
                }
                None => {
                    let mut slot = ProgressSlot::default();
                    slot.merge(event);
                    let seq = inner.push(Item::Progress(slot));
                    inner.progress_slot = Some(seq);
                    Pushed::Queued
                }
            },
            Notification::Stats(stats) => match inner.mergeable(inner.stats_slot) {
                Some(i) => {
                    inner.queue[i].1 = Item::Stats(stats);
                    inner.coalesced += 1;
                    Pushed::Coalesced
                }
                None => {
                    let seq = inner.push(Item::Stats(stats));
                    inner.stats_slot = Some(seq);
                    Pushed::Queued
                }
            },
            lossless => {
                if inner.lossless_queued >= self.max_lossless {
                    inner.close_now(CLOSE_TOO_SLOW, "client too slow: event backlog exceeded");
                    drop(inner);
                    self.notify.notify_one();
                    return Pushed::Overflow;
                }
                let seq = inner.push(Item::Event(lossless));
                inner.last_barrier = Some(seq);
                inner.lossless_queued += 1;
                Pushed::Queued
            }
        };
        drop(inner);
        self.notify.notify_one();
        pushed
    }

    /// Discards everything not yet sent and queues a close frame. Later
    /// pushes are ignored. Does nothing if already closing.
    pub fn close(&self, code: u16, reason: &str) {
        let mut inner = self.lock();
        if inner.closing {
            return;
        }
        inner.close_now(code, reason);
        drop(inner);
        self.notify.notify_one();
    }

    pub fn is_closing(&self) -> bool {
        self.lock().closing
    }

    /// How many lossy notifications were merged into an earlier one.
    pub fn coalesced(&self) -> u64 {
        self.lock().coalesced
    }

    /// The next item, waiting for one if the queue is empty.
    pub async fn pop(&self) -> Item {
        loop {
            // Register interest before checking, so a push between the check
            // and the await is not missed.
            let notified = self.notify.notified();
            if let Some(item) = self.try_pop() {
                return item;
            }
            notified.await;
        }
    }

    pub fn try_pop(&self) -> Option<Item> {
        let mut inner = self.lock();
        let (seq, item) = inner.queue.pop_front()?;
        match &item {
            Item::Event(_) => inner.lossless_queued -= 1,
            Item::Progress(_) if inner.progress_slot == Some(seq) => inner.progress_slot = None,
            Item::Stats(_) if inner.stats_slot == Some(seq) => inner.stats_slot = None,
            _ => {}
        }
        Some(item)
    }
}

impl Inner {
    fn push(&mut self, item: Item) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back((seq, item));
        seq
    }

    /// Queue index of `slot` if it is still queued and nothing that orders
    /// against it was queued after it.
    fn mergeable(&self, slot: Option<u64>) -> Option<usize> {
        let seq = slot?;
        if self.last_barrier.is_some_and(|b| b > seq) {
            return None;
        }
        let front = self.queue.front()?.0;
        let index = usize::try_from(seq.checked_sub(front)?).ok()?;
        (index < self.queue.len()).then_some(index)
    }

    fn close_now(&mut self, code: u16, reason: &str) {
        self.queue.clear();
        self.progress_slot = None;
        self.stats_slot = None;
        self.lossless_queued = 0;
        self.closing = true;
        self.push(Item::Close(code, reason.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vayu_proto::{DownloadState, RemovedEvent, StateEvent};

    fn progress(id: &str, bytes: u64) -> Notification {
        Notification::DownloadProgress(ProgressEvent {
            items: vec![ProgressItem {
                id: id.into(),
                downloaded_bytes: bytes,
                total_bytes: Some(100),
                speed_bps: 1,
                eta_secs: None,
                connections: 1,
                segments: None,
            }],
        })
    }

    fn state(id: &str) -> Notification {
        Notification::DownloadState(StateEvent {
            id: id.into(),
            state: DownloadState::Paused,
            error: None,
        })
    }

    fn drain(outbox: &Outbox) -> Vec<String> {
        std::iter::from_fn(|| outbox.try_pop())
            .map(|item| match item {
                Item::Response(t, _) => format!("resp:{t}"),
                Item::Event(n) => format!("{}:{}", n.method(), n.params()),
                Item::Progress(slot) => {
                    let ev = slot.into_event();
                    let parts: Vec<_> = ev
                        .items
                        .iter()
                        .map(|i| format!("{}={}", i.id, i.downloaded_bytes))
                        .collect();
                    format!("progress:{}", parts.join(","))
                }
                Item::Stats(s) => format!("stats:{}", s.active),
                Item::Close(code, _) => format!("close:{code}"),
            })
            .collect()
    }

    #[test]
    fn lossy_events_coalesce_but_never_cross_lossless_ones() {
        let outbox = Outbox::new(100);
        assert_eq!(outbox.push_notification(progress("a", 1)), Pushed::Queued);
        assert_eq!(
            outbox.push_notification(progress("b", 1)),
            Pushed::Coalesced
        );
        assert_eq!(
            outbox.push_notification(progress("a", 2)),
            Pushed::Coalesced
        );
        let stats = |active| {
            Notification::Stats(Stats {
                active,
                ..Stats::default()
            })
        };
        assert_eq!(outbox.push_notification(stats(1)), Pushed::Queued);
        // A progress event may still merge across a stats slot (both lossy).
        assert_eq!(
            outbox.push_notification(progress("b", 5)),
            Pushed::Coalesced
        );
        assert_eq!(outbox.push_notification(stats(2)), Pushed::Coalesced);
        assert_eq!(outbox.push_notification(state("a")), Pushed::Queued);
        // After a lossless event, a new slot starts.
        assert_eq!(outbox.push_notification(progress("a", 3)), Pushed::Queued);
        assert_eq!(outbox.push_notification(stats(3)), Pushed::Queued);
        assert_eq!(
            drain(&outbox),
            [
                "progress:a=2,b=5",
                "stats:2",
                r#"event.download.state:{"id":"a","state":"paused"}"#,
                "progress:a=3",
                "stats:3",
            ]
        );
        assert_eq!(outbox.coalesced(), 4);
        // Popped slots are no longer merge targets.
        assert_eq!(outbox.push_notification(progress("a", 4)), Pushed::Queued);
    }

    #[test]
    fn segments_survive_a_merge_without_segments() {
        let outbox = Outbox::new(10);
        let mut with = progress("a", 1);
        if let Notification::DownloadProgress(ev) = &mut with {
            ev.items[0].segments = Some(vec![(0, 100, 1)]);
        }
        outbox.push_notification(with);
        outbox.push_notification(progress("a", 2));
        let Some(Item::Progress(slot)) = outbox.try_pop() else {
            panic!("expected progress");
        };
        let ev = slot.into_event();
        assert_eq!(ev.items[0].downloaded_bytes, 2);
        assert_eq!(ev.items[0].segments, Some(vec![(0, 100, 1)]));
    }

    #[test]
    fn lossless_overflow_closes_with_4429() {
        let outbox = Outbox::new(3);
        for _ in 0..3 {
            assert_eq!(outbox.push_notification(state("a")), Pushed::Queued);
        }
        assert_eq!(
            outbox.push_notification(Notification::DownloadRemoved(RemovedEvent {
                ids: vec!["a".into()]
            })),
            Pushed::Overflow
        );
        assert!(outbox.is_closing());
        assert_eq!(outbox.push_notification(state("a")), Pushed::Closed);
        assert_eq!(outbox.push_response("x".into(), None), Pushed::Closed);
        assert_eq!(drain(&outbox), ["close:4429"]);
    }

    #[test]
    fn responses_are_barriers_but_do_not_count_as_backlog() {
        let outbox = Outbox::new(1);
        outbox.push_notification(progress("a", 1));
        outbox.push_response("r1".into(), None);
        outbox.push_response("r2".into(), None);
        assert_eq!(outbox.push_notification(progress("a", 2)), Pushed::Queued);
        assert_eq!(outbox.push_notification(state("a")), Pushed::Queued);
        assert_eq!(
            drain(&outbox),
            [
                "progress:a=1",
                "resp:r1",
                "resp:r2",
                "progress:a=2",
                r#"event.download.state:{"id":"a","state":"paused"}"#,
            ]
        );
    }
}
