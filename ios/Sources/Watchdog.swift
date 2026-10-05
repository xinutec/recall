import Foundation

/// Whether the mic has stalled. iOS can stop delivering audio silently (an
/// interruption that never ends, a route change, a media-services reset) while the
/// connection stays up, which looks like a live, silent source.
enum Watchdog {
    /// A healthy mic delivers a buffer every ~43 ms.
    static let stallThreshold: TimeInterval = 5

    /// Nil `lastBufferAt` (no buffer yet since start) is not a stall.
    static func isStalled(
        lastBufferAt: Date?, now: Date, threshold: TimeInterval = stallThreshold
    ) -> Bool {
        guard let lastBufferAt else { return false }
        return now.timeIntervalSince(lastBufferAt) > threshold
    }
}
