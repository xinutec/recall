import Foundation

/// A bounded PCM hand-off between the microphone and the network — a direct port
/// of the Android app's `PcmSpool.kt`, kept in step so both recorders behave the
/// same way when the host cannot keep up.
///
/// Network.framework does not block a send the way Android's socket write does;
/// it queues, and nobody bounds that queue, so a busy host becomes unbounded memory
/// and a backlog delivered at once much later.
///
/// So `offer` never blocks and never fails: if the spool fills, the oldest audio is
/// discarded and counted. Dropping is a loss either way; here it is bounded, chosen
/// and reported instead of invisible inside a framework buffer.
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
