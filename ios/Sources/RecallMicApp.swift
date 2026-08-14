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
            // The INTENT, recorded before the engine is asked and never cleared by
            // its answer. `enabled` means "this should be recording", not "the
            // engine started this time" — writing the outcome here was #887: one
            // failed mic open disabled auto-start forever AND silenced the beat,
            // so the single signal that would have reported the fault was the thing
            // the fault switched off.
            Prefs.enabled = true
            let ok = client.start()  // false if the mic couldn't be opened
            state.running = ok
            state.micOk = ok
            // Beat EITHER WAY. A mic that will not open is exactly what the fleet
            // wants to hear about, and the beat now carries `micOk` to say it.
            // The outcome is dropped on purpose: this is a one-off alongside the
            // beat loop, and the loop owns the retry schedule.
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

    /// One long-lived loop, started at launch and never cancelled — deliberately NOT
    /// wired to `setUIVisible` like the two polls below it. A backgrounded app kept
    /// alive for days by its audio session is exactly the thing this reports on, and
    /// a beat that stopped when the screen went dark would read as the app dying
    /// every time the phone was put down. One request an hour is not the traffic
    /// those polls were trimmed for.
    private func startBeating() {
        guard beatLoop == nil else { return }
        beatLoop = Task { [weak self] in
            // Consecutive failures, reset by any beat that lands. A blip must not cost
            // an hour of looking dead (#886) — which is exactly what it did on
            // 2026-08-14, when this phone stayed red for the full interval after its
            // tunnel came back and only cleared on a manual relaunch.
            var failures = 0
            while !Task.isCancelled {
                let outcome = await self?.beatNow() ?? .skipped
                failures = outcome.nextFailureCount(after: failures)
                let delay = Heartbeat.nextDelay(consecutiveFailures: failures)
                try? await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000_000)
            }
        }
    }

    /// Sends only while started: a stopped app is not going to record, and saying
    /// "alive" for it would paint the state we most want to see as healthy.
    ///
    /// Reports which of the three it was, because `skipped` must not drive the retry
    /// backoff — see `Heartbeat.Outcome`.
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

    // MARK: polling — capture state long-polls (held by the server until it
    // changes; ~RTT latency), fleet liveness every 1.5s (Android cadence), and —
    // like Android — only while the UI is actually visible.

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
                // Long-poll: the request hangs on the server until the household
                // state changes (a press on any client, the mic confirming), so
                // changes land in ~RTT. An older server (no stateToken) answers at
                // once → plain 5s poll; so does an unreachable one (failed call).
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
