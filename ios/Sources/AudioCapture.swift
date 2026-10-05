import AVFoundation

/// Captures the mic as 48 kHz mono s16le PCM, the ingest's format, and hands each
/// block to a callback.
///
/// `.measurement` mode turns off automatic gain and noise processing, like Android's
/// `UNPROCESSED`.
final class AudioCapture {
    /// 48 kHz, mono, Int16 (little-endian on iOS).
    static let sampleRate: Double = 48_000

    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private let target = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: AudioCapture.sampleRate,
        channels: 1, interleaved: true)!

    private var onPCM: ((Data) -> Void)?
    private var onLevel: ((Float) -> Void)?
    private var running = false

    // For the watchdog. Written on the audio thread, read on the main actor, so locked.
    private let bufferLock = NSLock()
    private var lastBufferAtLocked: Date?

    /// When the mic last delivered a buffer (nil before the first one after start).
    var lastBufferAt: Date? {
        bufferLock.lock()
        defer { bufferLock.unlock() }
        return lastBufferAtLocked
    }

    /// Ask for mic access, on iOS 16 and 17+.
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

        // 2048 frames, tens of milliseconds, so the meter stays lively.
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
        // A route change or a media-services reset can stop input with no
        // interruption notification.
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleRouteChange),
            name: AVAudioSession.routeChangeNotification, object: session)
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleMediaReset),
            name: AVAudioSession.mediaServicesWereResetNotification, object: session)
    }

    /// Reactivate the session and restart a stalled engine. Safe to repeat; the
    /// watchdog retries a failure.
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
            // Whatever `.shouldResume` says: an engine left paused is a silent source
            // that looks live. If another app holds the session, the watchdog
            // retries.
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
        // The engine can wedge on the old route.
        if reason == .oldDeviceUnavailable || reason == .newDeviceAvailable {
            engine.stop()
            kick()
        }
    }

    @objc private func handleMediaReset(_ note: Notification) {
        // The media daemon restarted, invalidating every audio object: rebuild.
        guard running, let pcm = onPCM, let level = onLevel else { return }
        stop()
        try? start(onPCM: pcm, onLevel: level)
    }
}
