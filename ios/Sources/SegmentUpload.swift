import CryptoKit
import Foundation

/// Delivers closed segments to recalld's ingest. A delivery counts only when the
/// receipt's sha-256 matches the bytes sent (docs/architecture.md, decision 3). A
/// 409 moves to `conflict/` for a person; a token refusal is retried; anything else
/// waits for the next pass.
///
/// An in-app task: the app runs while it records, and what it left behind is picked
/// up at the next launch. Not on expensive networks.
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
                // The eviction-grade check: the receipt must equal our hash.
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
