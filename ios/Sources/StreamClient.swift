import Foundation
import Network

/// Streams the mic to the recorder host over TCP, and stays alive in the background.
///
/// iOS keeps a background app running only while it holds an active audio session, so
/// the mic is captured the whole time the client is on, and PCM is forwarded only
/// while connected. Paused or unreachable, it keeps capturing and retries every 2 s,
/// so a pause on the server takes effect here as it does on Android.
///
/// Loop: connect (5 s timeout), handshake, forward PCM; on a drop, wait 2 s and retry.
final class StreamClient {
    private let state: MicState
    private let audio = AudioCapture()
    private let queue = DispatchQueue(label: "org.recall.mic.stream")
    private var loop: Task<Void, Never>?
    private var watchdog: Task<Void, Never>?
    private static let spoolSeconds = 60
    private var drainer: Task<Void, Never>?
    /// 60 s of audio between capture and network: rides out a busy host or a Wi-Fi
    /// stall. Sized from the capture rate.
    private let spool = PcmSpool(
        capacityBytes: Int(AudioCapture.sampleRate) * 2 * StreamClient.spoolSeconds)

    // Nil when not connected. Read from the audio thread, so locked.
    private let connLock = NSLock()
    private var connection: NWConnection?

    private let connectTimeout: UInt64 = 5
    private let reconnectDelayNs: UInt64 = 2_000_000_000

    /// Local segments of the same PCM, delivered by `SegmentUpload`. Written only
    /// while connected: the mic runs even while paused, and the connection is what
    /// means at home and not paused.
    private let segments = SegmentWriter(source: Prefs.deviceID)

    init(state: MicState) {
        self.state = state
        segments.onSegmentClosed = { SegmentUpload.kick() }
    }

    /// Start capturing (until `stop()`) and the connect loop; false if the mic did not
    /// open.
    func start() -> Bool {
        guard loop == nil else { return true }
        do {
            try audio.start(
                // Capture only fills the spool; the drain task sends.
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

    /// If capture stops delivering buffers (an interruption that never ended, a
    /// wedged route), restart it and zero the meter rather than freeze it. See
    /// Watchdog.
    private func watch() async {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if Watchdog.isStalled(lastBufferAt: audio.lastBufferAt, now: Date()) {
                audio.kick()
                await MainActor.run { state.level = 0 }
            }
        }
    }

    /// Send what capture spooled, while connected.
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
                await MainActor.run { state.droppedBytes = spool.dropped }
            }
        }
    }

    // MARK: - main loop

    private func run() async {
        while !Task.isCancelled {
            if let conn = await connect(host: Prefs.host, port: Prefs.port) {
                // The handshake first (sends are in order), then publish the
                // connection so PCM flows.
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

            // Not connected: ask Isis whether this is a pause.
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

    /// Send a PCM block if connected; otherwise drop it.
    @discardableResult
    private func sendIfConnected(_ data: Data) -> Bool {
        connLock.lock()
        let conn = connection
        connLock.unlock()
        // A send error ends a TCP stream: cancel, so the loop reconnects at once.
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

    /// Returns when the connection drops (a pause closes the listener, or the network).
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
