import Foundation
import Network

/// Streams the mic to the recall ingester over TCP — and stays alive in the background.
///
/// iOS keeps an app running in the background only while it holds an *active audio
/// session*, so the mic is captured for the WHOLE time the client is "on", not just
/// while connected. PCM is forwarded only while a connection is up; when paused or
/// unreachable the capture keeps running (so iOS doesn't suspend us) and the loop
/// reconnects every 2 s. That is what lets the recorder enable/disable this device
/// **over the network**: pause/resume on the server and the still-alive app follows —
/// the same model as the Android mics. Pause vs. unreachable is read from `/api/capture`.
///
/// Loop: connect (5 s timeout) → handshake → forward captured PCM → on drop, sleep 2 s,
/// retry. The mic itself never stops until `stop()`.
final class StreamClient {
    private let state: MicState
    private let audio = AudioCapture()
    private let queue = DispatchQueue(label: "org.recall.mic.stream")
    private var loop: Task<Void, Never>?
    private var watchdog: Task<Void, Never>?
    private static let spoolSeconds = 60
    private var drainer: Task<Void, Never>?
    /// Bounded capture-to-network hand-off (PcmSpool) — 60s of audio, enough to ride
    /// out a busy host or a Wi-Fi stall without the mic ever pausing. Derived from
    /// the capture rate rather than written as a literal: a hardcoded 16000 here
    /// silently made this a TWENTY-second spool against a 48 kHz stream.
    private let spool = PcmSpool(
        capacityBytes: Int(AudioCapture.sampleRate) * 2 * StreamClient.spoolSeconds)

    // The live connection, or nil when not connected. Read from the audio thread, so
    // access is guarded by a lock.
    private let connLock = NSLock()
    private var connection: NWConnection?

    private let connectTimeout: UInt64 = 5
    private let reconnectDelayNs: UInt64 = 2_000_000_000

    /// Store-and-forward shadow (docs/architecture.md, stage C2): the same PCM
    /// lands in capture-stamped local segments delivered with verified
    /// receipts. Gated on the CONNECTION, exactly like Android's C1: iOS keeps
    /// this mic hot even while paused (audio discarded by design), and the
    /// connection is the one signal that means at-home + unpaused + wanted.
    private let segments = SegmentWriter(source: Prefs.deviceID)

    init(state: MicState) {
        self.state = state
        segments.onSegmentClosed = { SegmentUpload.kick() }
    }

    /// Begin capturing (held until `stop()`) and start the connect/stream loop.
    /// Returns false if the mic couldn't be opened, so the caller can reflect that.
    func start() -> Bool {
        guard loop == nil else { return true }
        do {
            try audio.start(
                // Capture hands frames to the spool and returns — it never waits on
                // the network. A separate drain task does the sending, so a busy or
                // unreachable host can never reach back into the microphone.
                onPCM: { [weak self] data in self?.spool.offer(data) },
                onLevel: { [weak self] level in
                    Task { @MainActor in self?.state.level = level }
                })
        } catch {
            return false
        }
        loop = Task { await run() }
        watchdog = Task { await watch() }
        drainer = Task { await drain() }
        return true
    }

    func stop() {
        segments.closeSegment()
        SegmentUpload.kick()
        loop?.cancel()
        loop = nil
        watchdog?.cancel()
        watchdog = nil
        drainer?.cancel()
        drainer = nil
        audio.stop()
        setConnection(nil)
    }

