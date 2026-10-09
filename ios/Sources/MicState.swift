import Combine
import Foundation

/// The household's capture state from `/api/capture` on Isis, as Android's
/// `CaptureState`; `reachable` is false when the call failed, and the banner hides.
///
/// `running` and `pausedUntil` are what the mic confirmed, `desired*` what was asked,
/// changed at the press; `settled` that they agree. Until then the UI says
/// "Pausing…" or "Resuming…".
struct CaptureState: Equatable {
    var running: Bool
    var reachable: Bool
    var pausedUntil: Date?
    var desiredRunning = true
    var desiredPausedUntil: Date?
    var settled = true
    var micReachable = true
    /// Sent back as `?known=` to long-poll; nil means poll plainly.
    var stateToken: String?
}

/// The streaming phase, for the status card.
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
    /// False while the mic will not open. Sent in the heartbeat (#887). Starts
    /// true: not known to be broken.
    @Published var micOk = true
    @Published var connected = false  // TCP up and streaming
    @Published var phase: MicPhase = .stopped
    @Published var level: Float = 0  // 0...1 meter position
    /// Bytes of audio dropped since launch because the spool overran; normally zero.
    /// Lost from both the stream and the phone's copy. The heartbeat reports it.
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
