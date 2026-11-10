//! One connection: one request for one range, pumped into storage.
//!
//! The data path per chunk is: one atomic add for the byte counter, one
//! clock read, an optional speed-limit reservation, the segment claim (a
//! short uncontended lock) and a copy into the connection's gather buffer.
//! Gathered bytes go to storage in one `write_at` per `write_coalesce`
//! (256 KiB) — or sooner: once the oldest of them has waited
//! `write_coalesce_delay` (a timer, so a connection that goes quiet still
//! writes them), at the end of the range, and before the connection leaves.
//! A chunk at least `write_coalesce` long is written as is, without a copy.
//! The only allocations are one gather buffer per write (reused once
//! storage dropped the previous one) and the `BoxFuture` the `write_at`
//! contract returns.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use futures::StreamExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use vayu_core::{OpenRequest, RangeSource, SourceError, Storage};

use crate::limiter::RateLimiter;
use crate::segment::{Claim, Eof, Segment, WorkerId};
use crate::util::micros_since;

/// Counters the scheduler samples every tick.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    /// Bytes received, including duplicates a hedge discards.
    pub bytes: AtomicU64,
    pub chunks: AtomicU64,
    /// When the request's headers were accepted (µs since the run's epoch).
    pub opened_us: AtomicU64,
    /// When the last chunk arrived (µs since the run's epoch); 0 = none yet.
    pub last_us: AtomicU64,
}

/// Why a connection stopped.
#[derive(Debug)]
pub(crate) enum Exit {
    /// Its range is complete (or was split away to nothing).
    Finished,
    /// A hedge took the range over, or the hedge lost.
    Lost,
    /// The scheduler cancelled it.
    Cancelled,
    Failed {
        err: SourceError,
        /// Bytes had arrived on this request before it failed.
        streamed: bool,
    },
    Storage(io::Error),
}

pub(crate) struct Job {
    pub wid: WorkerId,
    pub seg: Arc<Segment>,
    pub source: Arc<dyn RangeSource>,
    pub storage: Arc<dyn Storage>,
    pub req: OpenRequest,
    pub counters: Arc<Counters>,
    pub cancel: CancellationToken,
    pub limiter: Arc<RateLimiter>,
    pub epoch: Instant,
    /// Gather up to this many bytes per `write_at` (0 = no gathering).
    pub coalesce: usize,
    pub coalesce_delay: Duration,
}

pub(crate) async fn work(job: Job) -> (WorkerId, Exit) {
    let wid = job.wid;
    (wid, pump(job).await)
}

/// Claimed bytes gathered for one `write_at`. They count as written only
/// once that write resolves; until then they are claimed and in flight, so
/// nobody else fetches them. They are always written before the connection
/// exits (finished, failed, cancelled or lost), so a range is never handed
/// over with a hole. If the task is aborted instead (the whole run is being
/// stopped) they are simply never journalled and are fetched again.
struct Gather {
    buf: BytesMut,
    /// File offset of `buf[0]`.
    offset: u64,
    /// Claims covered by `buf` (each one is `inflight` in the segment).
    claims: u32,
    /// When the oldest gathered byte arrived.
    since: Option<Instant>,
}

impl Gather {
    fn new(cap: usize) -> Self {
        Self {
            buf: BytesMut::with_capacity(cap),
            offset: 0,
            claims: 0,
            since: None,
        }
    }

    fn end(&self) -> u64 {
        self.offset + self.buf.len() as u64
    }

    /// Write out what is gathered. Returns whether the segment is now
    /// completely written.
    async fn flush(&mut self, seg: &Segment, storage: &dyn Storage) -> io::Result<bool> {
        if self.claims == 0 {
            return Ok(false);
        }
        let from = self.offset;
        let data = self.buf.split().freeze();
        let to = from + data.len() as u64;
        let claims = std::mem::take(&mut self.claims);
        self.since = None;
        self.offset = to;
        match storage.write_at(from, data).await {
            Ok(()) => Ok(seg.complete_claims(from, to, claims)),
            Err(e) => {
                seg.abort_writes(claims);
                Err(e)
            }
        }
    }
}

