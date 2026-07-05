import AVFoundation

/// Captures the mic as 48 kHz mono signed-16-bit little-endian PCM — the exact format
/// the recall ingester expects — and hands each block to a callback as raw bytes.
///
/// `.measurement` mode disables the system's AGC / noise processing, the closest iOS
/// equivalent to Android's `UNPROCESSED` audio source, so the recorder gets clean audio.
final class AudioCapture {
    /// Target wire format: 48 kHz, mono, Int16 interleaved (little-endian on iOS).
    static let sampleRate: Double = 48_000

    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private let target = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: AudioCapture.sampleRate,
        channels: 1, interleaved: true)!

    private var onPCM: ((Data) -> Void)?
    private var onLevel: ((Float) -> Void)?
    private var running = false

    // When the tap last delivered audio — the watchdog's staleness signal. Written
    // on the audio thread, read from the main actor, so guarded by a lock.
    private let bufferLock = NSLock()
    private var lastBufferAtLocked: Date?

    /// When the mic last delivered a buffer (nil before the first one after start).
    var lastBufferAt: Date? {
        bufferLock.lock()
        defer { bufferLock.unlock() }
        return lastBufferAtLocked
    }

    /// Ask for mic access (works across iOS 16/17+).
    static func requestPermission() async -> Bool {
        if #available(iOS 17.0, *) {
            return await AVAudioApplication.requestRecordPermission()
        } else {
            return await withCheckedContinuation { cont in
                AVAudioSession.sharedInstance().requestRecordPermission {
                    cont.resume(returning: $0)
                }
            }
        }
    }

    func start(onPCM: @escaping (Data) -> Void, onLevel: @escaping (Float) -> Void) throws {
        guard !running else { return }
        self.onPCM = onPCM
        self.onLevel = onLevel

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: [])
        try session.setActive(true, options: [])

        let input = engine.inputNode
        let inFormat = input.outputFormat(forBus: 0)
        converter = AVAudioConverter(from: inFormat, to: target)

        // ~2048 frames per tap keeps the level meter responsive (~tens of ms).
        input.installTap(onBus: 0, bufferSize: 2048, format: inFormat) { [weak self] buf, _ in
            self?.handle(buf)
        }

        engine.prepare()
        try engine.start()
        running = true
        bufferLock.lock()
        lastBufferAtLocked = nil
        bufferLock.unlock()

        NotificationCenter.default.addObserver(
            self, selector: #selector(handleInterruption),
            name: AVAudioSession.interruptionNotification, object: session)
        // A route change (wired mic unplugged, Bluetooth device gone) or a media-
        // services reset can stop input without any interruption notification.
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleRouteChange),
            name: AVAudioSession.routeChangeNotification, object: session)
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleMediaReset),
            name: AVAudioSession.mediaServicesWereResetNotification, object: session)
    }

    /// Try to bring a stalled engine back (the watchdog's lever): reactivate the
    /// session and restart the engine. Safe to call repeatedly; a failure is left
    /// for the next watchdog tick to retry.
    func kick() {
        guard running else { return }
        try? AVAudioSession.sharedInstance().setActive(true)
        if !engine.isRunning {
            try? engine.start()
        }
    }

    func stop() {
        guard running else { return }
        running = false
        NotificationCenter.default.removeObserver(self)
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        try? AVAudioSession.sharedInstance().setActive(
            false, options: [.notifyOthersOnDeactivation])
        onPCM = nil
        onLevel = nil
    }

    // MARK: - private

    private func handle(_ input: AVAudioPCMBuffer) {
        guard let converter else { return }
        let ratio = target.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }

        var supplied = false
        var err: NSError?
        converter.convert(to: out, error: &err) { _, status in
            if supplied {
                status.pointee = .noDataNow
                return nil
            }
            supplied = true
            status.pointee = .haveData
            return input
        }
        if err != nil || out.frameLength == 0 { return }
        bufferLock.lock()
        lastBufferAtLocked = Date()
        bufferLock.unlock()

        guard let ch = out.int16ChannelData else { return }
        let count = Int(out.frameLength)
        let bytes = count * MemoryLayout<Int16>.size
        let data = Data(bytes: ch[0], count: bytes)
        onPCM?(data)

        // Peak for the level meter, straight off the converted samples.
        let samples = UnsafeBufferPointer(start: ch[0], count: count)
        var peak: Int32 = 0
        for s in samples {
            let a = Int32(s).magnitude
            if Int32(a) > peak { peak = Int32(a) }
        }
        let level = Levels.meter(fromPeak: Float(peak) / 32768.0)
        onLevel?(level)
    }

    @objc private func handleInterruption(_ note: Notification) {
        guard
            let info = note.userInfo,
            let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }

        switch type {
        case .began:
            engine.pause()
        case .ended:
            // Restart REGARDLESS of `.shouldResume`: this is a dedicated always-on
            // mic, and an un-resumed engine is a silent "Streaming" source — the
            // recorder's worst failure. If another app still holds the session the
            // start fails quietly here and the watchdog keeps retrying.
            kick()
        @unknown default:
            break
        }
    }

    @objc private func handleRouteChange(_ note: Notification) {
        guard
            let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
            let reason = AVAudioSession.RouteChangeReason(rawValue: raw)
        else { return }
        // Losing the active input (or a new one appearing) can leave the engine
        // wedged on a dead route — restart it on the new one.
        if reason == .oldDeviceUnavailable || reason == .newDeviceAvailable {
            engine.stop()
            kick()
        }
    }

    @objc private func handleMediaReset(_ note: Notification) {
        // The media daemon restarted: every audio object is invalid. Tear down and
        // rebuild the whole capture path with the stored callbacks.
        guard running, let pcm = onPCM, let level = onLevel else { return }
        stop()
        try? start(onPCM: pcm, onLevel: level)
    }
}
