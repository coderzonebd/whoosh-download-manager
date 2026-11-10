//! Connections kept for reuse: idle HTTP/1.1 connections (one request at a
//! time) and HTTP/2 / HTTP/3 connections shared by up to
//! `max_streams_per_connection` requests, spread least-loaded first.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::task::AbortHandle;

use crate::h3guard::Violation;
use crate::lock;
use crate::origin::Origin;

pub(crate) type ReqBody = http_body_util::Full<Bytes>;
pub(crate) type H1Sender = hyper::client::conn::http1::SendRequest<ReqBody>;
pub(crate) type H2Sender = hyper::client::conn::http2::SendRequest<ReqBody>;
pub(crate) type H3Sender = h3::client::SendRequest<crate::h3guard::GuardedOpen, Bytes>;

/// RFC 9114 §8.1: closing an HTTP/3 connection without an error.
const H3_NO_ERROR: u32 = 0x100;

/// Idle HTTP/1.1 connections kept per origin, at most.
const MAX_IDLE_H1: usize = 64;

/// An HTTP/1.1 connection, used by one request at a time. Dropping it
/// closes the socket.
pub(crate) struct H1Conn {
    pub sender: H1Sender,
    task: AbortHandle,
    /// Came from the idle pool (so it may have gone stale meanwhile).
    pub reused: bool,
    /// The pool's count of HTTP/1.1 connections alive (for `PoolStats`).
    live: Arc<AtomicUsize>,
}

impl H1Conn {
    pub fn new(sender: H1Sender, task: AbortHandle, pool: &Pool) -> Self {
        pool.h1_live.fetch_add(1, Ordering::AcqRel);
        Self {
            sender,
            task,
            reused: false,
            live: pool.h1_live.clone(),
        }
    }
}

impl Drop for H1Conn {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::AcqRel);
        self.task.abort();
    }
}

/// What a factory's connection pool holds right now, over all origins
/// ([`HttpSourceFactory::pool_stats`](crate::HttpSourceFactory::pool_stats)).
/// For diagnostics and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// HTTP/1.1 connections carrying a request.
    pub http1_busy: usize,
    /// HTTP/1.1 connections idle in the pool and still open.
    pub http1_idle: usize,
    /// Open HTTP/2 connections.
    pub http2: usize,
    /// Open HTTP/3 (QUIC) connections.
    pub http3: usize,
    /// Requests in flight on HTTP/2 and HTTP/3 connections.
    pub streams: usize,
}

struct IdleH1 {
    conn: H1Conn,
    since: Instant,
}

/// A multiplexed (HTTP/2 or HTTP/3) connection.
pub(crate) struct Mux<S> {
    pub sender: S,
    in_flight: AtomicUsize,
    last_used: Mutex<Instant>,
    /// Set by the connection task when the connection ends.
    pub closed: Arc<AtomicBool>,
    /// A request on it failed: hand it out no more (the server may be
    /// closing it; on HTTP/3 a stream reset can arrive just before the
    /// connection close). Requests already on it continue.
    retired: AtomicBool,
    task: AbortHandle,
    quic: Option<quinn::Connection>,
    /// HTTP/3: set when the server broke a limit (see `h3guard`).
    violation: Option<Violation>,
}

impl<S> Mux<S> {
    pub fn new(
        sender: S,
        closed: Arc<AtomicBool>,
        task: AbortHandle,
        quic: Option<quinn::Connection>,
        violation: Option<Violation>,
    ) -> Arc<Self> {
        Arc::new(Self {
            sender,
            in_flight: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
            closed,
            retired: AtomicBool::new(false),
            task,
            quic,
            violation,
        })
    }

    /// Why the server's connection was cut off, if it broke a limit.
    pub fn violation(&self) -> Option<&str> {
        self.violation.as_ref()?.get().map(String::as_str)
    }

    /// The transport is gone (a retired connection is still open).
    fn transport_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            || self
                .quic
                .as_ref()
                .is_some_and(|c| c.close_reason().is_some())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            || self.retired.load(Ordering::Acquire)
            || self
                .quic
                .as_ref()
                .is_some_and(|c| c.close_reason().is_some())
    }

    fn idle_for(&self) -> Option<Duration> {
        (self.in_flight.load(Ordering::Acquire) == 0).then(|| lock(&self.last_used).elapsed())
    }

    fn lease(self: &Arc<Self>) -> MuxLease<S> {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        MuxLease { conn: self.clone() }
    }
}

impl<S> Drop for Mux<S> {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(c) = &self.quic {
            c.close(H3_NO_ERROR.into(), b"");
        }
    }
}

/// One request's share of a multiplexed connection.
pub(crate) struct MuxLease<S> {
    pub conn: Arc<Mux<S>>,
}

impl<S> MuxLease<S> {
    /// The request failed: do not give this connection to new requests.
    pub fn retire(&self) {
        self.conn.retired.store(true, Ordering::Release);
    }
}

