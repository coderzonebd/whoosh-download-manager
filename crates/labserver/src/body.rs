//! The streaming response body: content from the block cache, paced by
//! latency and bandwidth caps, interrupted by injected stalls and resets.

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use crate::bucket::TokenBucket;
use crate::conn::ConnCtl;
use crate::content::{BLOCK_SIZE, mix64};
use crate::handler::Body;
use crate::state::{ActiveGuard, FileRt, ScenarioRt, Shared};
use crate::stats::Stats;

const MIB: u64 = 1 << 20;

/// Pause between reaching a reset point and sending the RST.
const RESET_DRAIN: Duration = Duration::from_millis(20);

/// Everything needed to stream `[start, end)` of one file version.
#[derive(Debug)]
pub(crate) struct BodyPlan {
    pub shared: Arc<Shared>,
    pub rt: Arc<ScenarioRt>,
    pub file: Arc<FileRt>,
    pub seed: u64,
    pub start: u64,
    pub end: u64,
    pub conn: Arc<ConnCtl>,
    pub guard: Option<ActiveGuard>,
}

/// Small deterministic RNG (SplitMix64).
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix64(self.0)
    }
    /// Uniform in `(0, 1)`.
    fn unit_open(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    /// Bytes until the next event that happens with probability `p` per MiB.
    fn gap(&mut self, p: f64) -> Option<u64> {
        if p <= 0.0 {
            return None;
        }
        let whole_mib = if p >= 1.0 {
            0
        } else {
            (self.unit_open().ln() / (1.0 - p).ln()).floor() as u64
        };
        Some(
            whole_mib
                .saturating_mul(MIB)
                .saturating_add(self.below(MIB)),
        )
    }
}

#[derive(Debug)]
struct St {
    plan: BodyPlan,
    pos: u64,
    sent: u64,
    chunk: u64,
    chunk_delay: Option<Duration>,
    conn_bucket: Option<Arc<TokenBucket>>,
    global_bucket: Option<Arc<TokenBucket>>,
    reset_at: Option<u64>,
    stall_at: Option<u64>,
    rng: Rng,
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

impl St {
    fn new(plan: BodyPlan) -> Self {
        let sc = &plan.rt.sc;
        let mut rng = Rng(plan.rt.next_request_seed());
        let conn_bucket = sc
            .bandwidth
            .per_conn
            .map(|r| plan.conn.bucket(plan.rt.idx, r.0));
        let global_bucket = plan.rt.global_bucket.clone();
        let mut chunk = sc.latency.chunk_size.0.min(BLOCK_SIZE as u64);
        if let Some(rate) = [&conn_bucket, &global_bucket]
            .into_iter()
            .flatten()
            .map(|b| b.rate())
            .min()
        {
            chunk = chunk.min(TokenBucket::chunk_hint(rate) as u64);
        }
        let reset_at = sc
            .resets
            .and_then(|r| min_opt(r.after_bytes.map(|s| s.0), rng.gap(r.prob_per_mb)));
        let stall_at = sc
            .stalls
            .and_then(|s| min_opt(s.after_bytes.map(|s| s.0), rng.gap(s.prob_per_mb)));
        Self {
            pos: plan.start,
            sent: 0,
            chunk: chunk.max(1),
            chunk_delay: (sc.latency.chunk_ms > 0)
                .then(|| Duration::from_millis(sc.latency.chunk_ms)),
            conn_bucket,
            global_bucket,
            reset_at,
            stall_at,
            rng,
            plan,
        }
    }

    async fn next_chunk(&mut self) -> Option<Bytes> {
        if self.pos >= self.plan.end {
            self.plan.guard = None;
            return None;
        }
        let rt = self.plan.rt.clone();
        if let Some(stall) = rt.sc.stalls
            && self.stall_at.is_some_and(|at| self.sent >= at)
        {
            // At most one `after_bytes` stall per response; random ones recur.
            self.stall_at = self.rng.gap(stall.prob_per_mb).map(|g| self.sent + g + 1);
            if Stats::try_inject(&rt.stats.stalls, stall.max) {
                tokio::time::sleep(Duration::from_millis(stall.duration_ms)).await;
            }
        }
        if let Some(reset) = rt.sc.resets
            && self.reset_at.is_some_and(|at| self.sent >= at)
        {
            self.reset_at = None;
            if Stats::try_inject(&rt.stats.resets, reset.max) {
                // Let hyper flush what it has buffered (head included), so
                // the client gets the bytes up to the reset point unless its
                // own receive window is full.
                tokio::time::sleep(RESET_DRAIN).await;
                self.plan.conn.kill();
                // The connection task drops this body along with the socket.
                std::future::pending::<()>().await;
            }
        }

        let mut len = (self.plan.end - self.pos)
            .min(self.chunk)
            .min(BLOCK_SIZE as u64 - self.pos % BLOCK_SIZE as u64);
        for at in [self.reset_at, self.stall_at].into_iter().flatten() {
            if at > self.sent {
                len = len.min(at - self.sent);
            }
        }

        if let Some(d) = self.chunk_delay {
            tokio::time::sleep(d).await;
        }
        let ready = [&self.conn_bucket, &self.global_bucket]
            .into_iter()
            .flatten()
            .map(|b| b.reserve(len))
            .max();
        if let Some(t) = ready
            && t > Instant::now()
        {
            tokio::time::sleep_until(t.into()).await;
        }

        let block = self
            .plan
            .shared
            .cache
            .get(self.plan.seed, self.pos / BLOCK_SIZE as u64);
        let off = (self.pos % BLOCK_SIZE as u64) as usize;
        let data = block.slice(off..off + len as usize);
        self.pos += len;
        self.sent += len;
        rt.stats.bytes_sent.fetch_add(len, Relaxed);
        self.plan.file.served.fetch_add(len, Relaxed);
        if self.pos >= self.plan.end {
            // Free the client's slot before the last byte leaves, so a client
            // that starts its next request right away is not refused.
            self.plan.guard = None;
        }
        Some(data)
    }
}

/// A body streaming the plan's range with the scenario's faults.
pub(crate) fn stream_body(plan: BodyPlan) -> Body {
    let st = St::new(plan);
    let s = stream::unfold(st, |mut st| async move {
        let data = st.next_chunk().await?;
        Some((Ok::<_, io::Error>(Frame::data(data)), st))
    });
    StreamBody::new(s).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_distribution_is_roughly_geometric() {
        let mut r = Rng(7);
        assert_eq!(r.gap(0.0), None);
        assert!(r.gap(1.0).unwrap() < MIB);
        let n = 20_000;
        let mean = (0..n).map(|_| r.gap(0.1).unwrap()).sum::<u64>() as f64 / n as f64 / MIB as f64;
        // E[whole MiB] = (1-p)/p = 9, plus 0.5 for the offset within the MiB.
        assert!((9.0..10.0).contains(&mean), "mean {mean} MiB");
    }
}
