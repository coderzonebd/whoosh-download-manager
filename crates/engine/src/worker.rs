//! One connection: one request for one range, pumped into storage.
//!
//! The data path per chunk is: one atomic add for the byte counter, one
//! clock read, an optional speed-limit reservation, the segment claim (a
//! short uncontended lock), a zero-copy `Bytes` slice and the storage
//! write. No allocation of our own; the `BoxFuture` from `write_at` is the
//! storage contract's.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
}

pub(crate) async fn work(job: Job) -> (WorkerId, Exit) {
    let wid = job.wid;
    (wid, pump(job).await)
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
    loop {
        // The stream is polled first: when data is ready, the cancellation
        // future is never registered.
        let next = tokio::select! {
            biased;
            c = stream.next() => c,
            _ = cancel.cancelled() => return Exit::Cancelled,
        };
        let chunk = match next {
            Some(Ok(c)) => c,
            Some(Err(err)) => return Exit::Failed { err, streamed },
            None => {
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
            return Exit::Cancelled;
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
                _ = cancel.cancelled() => return Exit::Cancelled,
                _ = limiter.acquire(len) => {}
            }
        }
        match seg.claim(wid, pos, len) {
            Claim::Write { offset, from, to } => {
                let n = (to - from) as u64;
                let data = if from == 0 && to == chunk.len() {
                    chunk
                } else {
                    chunk.slice(from..to)
                };
                // Never cancelled mid-write: the claim is settled either way.
                if let Err(e) = storage.write_at(offset, data).await {
                    seg.abort_write();
                    return Exit::Storage(e);
                }
                if seg.complete(offset, offset + n) {
                    return Exit::Finished;
                }
            }
            Claim::Skip => {}
            Claim::Lost => return Exit::Lost,
            Claim::Done => return Exit::Finished,
        }
        pos += len;
        tokio::task::coop::consume_budget().await;
    }
}
