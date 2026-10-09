import CryptoKit
import Foundation

/// FLAC for 16-bit mono PCM, as Android's `Flac.kt`: fixed-predictor subframes
/// with Rice-coded residuals, the method libFLAC's fastest levels use. Lossless,
/// about a seventh of the WAV on household audio.
enum Flac {
    static let block = 4096

    /// `fLaC` and the STREAMINFO block: the bytes `FlacFile` patches at close.
    static let headerBytes = 42

    private static let bits = 16
    private static let maxOrder = 4
    private static let maxPartitionOrder = 6
    private static let maxRice = 14

    /// Zero `totalSamples`, frame sizes or `md5` mean "unknown", which is what a
    /// file cut short by a crash carries: decoders then read to the end.
    static func header(rate: Int, totalSamples: Int, minFrame: Int, maxFrame: Int, md5: Data)
        -> Data
    {
        var b = Bits()
        for c in "fLaC".utf8 { b.put(Int(c), 8) }
        b.put(1, 1)  // the last metadata block
        b.put(0, 7)  // STREAMINFO
        b.put(34, 24)
        b.put(block, 16)
        b.put(block, 16)
        b.put(minFrame, 24)
        b.put(maxFrame, 24)
        b.put(rate, 20)
        b.put(0, 3)  // one channel
        b.put(bits - 1, 5)
        b.put(totalSamples >> 32, 4)
        b.put(totalSamples & 0xFFFF_FFFF, 32)
        for byte in md5 { b.put(Int(byte), 8) }
        return Data(b.bytes)
    }

    /// One frame of `x`; only the stream's last may be short of `block`.
    static func frame(_ x: [Int], number: Int, rate: Int) -> Data {
        let count = x.count
        var b = Bits()
        b.put(0b11_1111_1111_1110, 14)
        b.put(0, 2)  // reserved, fixed block size
        let sizeCode = count == block ? 12 : count <= 256 ? 6 : 7
        b.put(sizeCode, 4)
        b.put(rate == 48_000 ? 10 : 0, 4)
        b.put(0, 4)  // mono
        b.put(4, 3)  // 16 bits per sample
        b.put(0, 1)
        utf8(&b, number)
        if sizeCode == 6 { b.put(count - 1, 8) }
        if sizeCode == 7 { b.put(count - 1, 16) }
        b.put(crc8(b.bytes), 8)
        subframe(&b, x)
        b.align()
        b.put(crc16(b.bytes), 16)
        return Data(b.bytes)
    }

    private static func subframe(_ b: inout Bits, _ x: [Int]) {
        let n = x.count
        if x.allSatisfy({ $0 == x[0] }) {
            b.put(0, 8)  // constant
            b.put(x[0], bits)
            return
        }
        var best = 0
        var bestSum = Int.max
        for order in 0...min(maxOrder, n - 1) {
            var sum = 0
            for i in order..<n { sum += abs(residual(x, i, order)) }
            if sum < bestSum {
                bestSum = sum
                best = order
            }
        }
        let residuals = (best..<n).map { residual(x, $0, best) }
        let r = rice(residuals, n: n, order: best)
        if 8 + best * bits + 6 + r.bits >= 8 + n * bits {
            b.put(1 << 1, 8)  // verbatim
            for s in x { b.put(s, bits) }
            return
        }
        b.put((8 | best) << 1, 8)  // fixed, of this order
        for i in 0..<best { b.put(x[i], bits) }
        b.put(0, 2)  // Rice, 4-bit parameters
        b.put(r.partitionOrder, 4)
        var at = 0
        for (p, k) in r.parameters.enumerated() {
            let size = (n >> r.partitionOrder) - (p == 0 ? best : 0)
            b.put(k, 4)
            for i in at..<(at + size) {
                let u = fold(residuals[i])
                b.unary(u >> k)
                b.put(u, k)
            }
            at += size
        }
    }

    private struct Rice {
        let partitionOrder: Int
        let parameters: [Int]
        let bits: Int
    }

    /// The partition order and per-partition parameters that cost the fewest bits.
    private static func rice(_ residuals: [Int], n: Int, order: Int) -> Rice {
        var best: Rice?
        for po in 0...maxPartitionOrder {
            if n % (1 << po) != 0 || (n >> po) <= order { break }
            var parameters = [Int](repeating: 0, count: 1 << po)
            var total = 0
            var at = 0
            for p in parameters.indices {
                let size = (n >> po) - (p == 0 ? order : 0)
                var sum = 0
                for i in at..<(at + size) { sum += fold(residuals[i]) }
                let guess =
                    size == 0 || sum < size
                    ? 0 : (Int.bitWidth - 1) - (sum / size).leadingZeroBitCount
                var partBest = Int.max
                for k in max(0, guess - 1)...min(maxRice, guess + 1) {
                    var cost = 4 + size * (k + 1)
                    for i in at..<(at + size) { cost += fold(residuals[i]) >> k }
                    if cost < partBest {
                        partBest = cost
                        parameters[p] = k
                    }
                }
                total += partBest
                at += size
            }
            if best == nil || total < best!.bits {
                best = Rice(partitionOrder: po, parameters: parameters, bits: total)
            }
        }
        return best!
    }

