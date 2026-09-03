import Foundation

/// A bounded PCM hand-off between the microphone and the network — a direct port
/// of the Android app's `PcmSpool.kt`, kept in step so both recorders behave the
/// same way when the host cannot keep up.
///
/// The capture tap used to hand each buffer straight to the connection. On iOS
/// that does not block the way Android's socket write does — Network.framework
/// queues instead — but the queue is nobody's to bound, so a busy host turns into
/// unbounded memory and a burst delivered much later. Measured 2026-09-03 with the
/// Mac at load 42: iphone11's segment rotation showed exactly that shape, a mean
/// BELOW 60s with a 228s worst case (stall, then a backlog arriving at once).
///
/// A recorder must not depend on its consumer's mood. `offer` never blocks and
/// never fails; if the spool fills, the OLDEST audio is discarded and counted.
/// Dropping is a real loss either way — the point is that it is bounded, chosen,
/// and REPORTED, rather than happening invisibly inside a framework buffer.
final class PcmSpool {
    private let capacityBytes: Int
    private var buffer: Data
    private var droppedBytes = 0
    private let lock = NSLock()

    init(capacityBytes: Int) {
        self.capacityBytes = capacityBytes
        self.buffer = Data()
        self.buffer.reserveCapacity(capacityBytes)
    }

    /// Take freshly-captured PCM. Never blocks.
    func offer(_ chunk: Data) {
        lock.lock()
        defer { lock.unlock() }
        // A chunk bigger than the whole spool keeps only its tail — the newest audio.
        var incoming = chunk
        if incoming.count > capacityBytes {
            let drop = incoming.count - capacityBytes
            droppedBytes += drop
            incoming = incoming.suffix(capacityBytes)
        }
        let overflow = (buffer.count + incoming.count) - capacityBytes
        if overflow > 0 {
            droppedBytes += overflow
            buffer.removeFirst(overflow)
        }
        buffer.append(incoming)
    }

    /// Everything held, oldest first, emptying the spool.
    func drain() -> Data {
        lock.lock()
        defer { lock.unlock() }
        let out = buffer
        buffer = Data()
        buffer.reserveCapacity(capacityBytes)
        return out
    }

    /// Bytes currently waiting to be sent.
    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return buffer.count
    }

    /// Bytes of captured audio discarded because the sender could not keep up.
    var dropped: Int {
        lock.lock()
        defer { lock.unlock() }
        return droppedBytes
    }
}
