import CryptoKit
import Foundation

/// Delivers closed segments to recalld's ingest plane and believes nothing
/// but its own arithmetic (docs/architecture.md, decision 3): a delivery
/// counts only when the receipt's sha-256 equals a local digest of the bytes
/// just sent. 409 → `conflict/`, never retried (a person must look); auth
/// answers are config, not verdicts — retry until the token arrives; anything
/// else stays put for the next pass.
///
/// A plain in-app task rather than a background scheduler: this app is alive
/// exactly while it records (the held audio session), and audio that exists
/// only while the app is gone was recorded before the app died — the next
/// launch's first pass picks it up. Wi-Fi only, via the path's own
/// `isExpensive` flag: continuous capture on a metered plan is a bill.
enum SegmentUpload {
    private static let lock = NSLock()
    private static var draining = false

    static func kick() {
        lock.lock()
        if draining {
            lock.unlock()
            return
        }
        draining = true
        lock.unlock()
        Task.detached(priority: .utility) {
            await drain()
            lock.lock()
            draining = false
            lock.unlock()
        }
    }

    private static func drain() async {
        for segment in SegmentStore.undelivered() {
            switch await deliver(segment) {
            case .verified: SegmentStore.markDelivered(segment)
            case .conflict: SegmentStore.markConflict(segment)
            case .failed: return  // the next kick retries; backoff is the cadence
            }
        }
        SegmentStore.evict()
    }

    private enum Delivery { case verified, conflict, failed }

    private static func deliver(_ segment: URL) async -> Delivery {
        guard let bytes = try? Data(contentsOf: segment) else { return .failed }
        let sha = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        let name = segment.lastPathComponent
        guard
            let url = URL(
                string: "\(Prefs.ingestBase)/ingest/v1/segments/\(Prefs.deviceID)/\(name)")
        else { return .failed }
        var request = URLRequest(url: url, timeoutInterval: 60)
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        if !Prefs.ingestToken.isEmpty {
            request.setValue("Bearer \(Prefs.ingestToken)", forHTTPHeaderField: "Authorization")
        }
        request.allowsExpensiveNetworkAccess = false  // Wi-Fi only, by policy
        do {
            let (body, response) = try await URLSession.shared.upload(for: request, from: bytes)
            guard let http = response as? HTTPURLResponse else { return .failed }
            switch http.statusCode {
            case 200:
                // The eviction-grade check: the receipt must equal OUR hash.
                guard
                    let receipt = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                    receipt["sha256"] as? String == sha,
                    receipt["bytes"] as? Int == bytes.count
                else { return .failed }
                return .verified
            case 409: return .conflict
            case 401, 403: return .failed  // missing/wrong token: config, not a verdict
            default: return .failed
            }
        } catch {
            return .failed
        }
    }
}
