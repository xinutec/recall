import Combine
import Foundation

/// Household pause state, read from the recorder's `/api/capture`. Mirrors the
/// Android `CaptureState` — the API is the authority on pause, never the socket.
/// `reachable` is false when the API call failed (off the LAN), so the banner hides.
///
/// Spec-vs-status: `running`/`pausedUntil` is the mic's confirmed word, `desired*`
/// is the intent (moves the instant a button is pressed), `settled` says they
/// agree. Unsettled renders as "Pausing…"/"Resuming…" — never a flap between the
/// intent just set and a not-yet-caught-up report. Defaults read an older server's
/// confirmed-only answer as settled.
struct CaptureState: Equatable {
    var running: Bool
    var reachable: Bool
    var pausedUntil: Date?
    var desiredRunning = true
    var desiredPausedUntil: Date?
    var settled = true
    var micReachable = true
    /// Fingerprint echoed back as ?known= to long-poll /api/capture — the request
    /// hangs until the state changes. Nil on an older server (plain polling).
    var stateToken: String?
}

/// Connection / streaming phase, used to drive the status card text.
enum MicPhase: Equatable {
    case stopped
    case waitingForHost  // can't reach the recorder
    case paused  // recorder is up but household recording is paused
    case streaming  // connected and pumping PCM
}

/// Observable app state shared by the UI, the audio capture, and the stream client.
@MainActor
final class MicState: ObservableObject {
    @Published var running = false  // user pressed Start
    /// False while the audio engine will not open. Carried by the heartbeat so a
    /// running-but-deaf app SAYS so — before #887 it simply stopped beating, and the
    /// check went red for the wrong reason. Starts true: "not known to be broken".
    @Published var micOk = true
    @Published var connected = false  // TCP up and streaming
    @Published var phase: MicPhase = .stopped
    @Published var level: Float = 0  // 0...1 meter position
    /// Bytes of captured audio this app discarded because it could not deliver
    /// them (the spool overran). The phone is the ONLY place that knows: those
    /// samples never reach the network, so no server-side check can see them.
    /// Zero is the normal value; anything else is speech heard and lost.
    @Published var droppedBytes: Int = 0
    @Published var capture = CaptureState(running: true, reachable: false, pausedUntil: nil)
    @Published var sources: [SourceStatus] = []  // fleet liveness for the Devices panel

    var deviceID: String { Prefs.deviceID }

    var statusText: String {
        switch phase {
        case .stopped: return "Stopped"
        case .waitingForHost: return "Waiting for recall host"
        case .paused: return "Household recording paused"
        case .streaming: return "Streaming to \(Prefs.host)"
        }
    }
}
