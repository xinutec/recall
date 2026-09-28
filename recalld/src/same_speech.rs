//! When two spans are the same speech: [`same_span`] for the timeline's fold
//! and the doctor's doubled-minute count, and [`COPY_SECONDS`] for the queue,
//! which knows only when a clip starts.

use chrono::{DateTime, Utc};

/// How far apart a mic's two clips of one minute can be stamped: a phone's
/// minute arrives as the Mac's `.flac` of its stream and the phone's own
/// `.wav`, cut separately; up to three seconds seen, a minute's clips about
/// sixty apart.
pub const COPY_SECONDS: i64 = 5;

/// Two spans are the same speech when they overlap by more than half of the
/// shorter one. A neighbour that only brushes a span, or a mic's next minute
/// that touches its last, is not.
pub fn same_span(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    let overlap = overlap(a, b);
    let shorter = seconds(a.0, a.1).min(seconds(b.0, b.1));
    overlap > 0.0 && overlap > shorter / 2.0
}

/// Seconds two spans share; zero when they only touch or are apart.
pub fn overlap(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> f64 {
    seconds(a.0.max(b.0), a.1.min(b.1)).max(0.0)
}

/// Seconds from `from` to `to`, exactly (microseconds, not whole seconds).
pub fn seconds(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    let delta = to - from;
    delta.num_microseconds().map_or_else(
        // Only for spans beyond ~292 000 years.
        || delta.num_seconds() as f64,
        |micros| micros as f64 / 1_000_000.0,
    )
}
