import Foundation

/// The ingest handshake, as Android's `Handshake.kt`.
enum Handshake {
    /// The line sent on the shared ingest port before any PCM: the device, the
    /// stream's shape, and `epoch`, which the server stamps the segments with.
    ///
    /// `epoch` is the time of the handshake: PCM is dropped while disconnected, so
    /// the first block sent was captured within one tap buffer of now, never after
    /// it arrives, as the server's clamp requires.
    ///
    /// Fixed-point, in the POSIX locale, so the decimal point is a point.
    static func line(id: String, rate: Int, epoch: TimeInterval) -> Data {
        let stamp = String(format: "%.3f", locale: Locale(identifier: "en_US_POSIX"), epoch)
        return Data("{\"id\":\"\(id)\",\"rate\":\(rate),\"channels\":1,\"epoch\":\(stamp)}\n".utf8)
    }
}
