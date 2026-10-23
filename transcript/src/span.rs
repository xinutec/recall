use crate::Instant;

/// A stretch of time, `start <= end`. Built only through [`Span::new`], so a
/// backwards span cannot exist.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "Stored")]
pub struct Span {
    start: Instant,
    end: Instant,
}

/// A span as stored, before [`Span::new`] checks its order.
#[derive(serde::Deserialize)]
struct Stored {
    start: Instant,
    end: Instant,
}

impl TryFrom<Stored> for Span {
    type Error = &'static str;

    fn try_from(stored: Stored) -> Result<Self, Self::Error> {
        Self::new(stored.start, stored.end).ok_or("span ends before it starts")
    }
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

    /// Microseconds the two share; zero when they only touch or are apart.
    pub fn shared_micros(self, other: Self) -> i64 {
        (self.end.min(other.end).micros() - self.start.max(other.start).micros()).max(0)
    }

    /// The span `micros` later (earlier when negative); `None` past the
    /// representable range.
    #[must_use]
    pub fn shifted(self, micros: i64) -> Option<Self> {
        let at = |i: Instant| Instant::from_micros(i.micros().checked_add(micros)?);
        Some(Self {
            start: at(self.start)?,
            end: at(self.end)?,
        })
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