    private static func residual(_ x: [Int], _ i: Int, _ order: Int) -> Int {
        switch order {
        case 0: x[i]
        case 1: x[i] - x[i - 1]
        case 2: x[i] - 2 * x[i - 1] + x[i - 2]
        case 3: x[i] - 3 * x[i - 1] + 3 * x[i - 2] - x[i - 3]
        default: x[i] - 4 * x[i - 1] + 6 * x[i - 2] - 4 * x[i - 3] + x[i - 4]
        }
    }

    private static func fold(_ r: Int) -> Int { (r << 1) ^ (r >> (Int.bitWidth - 1)) }

    private static func utf8(_ b: inout Bits, _ v: Int) {
        if v < 0x80 {
            b.put(v, 8)
            return
        }
        var extra = 1
        while v >= 1 << (5 * extra + 6) { extra += 1 }
        b.put(((0xFF00 >> (extra + 1)) & 0xFF) | (v >> (6 * extra)), 8)
        for i in stride(from: extra - 1, through: 0, by: -1) {
            b.put(0x80 | ((v >> (6 * i)) & 0x3F), 8)
        }
    }

    static func crc8(_ bytes: [UInt8]) -> Int {
        var crc = 0
        for byte in bytes {
            crc ^= Int(byte)
            for _ in 0..<8 {
                crc = crc & 0x80 != 0 ? ((crc << 1) ^ 0x07) & 0xFF : (crc << 1) & 0xFF
            }
        }
        return crc
    }

    static func crc16(_ bytes: [UInt8]) -> Int {
        var crc = 0
        for byte in bytes {
            crc ^= Int(byte) << 8
            for _ in 0..<8 {
                crc = crc & 0x8000 != 0 ? ((crc << 1) ^ 0x8005) & 0xFFFF : (crc << 1) & 0xFFFF
            }
        }
        return crc
    }

    /// Big-endian bits into a growing byte array.
    private struct Bits {
        private(set) var bytes: [UInt8] = []
        private var acc: UInt64 = 0
        private var pending = 0

        /// The low `n` bits of `value`, n at most 32.
        mutating func put(_ value: Int, _ n: Int) {
            if n == 0 { return }
            acc = (acc << UInt64(n)) | (UInt64(bitPattern: Int64(value)) & ((1 << UInt64(n)) - 1))
            pending += n
            while pending >= 8 {
                pending -= 8
                bytes.append(UInt8(truncatingIfNeeded: acc >> UInt64(pending)))
            }
            acc &= (1 << UInt64(pending)) - 1
        }

        mutating func unary(_ zeros: Int) {
            var left = zeros
            while left >= 32 {
                put(0, 32)
                left -= 32
            }
            put(1, left + 1)
        }

        mutating func align() {
            if pending > 0 { put(0, 8 - pending) }
        }
    }
}

/// A FLAC file written as s16le PCM arrives: a frame lands each time a block
/// fills, and `finish` patches the header with the truth. A crash leaves the
/// frames written so far under a header that says "length unknown", which
/// decoders read to the end.
final class FlacFile {
    private let out: FileHandle
    private let rate: Int
    private var block: [Int] = []
    private var low: UInt8?
    private var md5 = Insecure.MD5()
    private var frames = 0
    private var samples = 0
    private var minFrame = Int.max
    private var maxFrame = 0

    /// Creates (or truncates) `url` and writes the provisional header.
    init(creating url: URL, rate: Int) throws {
        FileManager.default.createFile(atPath: url.path, contents: nil)
        out = try FileHandle(forWritingTo: url)
        self.rate = rate
        block.reserveCapacity(Flac.block)
        out.write(
            Flac.header(
                rate: rate, totalSamples: 0, minFrame: 0, maxFrame: 0, md5: Data(count: 16)))
    }

    func write(_ pcm: Data) {
        for byte in pcm {
            guard let lo = low else {
                low = byte
                continue
            }
            block.append(Int(Int16(bitPattern: UInt16(byte) << 8 | UInt16(lo))))
            low = nil
            if block.count == Flac.block { flush() }
        }
    }

    /// Writes the last frame, patches the header and closes the file.
    func finish() {
        flush()
        let known = frames > 0
        out.seek(toFileOffset: 0)
        out.write(
            Flac.header(
                rate: rate, totalSamples: samples, minFrame: known ? minFrame : 0,
                maxFrame: maxFrame, md5: known ? Data(md5.finalize()) : Data(count: 16)))
        try? out.close()
    }

    private func flush() {
        if block.isEmpty { return }
        var pcm = Data(capacity: block.count * 2)
        for s in block { withUnsafeBytes(of: Int16(s).littleEndian) { pcm.append(contentsOf: $0) } }
        md5.update(data: pcm)
        let frame = Flac.frame(block, number: frames, rate: rate)
        out.seekToEndOfFile()
        out.write(frame)
        minFrame = min(minFrame, frame.count)
        maxFrame = max(maxFrame, frame.count)
        frames += 1
        samples += block.count
        block.removeAll(keepingCapacity: true)
    }
}
