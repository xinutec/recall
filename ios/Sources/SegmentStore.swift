import Foundation

/// The phone's segment cache, as on Android: a segment's state is its directory,
/// since a rename cannot half-happen.
///
///   segments/open/       being written
///   segments/            closed, undelivered: what the uploader drains
///   segments/delivered/  on Isis, its receipt's sha-256 matching ours
///   segments/conflict/   Isis holds different bytes under the name, or refused it
///
/// Only `evict` deletes, and only from `delivered/`, oldest first, under cache
/// pressure; never because the server asked (docs/architecture.md, decision 2).
enum SegmentStore {
    /// ~2 GiB, over a day of FLAC: covers the time from upload to the server's backup.
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

    /// Closed and undelivered, oldest first.
    static func undelivered() -> [URL] { files(in: root()) }

    /// Close whatever a crash left in `open/`: truncated, but real audio. Run at
    /// recorder start, before a new segment opens.
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

    /// Delete delivered segments, oldest first, until under the ceiling. With
    /// nothing delivered it stays over.
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
