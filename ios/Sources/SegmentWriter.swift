import Foundation

/// Writes PCM to closed, capture-stamped FLAC segments, as Android's
/// `SegmentWriter`: a minute of audio per file, counted in audio, not wall time,
/// named `<source>-YYYYMMDDTHHMMSS.phone.flac` by the UTC time the segment opened.
/// `.phone`, since the host cuts the same stream into FLAC too.
///
/// Fed by the stream's drain task, never the audio thread. The client calls
/// `closeSegment()` when the stream drops, since a name claims its audio is
/// continuous from its stamp.
final class SegmentWriter {
    static let sampleRate = 48_000
    static let segmentBytes = 60 * sampleRate * 2

    private let source: String
    private var flac: FlacFile?
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

    func offer(_ data: Data) {
        var from = data.startIndex
        while from < data.endIndex {
            guard let out = flac ?? openSegment() else { return }
            let take = min(Self.segmentBytes - written, data.endIndex - from)
            out.write(data[from..<(from + take)])
            written += take
            from += take
            if written >= Self.segmentBytes { closeSegment() }
        }
    }

    private func openSegment() -> FlacFile? {
        let name = "\(source)-\(Self.stamp.string(from: Date())).phone.flac"
        let target = SegmentStore.open().appendingPathComponent(name)
        guard let out = try? FlacFile(creating: target, rate: Self.sampleRate) else { return nil }
        flac = out
        path = target
        written = 0
        return out
    }

    /// Patch the header with the truth and rename into the closed set — the
    /// only step anything downstream observes. Idempotent.
    func closeSegment() {
        guard let out = flac, let target = path else { return }
        flac = nil
        path = nil
        out.finish()
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
