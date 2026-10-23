//! When two spans are the same speech: [`same_span`] for the timeline's fold
//! and the doctor's doubled-minute count, and [`COPY_SECONDS`] for the queue,
//! which knows only when a clip starts.

use transcript::Span;

/// How far apart a mic's two clips of one minute can be stamped: a phone's
/// minute arrives as the Mac's cut of its stream and the phone's own
/// `.phone.flac`, cut separately; up to three seconds seen, a minute's clips
/// about sixty apart.
pub const COPY_SECONDS: i64 = 5;

/// Two spans are the same speech when they overlap by more than half of the
/// shorter one. A neighbour that only brushes a span, or a mic's next minute
/// that touches its last, is not.
pub fn same_span(a: Span, b: Span) -> bool {
    let shared = a.shared_micros(b);
    shared > 0 && 2 * shared > a.micros().min(b.micros())
}
