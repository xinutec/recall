import Foundation

/// One recorder's liveness for the fleet view (which mics are streaming now).
/// Mirrors the Android `SourceStatus`.
struct SourceStatus: Identifiable, Equatable {
    let id: String
    let name: String
    let kind: String
    let active: Bool
    let lastActive: Date?
}

/// Talks to the recall web API (port 8000) — the same control plane the web app and the
/// Android app use — to read the *household* capture pause, control it, and read the
/// fleet's per-recorder liveness. The API stays up during a pause, so the app shows the
/// true state even while the stream port is closed. Off the home LAN, calls just fail
/// and the panels stay hidden (you shouldn't control the system from away).
enum CaptureApi {
    private static let port = 8000
    private static let timeout: TimeInterval = 4

    private static func url(_ host: String, _ path: String) -> URL? {
        URL(string: "http://\(host):\(port)/api\(path)")
    }

    // MARK: capture pause

    static func state(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture", "GET"))
    }

    static func pause(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture/pause", "POST"))
    }

    static func resume(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture/resume", "POST"))
    }

    // MARK: fleet

    /// nil = request failed (caller keeps its last list, doesn't blank the panel);
    /// an empty array means the host genuinely has no sources.
    static func sources(host: String) async -> [SourceStatus]? {
        guard
            let data = await body(host, "/sources", "GET"),
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let items = obj["items"] as? [[String: Any]]
        else { return nil }
        return items.map { o in
            SourceStatus(
                id: o["id"] as? String ?? "",
                name: o["name"] as? String ?? "",
                kind: o["kind"] as? String ?? "",
                active: o["active"] as? Bool ?? false,
                lastActive: (o["lastActive"] as? String).flatMap(parseISO))
        }
    }

    // MARK: helpers

    private static func parseCapture(_ data: Data?) -> CaptureState {
        guard
            let data,
            let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return CaptureState(running: true, reachable: false, pausedUntil: nil) }
        let running = o["running"] as? Bool ?? true
        let until = (o["pausedUntil"] as? String).flatMap(parseISO)
        return CaptureState(running: running, reachable: true, pausedUntil: until)
    }

    private static func body(_ host: String, _ path: String, _ method: String) async -> Data? {
        guard !host.isEmpty, let u = url(host, path) else { return nil }
        var req = URLRequest(url: u)
        req.httpMethod = method
        req.timeoutInterval = timeout
        do {
            let (data, resp) = try await URLSession.shared.data(for: req)
            guard (resp as? HTTPURLResponse)?.statusCode == 200 else { return nil }
            return data
        } catch {
            return nil
        }
    }

    /// ISO-8601 with or without fractional seconds (the API emits both forms).
    private static func parseISO(_ s: String) -> Date? {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: s) { return d }
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: s)
    }
}
