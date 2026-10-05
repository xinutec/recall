import Foundation

/// A bounded PCM hand-off from the microphone to the network, as Android's
/// `PcmSpool.kt`. Network.framework queues sends without bound, so a busy host would
/// otherwise mean unbounded memory.
///
/// `offer` never blocks: when full, the oldest audio is dropped and counted.
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

    /// Take PCM. Never blocks.
    func offer(_ chunk: Data) {
        lock.lock()
        defer { lock.unlock() }
        // A chunk bigger than the spool keeps only its tail.
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
