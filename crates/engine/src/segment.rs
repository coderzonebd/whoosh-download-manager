//! One range of the file being downloaded, shared by the scheduler and the
//! connection(s) working on it.
//!
//! A segment covers `[start, end)`. Its owner (one connection) *claims*
//! bytes before writing them, so the scheduler can split the unclaimed tail
//! off at any moment without ever giving away a byte that is being written.
//! `written` is the contiguous prefix whose writes have resolved; the
//! journal only ever records `[start, written)`.
//!
//! Hedging: a second connection (the *challenger*) may be started on the
//! same remaining range. Ownership is decided at the claim step, under the
//! segment's lock: a challenger that delivers takes over (and cancels the
//! old owner); for a stall hedge, an owner that delivers first cancels the
//! challenger instead ("whichever delivers first wins").
//!
//! The lock is held for a few comparisons per chunk and is uncontended
//! except when the scheduler looks at the segment (every tick) or a hedge
//! races, so it stays off the data path's critical cost.

use std::sync::{Arc, Mutex, MutexGuard};

use tokio_util::sync::CancellationToken;

pub(crate) type WorkerId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HedgeKind {
    /// The owner went silent: whoever delivers next wins.
    Stall,
    /// The owner is slow: the challenger takes over as soon as it delivers.
    Slow,
}

#[derive(Debug)]
pub(crate) struct Segment {
    pub(crate) start: u64,
    st: Mutex<SegState>,
}

#[derive(Debug)]
struct SegState {
    claimed: u64,
    written: u64,
    end: u64,
    /// Size unknown: `end` is fixed when the stream ends.
    open_ended: bool,
    /// Resolved writes beyond `written` (an old owner's write still in
    /// flight after a hedge took over).
    ahead: Vec<(u64, u64)>,
    inflight: u32,
    owner: Option<(WorkerId, CancellationToken)>,
    challenger: Option<(WorkerId, HedgeKind, CancellationToken)>,
    hedges: u32,
}

/// What a connection may do with a chunk it received.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Write `chunk[from..to]` at `offset`, then call
    /// [`Segment::complete_claims`]. `last`: the claim reached the end of
    /// the range (write it out now; nothing more will come).
    Write {
        offset: u64,
        from: usize,
        to: usize,
        last: bool,
    },
    /// Bytes another connection already claimed: discard, keep reading.
    Skip,
    /// Another connection owns the range now: stop.
    Lost,
    /// Nothing left to claim (the range was split or finished): stop.
    Done,
}

/// How a stream ended, from the segment's point of view.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Eof {
    Complete,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub start: u64,
    pub claimed: u64,
    pub written: u64,
    pub end: u64,
    pub owner: Option<WorkerId>,
    pub challenger: Option<WorkerId>,
    pub hedges: u32,
}

impl Snapshot {
    /// Bytes nobody has claimed yet.
    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.claimed)
    }

    pub fn complete(&self) -> bool {
        self.written >= self.end
    }
}

impl Segment {
    pub(crate) fn new(start: u64, end: u64) -> Arc<Self> {
        Self::build(start, end, false)
    }

    /// A segment from `start` whose end is learned when the stream ends.
    pub(crate) fn open_ended(start: u64) -> Arc<Self> {
        Self::build(start, u64::MAX, true)
    }

