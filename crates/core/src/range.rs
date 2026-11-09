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

#[cfg(test)]
mod tests {
    use super::*;

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
