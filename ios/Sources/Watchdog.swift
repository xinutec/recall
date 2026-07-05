import Foundation

/// Pure staleness decision for the mic watchdog — split out so it's unit-testable.
///
/// iOS can silently stop delivering audio while everything else looks healthy: an
/// interruption whose `.ended` never fires (or arrives without `.shouldResume`), a
/// route change, or a media-services reset. The TCP connection stays up, the phase
/// stays `.streaming`, and the server receives a live-looking silent source — the
/// worst failure mode for a recorder. The watchdog compares "when did the mic last
/// deliver a buffer" against a threshold and, when stalled, the capture engine is
/// kicked and the meter zeroed so the stall is visible instead of frozen.
enum Watchdog {
    /// The mic delivers a buffer every ~43 ms while healthy; 5 s of nothing is a
    /// stall, not scheduling jitter.
    static let stallThreshold: TimeInterval = 5

    /// `lastBufferAt` is nil until the first buffer after start — that grace period
    /// is not a stall (the engine may legitimately take a moment to spin up).
    static func isStalled(
        lastBufferAt: Date?, now: Date, threshold: TimeInterval = stallThreshold
    ) -> Bool {
        guard let lastBufferAt else { return false }
        return now.timeIntervalSince(lastBufferAt) > threshold
    }
}
