use serde::{Deserialize, Serialize};

/// A half-open byte range `[start, end)` within a remote file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Creates a range. Returns `None` when `end < start`.
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (end >= start).then_some(Self { start, end })
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Splits the range at `at` (absolute offset). Returns `None` unless
    /// `start < at < end`, so both halves are non-empty.
    pub fn split_at(&self, at: u64) -> Option<(Self, Self)> {
        (self.start < at && at < self.end).then_some((
            Self {
                start: self.start,
                end: at,
            },
            Self {
                start: at,
                end: self.end,
            },
        ))
    }

    /// The value for an HTTP `Range` header (`bytes=start-end`, inclusive end).
    /// Returns `None` for an empty range, which HTTP cannot express.
    pub fn to_http_header(&self) -> Option<String> {
        (!self.is_empty()).then(|| format!("bytes={}-{}", self.start, self.end - 1))
    }
}

/// A set of byte offsets kept as sorted, non-overlapping, non-adjacent
/// ranges. Used for "what is done" in the journal and the scheduler.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RangeSet(Vec<ByteRange>);

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a set from ranges in any order, overlapping or not.
    pub fn from_ranges(ranges: impl IntoIterator<Item = ByteRange>) -> Self {
        let mut s = Self::new();
        for r in ranges {
            s.insert(r);
        }
        s
    }

    pub fn ranges(&self) -> &[ByteRange] {
        &self.0
    }

    pub fn into_ranges(self) -> Vec<ByteRange> {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Total number of bytes covered.
    pub fn len(&self) -> u64 {
        self.0.iter().map(ByteRange::len).sum()
    }

    /// Adds `r`, merging with overlapping or touching ranges.
    pub fn insert(&mut self, r: ByteRange) {
        if r.is_empty() {
            return;
        }
        // First range whose end reaches r.start (touching counts).
        let lo = self.0.partition_point(|x| x.end < r.start);
        // First range that starts after r.end (touching counts as merge).
        let hi = self.0.partition_point(|x| x.start <= r.end);
        let mut merged = r;
        if lo < hi {
            merged.start = merged.start.min(self.0[lo].start);
            merged.end = merged.end.max(self.0[hi - 1].end);
        }
        self.0.splice(lo..hi, [merged]);
    }

    /// Removes every offset in `r`.
    pub fn remove(&mut self, r: ByteRange) {
        if r.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(self.0.len() + 1);
        for x in self.0.drain(..) {
            if x.end <= r.start || x.start >= r.end {
                out.push(x);
                continue;
            }
            if x.start < r.start {
                out.push(ByteRange {
                    start: x.start,
                    end: r.start,
                });
            }
            if x.end > r.end {
                out.push(ByteRange {
                    start: r.end,
                    end: x.end,
                });
            }
        }
        self.0 = out;
    }

    /// Whether every offset in `r` is in the set.
    pub fn contains(&self, r: ByteRange) -> bool {
        if r.is_empty() {
            return true;
        }
        let i = self.0.partition_point(|x| x.end <= r.start);
        self.0
            .get(i)
            .is_some_and(|x| x.start <= r.start && r.end <= x.end)
    }

    /// The parts of `[0, total)` not in the set, in order.
    pub fn gaps(&self, total: u64) -> Vec<ByteRange> {
        let mut out = Vec::new();
        let mut at = 0;
        for x in &self.0 {
            if x.start >= total {
                break;
            }
            if x.start > at {
                out.push(ByteRange {
                    start: at,
                    end: x.start,
                });
            }
            at = at.max(x.end);
        }
        if at < total {
            out.push(ByteRange {
                start: at,
                end: total,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: u64, b: u64) -> ByteRange {
        ByteRange::new(a, b).unwrap()
    }

    #[test]
    fn set_merges_overlapping_and_touching() {
        let mut s = RangeSet::new();
        s.insert(r(10, 20));
        s.insert(r(30, 40));
        s.insert(r(20, 25)); // touches 10..20
        assert_eq!(s.ranges(), &[r(10, 25), r(30, 40)]);
        s.insert(r(5, 35)); // swallows both
        assert_eq!(s.ranges(), &[r(5, 40)]);
        s.insert(r(50, 50)); // empty: ignored
        assert_eq!(s.len(), 35);
    }

    #[test]
    fn set_remove_contains_gaps() {
        let mut s = RangeSet::from_ranges([r(0, 100)]);
        s.remove(r(40, 60));
        assert_eq!(s.ranges(), &[r(0, 40), r(60, 100)]);
        assert!(s.contains(r(0, 40)));
        assert!(!s.contains(r(30, 70)));
        assert!(s.contains(r(70, 70)));
        assert_eq!(s.gaps(120), vec![r(40, 60), r(100, 120)]);
        assert_eq!(RangeSet::new().gaps(5), vec![r(0, 5)]);
    }

    #[test]
    fn set_matches_a_bitmap_model() {
        // Deterministic pseudo-random operations checked against a bitmap.
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        let mut s = RangeSet::new();
        let mut bits = [false; 256];
        for _ in 0..2000 {
            let a = next(256);
            let b = (a + next(40)).min(256);
            if next(3) == 0 {
                s.remove(r(a, b));
                bits[a as usize..b as usize].fill(false);
            } else {
                s.insert(r(a, b));
                bits[a as usize..b as usize].fill(true);
            }
            let model = RangeSet::from_ranges(
                (0..256u64)
                    .filter(|&i| bits[i as usize])
                    .map(|i| r(i, i + 1)),
            );
            assert_eq!(s, model);
        }
    }

    #[test]
    fn split_keeps_both_halves_non_empty() {
        let r = ByteRange::new(0, 10).unwrap();
        assert_eq!(r.split_at(0), None);
        assert_eq!(r.split_at(10), None);
        let (a, b) = r.split_at(4).unwrap();
        assert_eq!((a.len(), b.len()), (4, 6));
    }

    #[test]
    fn http_header_uses_inclusive_end() {
        assert_eq!(
            ByteRange::new(0, 100).unwrap().to_http_header().as_deref(),
            Some("bytes=0-99")
        );
        assert_eq!(ByteRange::new(5, 5).unwrap().to_http_header(), None);
    }

    #[test]
    fn rejects_inverted_range() {
        assert!(ByteRange::new(10, 9).is_none());
    }
}