impl<S> Drop for MuxLease<S> {
    fn drop(&mut self) {
        *lock(&self.conn.last_used) = Instant::now();
        self.conn.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct Conns {
    h1: Vec<IdleH1>,
    h2: Vec<Arc<Mux<H2Sender>>>,
    h3: Vec<Arc<Mux<H3Sender>>>,
}

impl Conns {
    fn is_empty(&self) -> bool {
        self.h1.is_empty() && self.h2.is_empty() && self.h3.is_empty()
    }
}

pub(crate) struct Pool {
    h1_live: Arc<AtomicUsize>,
    origins: Mutex<HashMap<Origin, Conns>>,
    max_streams: usize,
    idle_timeout: Duration,
    reaper: AtomicBool,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("origins", &lock(&self.origins).len())
            .finish_non_exhaustive()
    }
}

/// Picks the least-loaded open connection with room for one more stream.
fn pick<S>(list: &mut Vec<Arc<Mux<S>>>, max: usize) -> Option<MuxLease<S>> {
    list.retain(|m| !m.is_closed());
    list.iter()
        .map(|m| (m.in_flight.load(Ordering::Acquire), m))
        .filter(|(n, _)| *n < max)
        .min_by_key(|(n, _)| *n)
        .map(|(_, m)| m.lease())
}

impl Pool {
    pub fn new(max_streams: usize, idle_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            origins: Mutex::new(HashMap::new()),
            h1_live: Arc::new(AtomicUsize::new(0)),
            max_streams: max_streams.max(1),
            idle_timeout,
            reaper: AtomicBool::new(false),
        })
    }

    /// The most recently used idle HTTP/1.1 connection that is still open.
    pub fn take_h1(&self, o: &Origin) -> Option<H1Conn> {
        let mut map = lock(&self.origins);
        let conns = map.get_mut(o)?;
        while let Some(idle) = conns.h1.pop() {
            if idle.since.elapsed() < self.idle_timeout && !idle.conn.sender.is_closed() {
                let mut c = idle.conn;
                c.reused = true;
                return Some(c);
            }
        }
        None
    }

    /// Parks an HTTP/1.1 connection whose last response was read to the end.
    pub fn put_h1(self: &Arc<Self>, o: Origin, conn: H1Conn) {
        if conn.sender.is_closed() {
            return;
        }
        {
            let mut map = lock(&self.origins);
            let list = &mut map.entry(o).or_default().h1;
            if list.len() >= MAX_IDLE_H1 {
                list.remove(0);
            }
            list.push(IdleH1 {
                conn,
                since: Instant::now(),
            });
        }
        self.ensure_reaper();
    }

    pub fn take_h2(&self, o: &Origin) -> Option<MuxLease<H2Sender>> {
        pick(&mut lock(&self.origins).get_mut(o)?.h2, self.max_streams)
    }

    pub fn take_h3(&self, o: &Origin) -> Option<MuxLease<H3Sender>> {
        pick(&mut lock(&self.origins).get_mut(o)?.h3, self.max_streams)
    }

    /// Registers a new HTTP/2 connection; with `lease`, the caller's
    /// request is its first stream.
    pub fn add_h2(
        self: &Arc<Self>,
        o: Origin,
        m: Arc<Mux<H2Sender>>,
        lease: bool,
    ) -> Option<MuxLease<H2Sender>> {
        let l = lease.then(|| m.lease());
        lock(&self.origins).entry(o).or_default().h2.push(m);
        self.ensure_reaper();
        l
    }

    pub fn add_h3(
        self: &Arc<Self>,
        o: Origin,
        m: Arc<Mux<H3Sender>>,
        lease: bool,
    ) -> Option<MuxLease<H3Sender>> {
        let l = lease.then(|| m.lease());
        lock(&self.origins).entry(o).or_default().h3.push(m);
        self.ensure_reaper();
        l
    }

    pub fn stats(&self) -> PoolStats {
        let map = lock(&self.origins);
        let mut st = PoolStats::default();
        let mut idle_entries = 0;
        for c in map.values() {
            idle_entries += c.h1.len();
            st.http1_idle += c.h1.iter().filter(|i| !i.conn.sender.is_closed()).count();
            for m in c.h2.iter().filter(|m| !m.transport_closed()) {
                st.http2 += 1;
                st.streams += m.in_flight.load(Ordering::Acquire);
            }
            for m in c.h3.iter().filter(|m| !m.transport_closed()) {
                st.http3 += 1;
                st.streams += m.in_flight.load(Ordering::Acquire);
            }
        }
        st.http1_busy = self
            .h1_live
            .load(Ordering::Acquire)
            .saturating_sub(idle_entries);
        st
    }

    /// Connections to `o` that could take a request right now.
    pub fn warm(&self, o: &Origin) -> usize {
        let map = lock(&self.origins);
        let Some(c) = map.get(o) else { return 0 };
        let free = |n: usize| self.max_streams.saturating_sub(n).min(1);
        c.h1.len()
            + c.h2
                .iter()
                .filter(|m| !m.is_closed())
                .map(|m| free(m.in_flight.load(Ordering::Acquire)))
                .sum::<usize>()
            + c.h3
                .iter()
                .filter(|m| !m.is_closed())
                .map(|m| free(m.in_flight.load(Ordering::Acquire)))
                .sum::<usize>()
    }

    /// Drops connections that are closed or unused for `idle_timeout`.
    fn reap(&self) {
        let t = self.idle_timeout;
        fn keep<S>(m: &Arc<Mux<S>>, t: Duration) -> bool {
            !m.is_closed() && m.idle_for().is_none_or(|d| d < t)
        }
        let mut map = lock(&self.origins);
        for c in map.values_mut() {
            c.h1.retain(|i| i.since.elapsed() < t && !i.conn.sender.is_closed());
            c.h2.retain(|m| keep(m, t));
            c.h3.retain(|m| keep(m, t));
        }
        map.retain(|_, c| !c.is_empty());
    }

    /// Starts the background task that closes idle connections, once.
    fn ensure_reaper(self: &Arc<Self>) {
        if self.reaper.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.reaper.store(false, Ordering::Release);
            return;
        };
        let weak: Weak<Self> = Arc::downgrade(self);
        let every =
            (self.idle_timeout / 4).clamp(Duration::from_millis(50), Duration::from_secs(5));
        rt.spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(pool) = weak.upgrade() else { return };
                pool.reap();
            }
        });
    }
}
