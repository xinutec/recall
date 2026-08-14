import Foundation

#if canImport(UIKit)
    import UIKit
#endif

/// "I am still here", once an hour, whether or not there is anything to stream.
///
/// recall could not tell a dead recorder from a quiet room. Its liveness marker is
/// refreshed only by audio above the silence floor — deliberately, so a dot means
/// *recording* rather than merely connected — and while the household is paused the
/// ingest listener is closed and nothing streams at all. Capture was paused for the
/// four days before this was written, which is precisely the window in which this
/// app dying would have gone unnoticed until somebody picked the phone up (#837).
///
/// Sent to the CONTROL host (Isis, over WireGuard), not the recorder on the LAN —
/// the same split every other API call here already makes. Isis is reachable from
/// anywhere, so a phone that is out of the house still beats and "away" stops
/// looking like "dead".
///
/// ⚠ Beats only while the user has this app *started*. A stopped app is not going
/// to record, and a beat that arrived anyway would paint the one state we care
/// about — this mic will not capture anything — bright green. The check going red
/// after a deliberate Stop is correct, and pressing Start clears it.
///
/// Best-effort and silent, like `OutboxReport` on Android: a liveness report that
/// raised its own failures would be the tail wagging the dog.
enum Heartbeat {
    private static let port = 8000
    private static let timeout: TimeInterval = 8

    /// How often to beat. The grader's thresholds are expressed in multiples of this
    /// (recall.mic_alive.BEAT_EVERY_MINUTES), so the two cannot drift apart silently.
    static let every: TimeInterval = 60 * 60

    /// First retry after a beat that did not land. Doubles per consecutive failure up
    /// to `every`, so a blip costs minutes rather than an hour (#886).
    private static let retryBase: TimeInterval = 60

    /// When this process started. Read once, at first touch, which is app launch —
    /// so a beat carrying a recent value means the app restarted, and "up all week"
    /// is distinguishable from "crash-looping between beats".
    static let startedAt = Date()

    /// What one attempt did. Three cases, not two: a beat SKIPPED because the app is
    /// stopped is a deliberate state, not an unreachable control plane, and feeding it
    /// to the failure counter would spin the backoff and then beat hourly for nothing.
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

    /// Seconds until the next beat: the full cadence when the last one landed, a short
    /// backoff when it did not.
    ///
    /// Pure, so the schedule is pinned in tests without a network or an hour of
    /// waiting — the same reason `body` is pure.
    ///
    /// ⚠ The cap is what keeps this a BACKOFF and not a poll. One request an hour is
    /// the design; a phone that is simply off must never beat harder than that, and
    /// the whole retry burst is bounded to fit inside one cadence.
    static func nextDelay(consecutiveFailures: Int) -> TimeInterval {
        guard consecutiveFailures > 0 else { return every }
        // Doubling by multiplication rather than shifting: this counter is reset only
        // by a success, so a phone in a dead spot keeps incrementing it forever and a
        // shift would wrap.
        var delay = retryBase
        for _ in 1..<max(consecutiveFailures, 1) {
            if delay >= every { return every }
            delay *= 2
        }
        return min(delay, every)
    }

    /// App version and build, so a restart *into a new build* reads as a deploy
    /// rather than as a fault.
    static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }

    /// The JSON a beat carries. Pure and separate from the send so it can be tested
    /// without a network — the field names are a contract with `HeartbeatIn`.
    static func body(
        device: String, version: String, startedAt: Date, streaming: Bool, charging: Bool?,
        micOk: Bool
    ) -> [String: Any] {
        var out: [String: Any] = [
            "device": device,
            "app": "ios",
            "version": version,
            "startedAt": iso(startedAt),
            "streaming": streaming,
            // A running app that cannot open its mic must SAY so rather than fall
            // silent, which is what it used to do (#887).
            "micOk": micOk,
        ]
        // Absent rather than null when unknown: the simulator and a device with
        // battery monitoring off both report `.unknown`, and guessing "discharging"
        // there would invent the very reading a room phone is watched for.
        if let charging { out["charging"] = charging }
        return out
    }

    /// True on mains, false on battery, nil when iOS will not say. Room phones are
    /// mains-powered, so discharging is the leading indicator of the death this
    /// whole feature exists to catch — carried, though never graded, because a
    /// carried phone is off charge all day and that is not a fault.
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

    /// POST one beat, trying the control plane first and the recorder's LAN address
    /// second (#888).
    ///
    /// The fallback exists because the beat used to demand MORE reachability than
    /// recording does: audio goes to `host` on the LAN, so a phone at home with its
    /// tunnel off records every sample and still read as dead. `lanHost` runs the
    /// relay on the same port, so this is the identical request with the host
    /// swapped — and the relay marks what it forwards, so "alive but its tunnel is
    /// down" stays visible instead of being papered over.
    ///
    /// Order matters: the VPN is tried FIRST so the LAN path is a backstop rather
    /// than a shortcut, and a phone away from home behaves exactly as before.
    @discardableResult
    static func send(
        host: String, lanHost: String = "", device: String, streaming: Bool, micOk: Bool
    ) async -> Bool {
        let payload = body(
            device: device, version: version, startedAt: startedAt,
            streaming: streaming, charging: charging(), micOk: micOk)
        for candidate in [host, lanHost] where !candidate.isEmpty {
            if await post(payload, to: candidate) { return true }
        }
        return false
    }

    private static func post(_ payload: [String: Any], to host: String) async -> Bool {
        guard let url = URL(string: "http://\(host):\(port)/api/devices/heartbeat")
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
        // Any 2xx: the fleet answers 200, the LAN relay 204 (it stores nothing of
        // its own, so it has no body to return). Insisting on 200 would have made
        // every relayed beat read as a failure.
        let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
        return (200..<300).contains(code)
    }

    private static func iso(_ date: Date) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f.string(from: date)
    }
}
