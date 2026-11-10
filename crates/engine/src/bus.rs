//! Fan-out of notifications to independent subscribers.
//!
//! Every `subscribe()` gets its own queue. Non-lossy notifications
//! (`added`, `state`, `removed`, `settings`) are always queued. Lossy ones
//! (`progress`, `stats`) are queued as they come while the subscriber keeps
//! up; once it falls behind they are coalesced into the newest pending one
//! of the same kind at the back of the queue, and dropped once the backlog
//! is very long. Order is never changed.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use futures::Stream;
use vayu_proto::service::Notification;
use vayu_proto::{ProgressEvent, ProgressItem};

/// Pending events after which lossy ones are coalesced.
const COALESCE_AT: usize = 8;
/// Pending events after which lossy ones are dropped outright.
const DROP_LOSSY_AT: usize = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Default)]
pub(crate) struct Bus {
    subs: Mutex<Vec<Arc<Shared>>>,
    closed: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Default)]
struct Shared {
    q: Mutex<Queue>,
}

#[derive(Debug, Default)]
struct Queue {
    items: VecDeque<Notification>,
    waker: Option<Waker>,
    closed: bool,
    gone: bool,
}

impl Bus {
    pub(crate) fn subscribe(&self) -> Subscription {
        let s = Arc::new(Shared::default());
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            lock(&s.q).closed = true;
        } else {
            lock(&self.subs).push(s.clone());
        }
        Subscription(s)
    }

    pub(crate) fn emit(&self, n: Notification) {
        let mut subs = lock(&self.subs);
        subs.retain(|s| !lock(&s.q).gone);
        for s in subs.iter() {
            let mut q = lock(&s.q);
            push(&mut q.items, n.clone());
            if let Some(w) = q.waker.take() {
                w.wake();
            }
        }
    }

    /// Ends every subscription (after the queued notifications).
    pub(crate) fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        for s in lock(&self.subs).drain(..) {
            let mut q = lock(&s.q);
            q.closed = true;
            if let Some(w) = q.waker.take() {
                w.wake();
            }
        }
    }

    #[cfg(test)]
    fn subscribers(&self) -> usize {
        let mut subs = lock(&self.subs);
        subs.retain(|s| !lock(&s.q).gone);
        subs.len()
    }
}

fn push(items: &mut VecDeque<Notification>, n: Notification) {
    if !n.is_lossy() || items.len() < COALESCE_AT {
        items.push_back(n);
        return;
    }
    let n = match (items.back_mut(), n) {
        (Some(Notification::Stats(old)), Notification::Stats(new)) => {
            *old = new;
            return;
        }
        (Some(Notification::DownloadProgress(old)), Notification::DownloadProgress(new)) => {
            merge_progress(old, new);
            return;
        }
        (_, n) => n,
    };
    if items.len() < DROP_LOSSY_AT {
        items.push_back(n);
    }
}

/// Newer items replace older ones for the same download; segments survive
/// from the older item when the newer one has none.
fn merge_progress(old: &mut ProgressEvent, new: ProgressEvent) {
    for item in new.items {
        match old.items.iter_mut().find(|o| o.id == item.id) {
            Some(o) => {
                let segments = item.segments.clone().or_else(|| o.segments.take());
                *o = ProgressItem { segments, ..item };
            }
            None => old.items.push(item),
        }
    }
}

/// One subscriber's stream of notifications.
#[derive(Debug)]
pub(crate) struct Subscription(Arc<Shared>);

impl Stream for Subscription {
    type Item = Notification;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Notification>> {
        let mut q = lock(&self.0.q);
        if let Some(n) = q.items.pop_front() {
            return Poll::Ready(Some(n));
        }
        if q.closed {
            return Poll::Ready(None);
        }
        q.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut q = lock(&self.0.q);
        q.gone = true;
        q.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use vayu_proto::{DownloadState, StateEvent, Stats};

    fn state(i: u32) -> Notification {
        Notification::DownloadState(StateEvent {
            id: format!("d{i}"),
            state: DownloadState::Downloading,
            error: None,
        })
    }

    fn progress(id: &str, bytes: u64, segs: bool) -> Notification {
        Notification::DownloadProgress(ProgressEvent {
            items: vec![ProgressItem {
                id: id.into(),
                downloaded_bytes: bytes,
                total_bytes: None,
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                segments: segs.then(|| vec![(0, 1, 1)]),
            }],
        })
    }

    #[tokio::test]
    async fn subscribers_are_independent_and_lossless() {
        let bus = Bus::default();
        let mut a = bus.subscribe();
        let mut slow = bus.subscribe();
        for i in 0..1000 {
            bus.emit(state(i));
            bus.emit(Notification::Stats(Stats::default()));
        }
        // `a` reads everything as it comes.
        let mut seen = 0;
        for _ in 0..2000 {
            if let Notification::DownloadState(_) = a.next().await.unwrap() {
                seen += 1;
            }
        }
        assert_eq!(seen, 1000);
        // `slow` never read: every state event is still there, in order.
        bus.close();
        let mut states = Vec::new();
        while let Some(n) = slow.next().await {
            if let Notification::DownloadState(s) = n {
                states.push(s.id);
            }
        }
        let want: Vec<String> = (0..1000).map(|i| format!("d{i}")).collect();
        assert_eq!(states, want);
        assert!(a.next().await.is_none());
    }

    #[tokio::test]
    async fn lossy_events_coalesce_for_a_slow_subscriber() {
        let bus = Bus::default();
        let mut s = bus.subscribe();
        for i in 0..100 {
            bus.emit(progress("x", i, i == 10));
        }
        bus.close();
        let all: Vec<Notification> = s.by_ref().collect().await;
        assert!(all.len() <= COALESCE_AT, "{}", all.len());
        let Notification::DownloadProgress(last) = all.last().unwrap() else {
            panic!()
        };
        assert_eq!(last.items[0].downloaded_bytes, 99);
        assert!(last.items[0].segments.is_some(), "segments kept");
    }

    #[test]
    fn dropped_subscriptions_are_forgotten() {
        let bus = Bus::default();
        let a = bus.subscribe();
        let _b = bus.subscribe();
        drop(a);
        bus.emit(state(1));
        assert_eq!(bus.subscribers(), 1);
    }
}
