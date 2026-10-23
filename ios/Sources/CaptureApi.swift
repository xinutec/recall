import Foundation

/// One recorder's liveness, as Android's `SourceStatus`.
struct SourceStatus: Identifiable, Equatable {
    let id: String
    let name: String
    let kind: String
    let active: Bool
    let lastActive: Date?
}

/// The web API on Isis: the recording window, read and set, and the recorders'
/// liveness. It answers while capture is off, when the recorder's port is closed. A
/// failed call leaves the panels hidden.
enum CaptureApi {
    private static let timeout: TimeInterval = 4

    private static func url(_ host: String, _ path: String) -> URL? {
        URL(string: "\(ApiBase.of(host))/api\(path)")
    }

    // MARK: the recording window

    /// With `wait` and `known` (the last stateToken), a long poll: the server answers
    /// when the state changes.
    static func state(host: String, wait: Int = 0, known: String? = nil) async -> CaptureState {
        let query = wait > 0 ? "?wait=\(wait)&known=\(known ?? "")" : ""
        return await parseCapture(
            await body(host, "/capture\(query)", "GET", timeout: timeout + Double(wait)))
    }

    /// Open a recording window until `until`, or move its end.
    static func record(host: String, until: Date) async -> CaptureState {
        let f = ISO8601DateFormatter()
        return await parseCapture(
            await body(host, "/capture/record?until=\(f.string(from: until))", "POST"))
    }

    static func stop(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture/stop", "POST"))
    }

    /// Stop recording, keeping the window's end.
    static func pause(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture/pause", "POST"))
    }

    static func unpause(host: String) async -> CaptureState {
        await parseCapture(await body(host, "/capture/unpause", "POST"))
    }

    // MARK: fleet

    /// nil: the request failed; empty: no sources.
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
            let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let running = o["running"] as? Bool,
            let held = o["held"] as? Bool,
            let desiredRunning = o["desiredRunning"] as? Bool,
            let desiredHeld = o["desiredHeld"] as? Bool,
            let settled = o["settled"] as? Bool,
            let micReachable = o["micReachable"] as? Bool,
            let stateToken = o["stateToken"] as? String
        else { return CaptureState(running: false, reachable: false) }
        return CaptureState(
            running: running,
            reachable: true,
            until: (o["until"] as? String).flatMap(parseISO),
            held: held,
            desiredRunning: desiredRunning,
            desiredUntil: (o["desiredUntil"] as? String).flatMap(parseISO),
            desiredHeld: desiredHeld,
            settled: settled,
            micReachable: micReachable,
            stateToken: stateToken)
    }

    private static func body(
        _ host: String, _ path: String, _ method: String,
        timeout: TimeInterval = CaptureApi.timeout
    ) async -> Data? {
        guard !host.isEmpty, let u = url(host, path) else { return nil }
        var req = URLRequest(url: u)
        req.httpMethod = method
        req.timeoutInterval = timeout
        // This phone's own token says whose phone asks: recalld answers with its
        // owner's recorders.
        if !Prefs.ingestToken.isEmpty {
            req.setValue("Bearer \(Prefs.ingestToken)", forHTTPHeaderField: "Authorization")
        }
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
