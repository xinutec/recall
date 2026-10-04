use crate::Instant;

/// A stretch of time, `start <= end`. Built only through [`Span::new`], so a
/// backwards span cannot exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Span {
    start: Instant,
    end: Instant,
}

impl Span {
    /// `None` when `end` is before `start`. A zero-length span is a point.
    pub fn new(start: Instant, end: Instant) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    pub const fn start(self) -> Instant {
        self.start
    }

    pub const fn end(self) -> Instant {
        self.end
    }

    pub const fn micros(self) -> i64 {
        self.end.micros() - self.start.micros()
    }

    /// Whether the two share any time. Touching ends do not overlap.
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// The shared stretch, if any.
    pub fn intersection(self, other: Self) -> Option<Self> {
        Self::new(self.start.max(other.start), self.end.min(other.end))
            .filter(|shared| shared.micros() > 0)
    }

    /// The smallest span containing both.
    #[must_use]
    pub fn cover(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn contains(self, at: Instant) -> bool {
        self.start <= at && at < self.end
    }
}
