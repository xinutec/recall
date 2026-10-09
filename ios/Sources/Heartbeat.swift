import Foundation

#if canImport(UIKit)
    import UIKit
#endif

/// "Still here", once an hour, whether or not anything streams (#837): audio alone
/// cannot tell a dead app from a quiet room or a pause.
///
/// Sent to Isis, reachable from anywhere over WireGuard, so a phone that is out still
/// beats; the recorder's LAN address is the fallback.
///
/// Only while the app is started: a stopped app records nothing and must not look
/// alive. Best-effort and silent.
enum Heartbeat {
    private static let timeout: TimeInterval = 8

    /// How often to beat; equal to `recalld::devices::BEAT_EVERY_MINUTES`, which the
    /// grader's thresholds are multiples of.
    static let every: TimeInterval = 60 * 60

    /// First retry after a failed beat; doubles per failure up to `every` (#886).
    private static let retryBase: TimeInterval = 60

    /// When this process started (first touched at launch), so restarts show.
    static let startedAt = Date()

    /// What one attempt did. A beat skipped because the app is stopped is not a
    /// failure, and must not drive the backoff.
    enum Outcome {
        case sent
        case failed
        case skipped

        /// The failure count to carry into the next wait.
        func nextFailureCount(after current: Int) -> Int {
            switch self {
            case .sent: 0
            case .failed: current + 1
            case .skipped: current
            }
        }
    }

    /// Seconds until the next beat: the full cadence after a success, a backoff after
    /// a failure, never more often than hourly once the backoff reaches the cadence.
    static func nextDelay(consecutiveFailures: Int) -> TimeInterval {
        guard consecutiveFailures > 0 else { return every }
        // Doubling, not shifting: the counter grows without bound in a dead spot.
        var delay = retryBase
        for _ in 1..<max(consecutiveFailures, 1) {
            if delay >= every { return every }
            delay *= 2
        }
        return min(delay, every)
    }

    /// App version and build, so a restart into a new build reads as a deploy.
    static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }

    /// A beat's JSON, the server's `HeartbeatIn`.
    static func body(
        device: String, version: String, startedAt: Date, streaming: Bool, charging: Bool?,
        micOk: Bool, droppedBytes: Int
    ) -> [String: Any] {
        var out: [String: Any] = [
            "device": device,
            "app": "ios",
            "version": version,
            "startedAt": iso(startedAt),
            "streaming": streaming,
            // A running app that cannot open its mic (#887).
            "micOk": micOk,
            "droppedBytes": droppedBytes,
        ]
        // Absent when unknown (the simulator, monitoring off), rather than a guess.
        if let charging { out["charging"] = charging }
        return out
    }

    /// True on mains, false on battery, nil if unknown. For a room phone, discharging
    /// warns of its death; reported, not graded, since a carried phone discharges all
    /// day.
    static func charging() -> Bool? {
        #if canImport(UIKit)
            UIDevice.current.isBatteryMonitoringEnabled = true
            switch UIDevice.current.batteryState {
            case .charging, .full: return true
            case .unplugged: return false
            default: return nil
            }
        #else
            return nil
        #endif
    }

    /// Audio dropped since the last beat that landed, from a running total.
    struct DropsSinceBeat {
        private var reported = 0

        func pending(total: Int) -> Int { total - reported }

        mutating func landed(total: Int) { reported = total }
    }

    /// POST one beat to Isis, else to the recorder's LAN address, where the Mac
    /// relays it (#888): a phone at home with its tunnel off still records, so must
    /// not read as dead. The relay marks what it forwards.
    @discardableResult
    static func send(
        host: String, lanHost: String = "", device: String, streaming: Bool, micOk: Bool,
        droppedBytes: Int
    ) async -> Bool {
        let payload = body(
            device: device, version: version, startedAt: startedAt,
            streaming: streaming, charging: charging(), micOk: micOk, droppedBytes: droppedBytes)
        for candidate in hostsToTry(control: host, lan: lanHost) {
            if await post(payload, to: candidate) { return true }
        }
        return false
    }

    /// The hosts to try, in order, blanks and duplicates dropped.
    static func hostsToTry(control: String, lan: String) -> [String] {
        var seen: Set<String> = []
        return [control, lan].filter { !$0.isEmpty && seen.insert($0).inserted }
    }

    private static func post(_ payload: [String: Any], to host: String) async -> Bool {
        guard let url = URL(string: "\(ApiBase.of(host))/api/devices/heartbeat")
        else { return false }
        guard let data = try? JSONSerialization.data(withJSONObject: payload) else {
            return false
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.timeoutInterval = timeout
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = data
        guard let (_, resp) = try? await URLSession.shared.data(for: req) else {
            return false
        }
        // Isis answers 200, the relay 204.
        let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
        return (200..<300).contains(code)
    }

    private static func iso(_ date: Date) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f.string(from: date)
    }
}