async fn pump(job: Job) -> Exit {
    let Job {
        wid,
        seg,
        source,
        storage,
        req,
        counters,
        cancel,
        limiter,
        epoch,
        coalesce,
        coalesce_delay,
    } = job;
    let mut pos = req.range.map_or(0, |r| r.start);
    let mut streamed = false;
    let mut stream = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Exit::Cancelled,
        r = source.open(req) => match r {
            Ok(s) => s,
            Err(err) => return Exit::Failed { err, streamed: false },
        },
    };
    counters
        .opened_us
        .store(micros_since(epoch), Ordering::Relaxed);
    let mut gather = Gather::new(0);
    // Whatever ends the connection, gathered bytes are written first.
    macro_rules! leave {
        ($exit:expr) => {{
            return match gather.flush(&seg, &*storage).await {
                Ok(_) => $exit,
                Err(e) => Exit::Storage(e),
            };
        }};
    }
    loop {
        // The stream is polled first: when data is ready, neither the
        // cancellation future nor the gather timer is registered.
        let next = loop {
            let due = gather.since.map(|t| t + coalesce_delay);
            tokio::select! {
                biased;
                c = stream.next() => break c,
                _ = cancel.cancelled() => leave!(Exit::Cancelled),
                // Gathered bytes waited long enough while the source is
                // quiet: write them now, keep waiting for data.
                _ = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                    match gather.flush(&seg, &*storage).await {
                        Ok(true) => return Exit::Finished,
                        Ok(false) => {}
                        Err(e) => return Exit::Storage(e),
                    }
                }
            }
        };
        let chunk = match next {
            Some(Ok(c)) => c,
            Some(Err(err)) => leave!(Exit::Failed { err, streamed }),
            None => {
                if let Err(e) = gather.flush(&seg, &*storage).await {
                    return Exit::Storage(e);
                }
                return match seg.on_eof(pos) {
                    Eof::Complete => Exit::Finished,
                    Eof::Short => Exit::Failed {
                        err: SourceError::Transient("response ended early".into()),
                        streamed: true,
                    },
                };
            }
        };
        if cancel.is_cancelled() {
            leave!(Exit::Cancelled);
        }
        if chunk.is_empty() {
            continue;
        }
        streamed = true;
        let len = chunk.len() as u64;
        counters.bytes.fetch_add(len, Ordering::Relaxed);
        counters.chunks.fetch_add(1, Ordering::Relaxed);
        counters
            .last_us
            .store(micros_since(epoch), Ordering::Relaxed);
        if limiter.limited() {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => leave!(Exit::Cancelled),
                _ = limiter.acquire(len) => {}
            }
        }
        match seg.claim(wid, pos, len) {
            Claim::Write {
                offset,
                from,
                to,
                last,
            } => {
                if gather.claims > 0 && offset != gather.end() {
                    // Not contiguous (never expected): write what we have,
                    // then handle this claim as usual. The range cannot be
                    // complete while this claim is unwritten.
                    match gather.flush(&seg, &*storage).await {
                        Ok(done) => debug_assert!(!done, "range complete before a later claim"),
                        Err(e) => {
                            seg.abort_writes(1); // this claim, not gathered yet
                            return Exit::Storage(e);
                        }
                    }
                }
                let n = to - from;
                if coalesce == 0 || (gather.claims == 0 && n >= coalesce) {
                    // Big enough on its own: no copy.
                    let data = if from == 0 && to == chunk.len() {
                        chunk
                    } else {
                        chunk.slice(from..to)
                    };
                    // Never cancelled mid-write: the claim is settled either way.
                    if let Err(e) = storage.write_at(offset, data).await {
                        seg.abort_writes(1);
                        return Exit::Storage(e);
                    }
                    if seg.complete_claims(offset, offset + n as u64, 1) {
                        return Exit::Finished;
                    }
                } else {
                    let now = Instant::now();
                    if gather.claims == 0 {
                        gather.offset = offset;
                        gather.since = Some(now);
                        // One allocation per write (the last one went to
                        // storage), never a chain of doublings and copies.
                        // It reuses the old buffer once storage dropped it.
                        gather.buf.reserve(coalesce);
                    }
                    gather.buf.extend_from_slice(&chunk[from..to]);
                    gather.claims += 1;
                    let due = last
                        || gather.buf.len() >= coalesce
                        || gather
                            .since
                            .is_some_and(|t| now.duration_since(t) >= coalesce_delay);
                    if due {
                        match gather.flush(&seg, &*storage).await {
                            Ok(true) => return Exit::Finished,
                            Ok(false) => {}
                            Err(e) => return Exit::Storage(e),
                        }
                    }
                }
            }
            Claim::Skip => {}
            Claim::Lost => leave!(Exit::Lost),
            Claim::Done => leave!(Exit::Finished),
        }
        pos += len;
        tokio::task::coop::consume_budget().await;
    }
}