    fn build(start: u64, end: u64, open_ended: bool) -> Arc<Self> {
        Arc::new(Self {
            start,
            st: Mutex::new(SegState {
                claimed: start,
                written: start,
                end,
                open_ended,
                ahead: Vec::with_capacity(2),
                inflight: 0,
                owner: None,
                challenger: None,
                hedges: 0,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, SegState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let s = self.lock();
        Snapshot {
            start: self.start,
            claimed: s.claimed,
            written: s.written,
            end: s.end,
            owner: s.owner.as_ref().map(|o| o.0),
            challenger: s.challenger.as_ref().map(|c| c.0),
            hedges: s.hedges,
        }
    }

    pub(crate) fn written(&self) -> u64 {
        self.lock().written
    }

    /// Make `wid` the owner of an ownerless segment. Returns the range its
    /// request must cover, or `None` if the segment has an owner already or
    /// nothing is left.
    pub(crate) fn assign_owner(
        &self,
        wid: WorkerId,
        cancel: CancellationToken,
    ) -> Option<(u64, u64)> {
        let mut s = self.lock();
        if s.owner.is_some() || s.claimed >= s.end {
            return None;
        }
        s.owner = Some((wid, cancel));
        Some((s.claimed, s.end))
    }

    /// Start a hedge on the owner's remaining range. Returns the range its
    /// request must cover, or `None` if not possible.
    pub(crate) fn assign_challenger(
        &self,
        wid: WorkerId,
        kind: HedgeKind,
        cancel: CancellationToken,
    ) -> Option<(u64, u64)> {
        let mut s = self.lock();
        if s.owner.is_none() || s.challenger.is_some() || s.claimed >= s.end || s.open_ended {
            return None;
        }
        s.challenger = Some((wid, kind, cancel));
        s.hedges += 1;
        Some((s.claimed, s.end))
    }

    /// `me` received `len` bytes starting at file offset `at`.
    pub(crate) fn claim(&self, me: WorkerId, at: u64, len: u64) -> Claim {
        let mut s = self.lock();
        let is_owner = matches!(&s.owner, Some((o, _)) if *o == me);
        if !is_owner {
            if !matches!(&s.challenger, Some((c, _, _)) if *c == me) {
                return Claim::Lost;
            }
            // The challenger delivered first: it takes over.
            let (_, _, token) = s.challenger.take().expect("checked above");
            if let Some((_, old)) = s.owner.replace((me, token)) {
                old.cancel();
            }
        } else if matches!(s.challenger, Some((_, HedgeKind::Stall, _))) {
            // The stalled owner came back before the hedge delivered.
            if let Some((_, _, token)) = s.challenger.take() {
                token.cancel();
            }
        }
        if s.claimed >= s.end {
            return Claim::Done;
        }
        let chunk_end = at + len;
        if chunk_end <= s.claimed {
            return Claim::Skip;
        }
        if at > s.claimed {
            // A hole would open; impossible by construction (an owner's
            // stream is contiguous from `claimed`).
            debug_assert!(false, "claim at {at} beyond claimed {}", s.claimed);
            return Claim::Lost;
        }
        let from = s.claimed;
        let to = chunk_end.min(s.end);
        s.claimed = to;
        s.inflight += 1;
        Claim::Write {
            offset: from,
            from: (from - at) as usize,
            to: (to - at) as usize,
            last: !s.open_ended && to >= s.end,
        }
    }

    /// The write of `[from, to)` resolved. Returns true once the whole
    /// segment is written.
    #[cfg(test)]
    pub(crate) fn complete(&self, from: u64, to: u64) -> bool {
        self.complete_claims(from, to, 1)
    }

    /// One write covering `claims` consecutive claims (`[from, to)`)
    /// resolved. Returns true once the whole segment is written.
    pub(crate) fn complete_claims(&self, from: u64, to: u64, claims: u32) -> bool {
        let mut g = self.lock();
        let s = &mut *g;
        s.inflight = s.inflight.saturating_sub(claims);
        if from <= s.written {
            s.written = s.written.max(to);
            loop {
                let before = s.written;
                let mut w = s.written;
                s.ahead.retain(|&(a, b)| {
                    if a <= w {
                        w = w.max(b);
                        false
                    } else {
                        true
                    }
                });
                s.written = w;
                if s.written == before {
                    break;
                }
            }
        } else {
            s.ahead.push((from, to));
        }
        !s.open_ended && s.written >= s.end
    }

    /// A write covering `claims` claims failed.
    pub(crate) fn abort_writes(&self, claims: u32) {
        let mut s = self.lock();
        s.inflight = s.inflight.saturating_sub(claims);
    }

    /// The stream of `me` ended after delivering up to file offset `pos`.
    pub(crate) fn on_eof(&self, pos: u64) -> Eof {
        let mut s = self.lock();
        if s.open_ended {
            // Only the owner's end of stream ends an open-ended segment.
            if s.claimed == pos {
                s.end = s.claimed;
                s.open_ended = false;
                return Eof::Complete;
            }
            return Eof::Short;
        }
        if s.claimed >= s.end {
            Eof::Complete
        } else {
            Eof::Short
        }
    }

    /// A connection left (finished, failed, cancelled or lost). If it owned
    /// the segment, the segment becomes ownerless and its unwritten rest
    /// can be picked up again.
    pub(crate) fn release(&self, wid: WorkerId) {
        let mut s = self.lock();
        if matches!(&s.owner, Some((o, _)) if *o == wid) {
            s.owner = None;
            if s.inflight == 0 {
                s.claimed = s.written;
            }
        }
        if matches!(&s.challenger, Some((c, _, _)) if *c == wid) {
            s.challenger = None;
        }
    }

    /// Split off the unclaimed tail for another connection. The current
    /// owner keeps `keep_frac` of the remaining bytes (both pieces at least
    /// `min`); the split point is rounded down to `align` when that still
    /// leaves `min` bytes. Hedged and open-ended segments are not split.
    pub(crate) fn split(&self, min: u64, keep_frac: f64, align: u64) -> Option<Arc<Segment>> {
        let mut s = self.lock();
        if s.open_ended || s.challenger.is_some() {
            return None;
        }
        let rem = s.end.saturating_sub(s.claimed);
        if min == 0 || rem < 2 * min {
            return None;
        }
        let keep = ((rem as f64 * keep_frac.clamp(0.0, 1.0)) as u64).clamp(min, rem - min);
        let mut at = s.claimed + keep;
        if align > 1 {
            let aligned = at - at % align;
            if aligned >= s.claimed + min {
                at = aligned;
            }
        }
        let tail = Segment::new(at, s.end);
        s.end = at;
        Some(tail)
    }

    #[cfg(test)]
    pub(crate) fn hedges(&self) -> u32 {
        self.lock().hedges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok() -> CancellationToken {
        CancellationToken::new()
    }

    #[test]
    fn owner_claims_and_completes() {
        let s = Segment::new(100, 300);
        assert_eq!(s.assign_owner(1, tok()), Some((100, 300)));
        assert_eq!(s.assign_owner(2, tok()), None);
        assert_eq!(
            s.claim(1, 100, 150),
            Claim::Write {
                offset: 100,
                from: 0,
                to: 150,
                last: false
            }
        );
        assert!(!s.complete(100, 250));
        // The last chunk runs past the end: cut.
        assert_eq!(
            s.claim(1, 250, 100),
            Claim::Write {
                offset: 250,
                from: 0,
                to: 50,
                last: true
            }
        );
        assert!(s.complete(250, 300));
        assert_eq!(s.claim(1, 300, 10), Claim::Done);
        assert_eq!(s.claim(9, 300, 10), Claim::Lost);
    }

    #[test]
    fn split_never_gives_away_claimed_bytes() {
        let s = Segment::new(0, 10_000);
        s.assign_owner(1, tok());
        assert!(matches!(s.claim(1, 0, 1000), Claim::Write { .. }));
        // Claimed 1000 but not yet written: the split must start after it.
        let tail = s.split(1000, 0.5, 1).unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.end, tail.start);
        assert!(tail.start >= 2000 && tail.start <= 9000);
        assert_eq!(tail.snapshot().end, 10_000);
        // Too small to split again into two ≥ 4000-byte pieces.
        assert!(s.split(4000, 0.5, 1).is_none());
    }

    #[test]
    fn split_respects_min_and_alignment() {
        let s = Segment::new(0, 10 * 1024);
        let t = s.split(1024, 0.0, 1).unwrap(); // stalled owner keeps the minimum
        assert_eq!((s.snapshot().end, t.start), (1024, 1024));
        let s = Segment::new(0, 100_000);
        let t = s.split(1000, 0.5, 4096).unwrap();
        assert_eq!(t.start % 4096, 0);
        assert!(Segment::new(0, 1999).split(1000, 0.5, 1).is_none());
    }

    #[test]
    fn challenger_takes_over_when_it_delivers_first() {
        let s = Segment::new(0, 1000);
        let owner_tok = tok();
        s.assign_owner(1, owner_tok.clone());
        assert!(matches!(s.claim(1, 0, 100), Claim::Write { .. }));
        s.complete(0, 100);
        assert_eq!(
            s.assign_challenger(2, HedgeKind::Slow, tok()),
            Some((100, 1000))
        );
        assert_eq!(s.assign_challenger(3, HedgeKind::Slow, tok()), None);
        // Slow hedge: the owner keeps going until the challenger delivers.
        assert!(matches!(s.claim(1, 100, 100), Claim::Write { .. }));
        s.complete(100, 200);
        // The challenger's stream starts at 100: the first 100 bytes are
        // duplicates; it takes over and writes from 200.
        assert_eq!(
            s.claim(2, 100, 150),
            Claim::Write {
                offset: 200,
                from: 100,
                to: 150,
                last: false
            }
        );
        assert!(owner_tok.is_cancelled());
        assert_eq!(s.claim(1, 200, 10), Claim::Lost);
        assert_eq!(s.snapshot().owner, Some(2));
        assert_eq!(s.hedges(), 1);
    }

    #[test]
    fn stalled_owner_that_returns_first_cancels_the_hedge() {
        let s = Segment::new(0, 1000);
        s.assign_owner(1, tok());
        let hedge = tok();
        s.assign_challenger(2, HedgeKind::Stall, hedge.clone());
        assert!(matches!(s.claim(1, 0, 10), Claim::Write { .. }));
        assert!(hedge.is_cancelled());
        assert_eq!(s.claim(2, 0, 10), Claim::Lost);
    }

    #[test]
    fn out_of_order_writes_after_a_takeover() {
        let s = Segment::new(0, 300);
        s.assign_owner(1, tok());
        // Owner claims [0,100) and its write is slow.
        assert!(matches!(s.claim(1, 0, 100), Claim::Write { .. }));
        s.assign_challenger(2, HedgeKind::Slow, tok());
        // Challenger (stream from 100) takes over and writes the rest first.
        assert!(matches!(
            s.claim(2, 100, 200),
            Claim::Write { offset: 100, .. }
        ));
        assert!(!s.complete(100, 300));
        assert_eq!(s.written(), 0);
        // The old write lands: everything is contiguous now.
        assert!(s.complete(0, 100));
        assert_eq!(s.written(), 300);
    }

    #[test]
    fn release_makes_the_rest_available_again() {
        let s = Segment::new(0, 1000);
        s.assign_owner(1, tok());
        assert!(matches!(s.claim(1, 0, 300), Claim::Write { .. }));
        s.complete(0, 300);
        s.release(1);
        let snap = s.snapshot();
        assert_eq!(
            (snap.owner, snap.claimed, snap.remaining()),
            (None, 300, 700)
        );
        assert_eq!(s.assign_owner(2, tok()), Some((300, 1000)));
    }

    #[test]
    fn open_ended_segment_ends_with_the_stream() {
        let s = Segment::open_ended(0);
        s.assign_owner(1, tok());
        assert!(matches!(s.claim(1, 0, 500), Claim::Write { .. }));
        assert!(!s.complete(0, 500));
        assert_eq!(s.on_eof(500), Eof::Complete);
        let snap = s.snapshot();
        assert_eq!((snap.end, snap.complete()), (500, true));
        assert!(Segment::new(0, 10).split(1, 0.5, 1).is_some());
        assert!(Segment::open_ended(0).split(1, 0.5, 1).is_none());
    }
}
