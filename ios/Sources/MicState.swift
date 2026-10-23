import Combine
import Foundation

/// What a person asked capture to do, as recall's `Window` (audiocore): off, record
/// until a time, or hold a window open without recording. Off unless someone opened
/// one; recording with no window is a fault, which records.
enum Window: Equatable {
    case off
    case record(until: Date)
    case hold(until: Date)

    /// The window as it stands at `now`: past its end, off.
    func at(_ now: Date) -> Window {
        switch self {
        case .record(let until), .hold(let until): return until > now ? self : .off
        case .off: return .off
        }
    }
}

/// What a button asks of the recording window.
enum Press {
    case record(until: Date)
    case stop
    case pause
    case unpause
}

/// The capture state from `/api/capture` on Isis, as Android's `CaptureState`;
/// `reachable` is false when the call failed, and the banner hides.
///
/// `running`, `until` and `held` are what the mic applied, `desired*` what was asked,
/// changed at the press; `settled` that they agree. Until then the UI says
/// "Starting…", "Stopping…" or "Pausing…".
struct CaptureState: Equatable {
    var running: Bool
    var reachable: Bool
    var until: Date?
    var held = false
    var desiredRunning = false
    var desiredUntil: Date?
    var desiredHeld = false
    var settled = true
    var micReachable = true
    /// Sent back as `?known=` to long-poll; nil means poll plainly.
    var stateToken: String?

    /// The window asked for; nil for a fault (recording with no end), which records.
    var desiredWindow: Window? {
        guard let end = desiredUntil else { return desiredRunning ? nil : .off }
        return desiredHeld ? .hold(until: end) : .record(until: end)
    }
}

/// The streaming phase, for the status card.
enum MicPhase: Equatable {
    case stopped
    case waitingForHost  // can't reach the recorder
    case off  // recorder is up, but no recording window is open
    case streaming  // connected and pumping PCM
}

/// Observable app state shared by the UI, the audio capture, and the stream client.
@MainActor
final class MicState: ObservableObject {
    @Published var running = false  // user pressed Start
    /// False while the mic will not open. Sent in the heartbeat (#887). Starts
    /// true: not known to be broken.
    @Published var micOk = true
    @Published var connected = false  // TCP up and streaming
    @Published var phase: MicPhase = .stopped
    @Published var level: Float = 0  // 0...1 meter position
    /// Bytes of audio dropped since launch because the spool overran; normally zero.
    /// Lost from both the stream and the phone's copy. The heartbeat reports it.
    @Published var droppedBytes: Int = 0
    @Published var capture = CaptureState(running: false, reachable: false)
    @Published var sources: [SourceStatus] = []  // fleet liveness for the Devices panel

    var deviceID: String { Prefs.deviceID }

    var statusText: String {
        switch phase {
        case .stopped: return "Stopped"
        case .waitingForHost: return "Waiting for recall host"
        case .off: return "Off: no recording window open"
        case .streaming: return "Streaming to \(Prefs.host)"
        }
    }
}
