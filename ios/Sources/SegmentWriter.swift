import Foundation

/// PCM chunks into closed, capture-stamped WAV segments (stage C2's shadow,
/// mirroring the Android `SegmentWriter`): one minute of audio per file,
/// counted in bytes fed — audio time, not wall time — named
/// `<source>-YYYYMMDDTHHMMSS.wav` from this device's UTC clock at segment
/// open. WAV first for the same reason as Android: the delivery protocol is
/// container-agnostic and a platform encoder's header behaviour is probed
/// before it is trusted.
///
/// The mic path calls `offer` and returns; writing happens on the caller's
/// audio-callback-free queue (the stream's drainer feeds us, never the audio
/// thread directly). A segment never spans a disconnect gap: the client calls
/// `closeSegment()` when the stream drops, because the name claims continuity
/// from its stamp and the tee is gated on the connection.
final class SegmentWriter {
    static let sampleRate = 48_000
    static let segmentBytes = 60 * sampleRate * 2

    private let source: String
    private var handle: FileHandle?
    private var path: URL?
    private var written = 0
    var onSegmentClosed: (() -> Void)?

    init(source: String) {
        self.source = source
        SegmentStore.sweepOpen()
    }

    private static let stamp: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd'T'HHmmss"
        f.timeZone = TimeZone(identifier: "UTC")
        f.locale = Locale(identifier: "en_US_POSIX")
        return f
    }()

    static func wavHeader(dataBytes: Int) -> Data {
        var d = Data()
        func le32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func le16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        d.append(contentsOf: Array("RIFF".utf8))
        le32(UInt32(36 + dataBytes))
        d.append(contentsOf: Array("WAVE".utf8))
        d.append(contentsOf: Array("fmt ".utf8))
        le32(16)
        le16(1)  // PCM
        le16(1)  // mono
        le32(UInt32(sampleRate))
        le32(UInt32(sampleRate * 2))
        le16(2)
        le16(16)
        d.append(contentsOf: Array("data".utf8))
        le32(UInt32(dataBytes))
        return d
    }

    func offer(_ data: Data) {
        var from = 0
        let bytes = [UInt8](data)
        while from < bytes.count {
            let out = handle ?? openSegment()
            guard let out else { return }
            let room = Self.segmentBytes - written
            let take = min(room, bytes.count - from)
            out.write(Data(bytes[from..<(from + take)]))
            written += take
            from += take
            if written >= Self.segmentBytes { closeSegment() }
        }
    }

    private func openSegment() -> FileHandle? {
        let name = "\(source)-\(Self.stamp.string(from: Date())).wav"
        let target = SegmentStore.open().appendingPathComponent(name)
        FileManager.default.createFile(atPath: target.path, contents: Self.wavHeader(dataBytes: 0))
        guard let out = try? FileHandle(forWritingTo: target) else { return nil }
        out.seekToEndOfFile()
        handle = out
        path = target
        written = 0
        return out
    }

    /// Patch the header with the truth and rename into the closed set — the
    /// only step anything downstream observes. Idempotent.
    func closeSegment() {
        guard let out = handle, let target = path else { return }
        handle = nil
        path = nil
        out.seek(toFileOffset: 0)
        out.write(Self.wavHeader(dataBytes: written))
        try? out.close()
        if written == 0 {
            try? FileManager.default.removeItem(at: target)
        } else {
            let dest = SegmentStore.root().appendingPathComponent(target.lastPathComponent)
            if (try? FileManager.default.moveItem(at: target, to: dest)) != nil {
                onSegmentClosed?()
            }
        }
        written = 0
    }
}