    /// The silent-mic watchdog: if the capture engine stops delivering buffers
    /// (an interruption that never resumed, a wedged route), kick it and zero the
    /// meter so the stall shows as silence instead of a frozen level. See Watchdog.
    private func watch() async {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if Watchdog.isStalled(lastBufferAt: audio.lastBufferAt, now: Date()) {
                audio.kick()
                await MainActor.run { state.level = 0 }
            }
        }
    }

    /// Send whatever capture has spooled, whenever a connection is up. Runs
    /// independently of capture, so the microphone is never waiting on the network.
    private func drain() async {
        while !Task.isCancelled {
            let pending = spool.drain()
            if pending.isEmpty {
                try? await Task.sleep(nanoseconds: 20_000_000)
                continue
            }
            let connected = sendIfConnected(pending)
            if connected { segments.offer(pending) }
            if spool.dropped > 0 {
                // The phone is the only place that can know audio was lost here,
                // so it must be visible rather than silently absent.
                await MainActor.run { state.droppedBytes = spool.dropped }
            }
        }
    }

    // MARK: - main loop

    private func run() async {
        while !Task.isCancelled {
            if let conn = await connect(host: Prefs.host, port: Prefs.port) {
                // Handshake first (sends are FIFO on the connection, so it precedes any
                // PCM), then publish the connection so captured audio starts flowing.
                conn.send(
                    content: Handshake.line(
                        id: Prefs.deviceID, rate: 48000,
                        epoch: Date().timeIntervalSince1970),
                    completion: .contentProcessed { [weak conn] err in
                        if err != nil { conn?.cancel() }
                    })
                setConnection(conn)
                await set(connected: true, phase: .streaming)
                await waitUntilClosed(conn)
                setConnection(nil)
                // A segment never spans the gap the mic just fell into.
                segments.closeSegment()
                SegmentUpload.kick()
                await set(connected: false, phase: nil)
            }
            if Task.isCancelled { break }

            // Not connected — ask the control plane (Isis) whether this is a deliberate
            // pause (vs the recorder host just being unreachable).
            let cap = await CaptureApi.state(host: Prefs.controlHost)
            await MainActor.run {
                state.capture = cap
                state.phase = (cap.reachable && !cap.running) ? .paused : .waitingForHost
            }
            try? await Task.sleep(nanoseconds: reconnectDelayNs)
        }
        await set(connected: false, phase: .stopped)
    }

    // MARK: - connection

    /// Forward a captured PCM block if a connection is currently up; otherwise drop it.
    @discardableResult
    private func sendIfConnected(_ data: Data) -> Bool {
        connLock.lock()
        let conn = connection
        connLock.unlock()
        // A send error on a TCP stream is fatal for the connection — cancel it so
        // the main loop notices immediately and reconnects, instead of pumping PCM
        // into a dead pipe until some later state change.
        conn?.send(
            content: data,
            completion: .contentProcessed { [weak conn] err in
                if err != nil { conn?.cancel() }
            })
        return conn != nil
    }

    private func setConnection(_ conn: NWConnection?) {
        connLock.lock()
        let old = connection
        connection = conn
        connLock.unlock()
        if old !== conn { old?.cancel() }
    }

    private func connect(host: String, port: Int) async -> NWConnection? {
        guard !host.isEmpty, port > 0, port <= 65_535,
            let nwPort = NWEndpoint.Port(rawValue: UInt16(port))
        else { return nil }

        let params = NWParameters.tcp
        if let tcp = params.defaultProtocolStack.transportProtocol as? NWProtocolTCP.Options {
            tcp.noDelay = true
        }
        let conn = NWConnection(host: NWEndpoint.Host(host), port: nwPort, using: params)

        return await withCheckedContinuation { (cont: CheckedContinuation<NWConnection?, Never>) in
            var resumed = false
            let finish: (NWConnection?) -> Void = { result in
                if resumed { return }
                resumed = true
                cont.resume(returning: result)
            }
            conn.stateUpdateHandler = { st in
                switch st {
                case .ready: finish(conn)
                case .failed, .cancelled: finish(nil)
                default: break
                }
            }
            conn.start(queue: queue)
            queue.asyncAfter(deadline: .now() + .seconds(Int(connectTimeout))) {
                if !resumed {
                    conn.cancel()
                    finish(nil)
                }
            }
        }
    }

    /// Resolve once the connection drops (server pause closes the listener / network loss).
    private func waitUntilClosed(_ conn: NWConnection) async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            var resumed = false
            conn.stateUpdateHandler = { st in
                switch st {
                case .failed, .cancelled:
                    if !resumed {
                        resumed = true
                        cont.resume()
                    }
                default: break
                }
            }
        }
    }

    private func set(connected: Bool, phase: MicPhase?) async {
        await MainActor.run {
            state.connected = connected
            if let phase { state.phase = phase }
        }
    }
}
