import Foundation

/// The ingest handshake line — a direct port of the Android app's `Handshake.kt`,
/// kept pure so the two announcements can't drift apart and the format is testable
/// without a socket.
enum Handshake {
    /// The one-line announcement sent on the shared ingest port before any PCM:
    /// identity, stream shape, and the capture epoch of the first PCM byte.
    ///
    /// `epoch` is the wall-clock at handshake time. The capture engine streams
    /// continuously and PCM is dropped while disconnected, so the first block a
    /// fresh connection forwards was captured essentially now — within one tap
    /// buffer, and always at or before its arrival at the server, which is the
    /// side the server's clamp accepts. The server measures that first byte's
    /// arrival and renames this connection's segments from arrival time back to
    /// capture time, so cross-mic timestamps share a clock.
    ///
    /// Field order and the trailing newline must match the recorder's parser
    /// exactly; the epoch is fixed-point (never exponent notation) with a POSIX
    /// locale so the decimal point survives every region setting.
    static func line(id: String, rate: Int, epoch: TimeInterval) -> Data {
        let stamp = String(format: "%.3f", locale: Locale(identifier: "en_US_POSIX"), epoch)
        return Data("{\"id\":\"\(id)\",\"rate\":\(rate),\"channels\":1,\"epoch\":\(stamp)}\n".utf8)
    }
}
