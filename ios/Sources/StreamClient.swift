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

    // The live connection, or nil when not connected. Read from the audio thread, so
    // access is guarded by a lock.
    private let connLock = NSLock()
    private var connection: NWConnection?

    private let connectTimeout: UInt64 = 5
    private let reconnectDelayNs: UInt64 = 2_000_000_000

    init(state: MicState) { self.state = state }

    /// Begin capturing (held until `stop()`) and start the connect/stream loop.
    /// Returns false if the mic couldn't be opened, so the caller can reflect that.
    func start() -> Bool {
        guard loop == nil else { return true }
        do {
            try audio.start(
                onPCM: { [weak self] data in self?.sendIfConnected(data) },
                onLevel: { [weak self] level in
                    Task { @MainActor in self?.state.level = level }
                })
        } catch {
            return false
        }
        loop = Task { await run() }
        watchdog = Task { await watch() }
        return true
    }

    func stop() {
        loop?.cancel()
        loop = nil
        watchdog?.cancel()
        watchdog = nil
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

    // MARK: - main loop

    private func run() async {
        while !Task.isCancelled {
            if let conn = await connect(host: Prefs.host, port: Prefs.port) {
                // Handshake first (sends are FIFO on the connection, so it precedes any
                // PCM), then publish the connection so captured audio starts flowing.
                conn.send(
                    content: handshakeLine(id: Prefs.deviceID),
                    completion: .contentProcessed { [weak conn] err in
                        if err != nil { conn?.cancel() }
                    })
                setConnection(conn)
                await set(connected: true, phase: .streaming)
                await waitUntilClosed(conn)
                setConnection(nil)
                await set(connected: false, phase: nil)
            }
            if Task.isCancelled { break }

            // Not connected — ask the recorder whether this is a deliberate pause.
            let cap = await CaptureApi.state(host: Prefs.host)
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
    private func sendIfConnected(_ data: Data) {
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

    private func handshakeLine(id: String) -> Data {
        // Field order and trailing newline must match the recorder's parser exactly.
        Data("{\"id\":\"\(id)\",\"rate\":48000,\"channels\":1}\n".utf8)
    }

    private func set(connected: Bool, phase: MicPhase?) async {
        await MainActor.run {
            state.connected = connected
            if let phase { state.phase = phase }
        }
    }
}
