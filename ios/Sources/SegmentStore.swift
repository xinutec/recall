import Foundation

/// The phone's segment cache (docs/architecture.md, stage C2) — the meeting
/// queue's idiom shared with Android: a segment's state is which directory it
/// is in, because a rename cannot half-happen.
///
///   segments/open/       being written; nothing touches it
///   segments/            closed, undelivered — what the uploader drains
///   segments/delivered/  Isis holds it, PROVEN: receipt sha-256 matched a local re-hash
///   segments/conflict/   Isis holds DIFFERENT bytes under this name — a person must look
///
/// Deletion happens only in `evict`, eats only `delivered/`, oldest first,
/// under cache pressure — never an undelivered segment, never on a server's
/// word (decision 2: eviction is a local decision).
enum SegmentStore {
    /// ~2 GiB: hours of WAV — local history spanning the upload→backup window.
    static let ceilingBytes: Int64 = 2 * 1024 * 1024 * 1024

    static func root() -> URL {
        let base = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("segments", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func open() -> URL { sub("open") }
    static func delivered() -> URL { sub("delivered") }
    static func conflict() -> URL { sub("conflict") }

    private static func sub(_ name: String) -> URL {
        let dir = root().appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private static func files(in dir: URL) -> [URL] {
        let all =
            (try? FileManager.default.contentsOfDirectory(
                at: dir, includingPropertiesForKeys: [.fileSizeKey])) ?? []
        return all.filter { !$0.hasDirectoryPath }.sorted {
            $0.lastPathComponent < $1.lastPathComponent
        }
    }

    /// Closed, undelivered, oldest first — the uploader's work list.
    static func undelivered() -> [URL] { files(in: root()) }

    /// Adopt a crash's leftovers: a truncated segment is real audio that was
    /// really heard. Run at recorder start, before a new segment opens.
    static func sweepOpen() {
        for orphan in files(in: open()) {
            try? FileManager.default.moveItem(
                at: orphan, to: root().appendingPathComponent(orphan.lastPathComponent))
        }
    }

    static func markDelivered(_ segment: URL) {
        try? FileManager.default.moveItem(
            at: segment, to: delivered().appendingPathComponent(segment.lastPathComponent))
    }

    static func markConflict(_ segment: URL) {
        try? FileManager.default.moveItem(
            at: segment, to: conflict().appendingPathComponent(segment.lastPathComponent))
    }

    private static func size(_ url: URL) -> Int64 {
        Int64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }

    /// Free space down to the ceiling, verified-delivered oldest first and
    /// nothing else. Over the ceiling with nothing delivered stays over —
    /// undelivered audio is the point of the cache.
    static func evict(ceiling: Int64 = ceilingBytes) {
        var total = [root(), open(), delivered(), conflict()]
            .flatMap(files(in:)).reduce(Int64(0)) { $0 + size($1) }
        for oldest in files(in: delivered()) where total > ceiling {
            let bytes = size(oldest)
            if (try? FileManager.default.removeItem(at: oldest)) != nil {
                total -= bytes
            }
        }
    }
}
