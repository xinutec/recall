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
            // The polls feed only the visible UI; an app kept alive in the background
            // must not poll for a screen nobody sees. Streaming has its own loop.
            .onChange(of: scenePhase) { phase in
                controller.setUIVisible(phase == .active)
            }
        }
    }
}

/// Owns the state and the stream client, polls Isis, and wires Start/Stop and the
/// household pause. iOS cannot start an app at boot, so streaming resumes when the app
/// is next opened, if it was enabled.
@MainActor
final class RecallController: ObservableObject {
    let state = MicState()
    private lazy var client = StreamClient(state: state)
    private var capturePoll: Task<Void, Never>?
    private var sourcesPoll: Task<Void, Never>?
    private var beatLoop: Task<Void, Never>?

    func onLaunch() {
        restartPolling()
        startBeating()
        if Prefs.enabled && !Prefs.host.isEmpty { start() }
    }

    // MARK: streaming

    func start() {
        guard !state.running else { return }
        Task {
            guard await AudioCapture.requestPermission() else { return }
            // The intent, whatever the engine answers: storing the outcome once let
            // one failed open disable auto-start and the beat for good (#887).
            Prefs.enabled = true
            let ok = client.start()  // false if the mic couldn't be opened
            state.running = ok
            state.micOk = ok
            // Beat either way, carrying `micOk`. The loop owns retries.
            _ = await beatNow()
        }
    }

    func stop() {
        Prefs.enabled = false
        state.running = false
        state.phase = .stopped
        state.level = 0
        client.stop()
    }

    // MARK: liveness (#837)

    /// The beat loop, from launch, never cancelled: unlike the polls it runs in the
    /// background too, since the backgrounded app is what it reports on.
    private func startBeating() {
        guard beatLoop == nil else { return }
        beatLoop = Task { [weak self] in
            // Consecutive failures, which shorten the next wait (#886).
            var failures = 0
            while !Task.isCancelled {
                let outcome = await self?.beatNow() ?? .skipped
                failures = outcome.nextFailureCount(after: failures)
                let delay = Heartbeat.nextDelay(consecutiveFailures: failures)
                try? await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000_000)
            }
        }
    }

    /// Beats only while started; see `Heartbeat.Outcome`.
    private func beatNow() async -> Heartbeat.Outcome {
        guard Prefs.enabled else { return .skipped }
        let sent = await Heartbeat.send(
            host: Prefs.controlHost, lanHost: Prefs.host, device: Prefs.deviceID,
            streaming: state.connected, micOk: state.micOk)
        return sent ? .sent : .failed
    }

    // MARK: household pause (control plane)

    func pauseHousehold() {
        Task { state.capture = await CaptureApi.pause(host: Prefs.controlHost) }
    }

    func resumeHousehold() {
        Task { state.capture = await CaptureApi.resume(host: Prefs.controlHost) }
    }

    // MARK: polling, only while the UI is visible: the capture state by long poll,
    // the recorders every 1.5 s, as on Android.

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
                guard !Prefs.controlHost.isEmpty else {
                    try? await Task.sleep(nanoseconds: 5_000_000_000)
                    continue
                }
                // The server answers when the state changes. Without a stateToken
                // (or on failure), a plain 5 s poll.
                let known = self?.state.capture.stateToken
                let cap = await CaptureApi.state(host: Prefs.controlHost, wait: 25, known: known)
                self?.state.capture = cap
                let pace: UInt64 = cap.stateToken != nil ? 250_000_000 : 5_000_000_000
                try? await Task.sleep(nanoseconds: pace)
            }
        }
        sourcesPoll = Task { [weak self] in
            while !Task.isCancelled {
                if !Prefs.controlHost.isEmpty,
                    let s = await CaptureApi.sources(host: Prefs.controlHost)
                {
                    self?.state.sources = s
                }
                try? await Task.sleep(nanoseconds: 1_500_000_000)
            }
        }
    }
}
