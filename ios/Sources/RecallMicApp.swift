import SwiftUI

@main
struct RecallMicApp: App {
    @StateObject private var controller = RecallController()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView(
                state: controller.state,
                onStart: { controller.start() },
                onStop: { controller.stop() },
                onPause: { controller.pauseHousehold() },  // also used by "Still away (24h)"
                onResume: { controller.resumeHousehold() },
                onHostChanged: { controller.restartPolling() }
            )
            .onAppear { controller.onLaunch() }
            // The polls only feed the visible UI (banner + Devices panel); a
            // backgrounded app kept alive for days by its audio session must not
            // hit the API ~57k times a day for a screen nobody sees. Streaming
            // itself is unaffected — StreamClient runs its own loop.
            .onChange(of: scenePhase) { phase in
                controller.setUIVisible(phase == .active)
            }
        }
    }
}

/// Owns the shared state and the stream client, runs the control-plane polling, and
/// wires Start/Stop and the household pause/resume intent.
///
/// Note: iOS can't auto-launch on boot (unlike the Android `BootReceiver`); instead the
/// app auto-resumes streaming when it's next opened, if it was left enabled.
@MainActor
final class RecallController: ObservableObject {
    let state = MicState()
    private lazy var client = StreamClient(state: state)
    private var capturePoll: Task<Void, Never>?
    private var sourcesPoll: Task<Void, Never>?

    func onLaunch() {
        restartPolling()
        if Prefs.enabled && !Prefs.host.isEmpty { start() }
    }

    // MARK: streaming

    func start() {
        guard !state.running else { return }
        Task {
            guard await AudioCapture.requestPermission() else { return }
            let ok = client.start()  // false if the mic couldn't be opened
            state.running = ok
            Prefs.enabled = ok
        }
    }

    func stop() {
        Prefs.enabled = false
        state.running = false
        state.phase = .stopped
        state.level = 0
        client.stop()
    }

    // MARK: household pause (control plane)

    func pauseHousehold() {
        Task { state.capture = await CaptureApi.pause(host: Prefs.controlHost) }
    }

    func resumeHousehold() {
        Task { state.capture = await CaptureApi.resume(host: Prefs.controlHost) }
    }

    // MARK: polling — capture state every 5s, fleet liveness every 1.5s (Android
    // cadence), and — like Android — only while the UI is actually visible.

    func setUIVisible(_ visible: Bool) {
        if visible {
            restartPolling()
        } else {
            capturePoll?.cancel()
            sourcesPoll?.cancel()
            capturePoll = nil
            sourcesPoll = nil
        }
    }

    func restartPolling() {
        capturePoll?.cancel()
        sourcesPoll?.cancel()
        capturePoll = Task { [weak self] in
            while !Task.isCancelled {
                if !Prefs.controlHost.isEmpty {
                    let cap = await CaptureApi.state(host: Prefs.controlHost)
                    self?.state.capture = cap
                }
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
        sourcesPoll = Task { [weak self] in
            while !Task.isCancelled {
                if !Prefs.controlHost.isEmpty,
                    let s = await CaptureApi.sources(host: Prefs.controlHost) {
                    self?.state.sources = s
                }
                try? await Task.sleep(nanoseconds: 1_500_000_000)
            }
        }
    }
}
