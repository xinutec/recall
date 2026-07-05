import Combine
import Foundation

/// Household pause state, read from the recorder's `/api/capture`. Mirrors the
/// Android `CaptureState` — the API is the authority on pause, never the socket.
/// `reachable` is false when the API call failed (off the LAN), so the banner hides.
struct CaptureState: Equatable {
    var running: Bool
    var reachable: Bool
    var pausedUntil: Date?
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
    @Published var connected = false  // TCP up and streaming
    @Published var phase: MicPhase = .stopped
    @Published var level: Float = 0  // 0...1 meter position
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
