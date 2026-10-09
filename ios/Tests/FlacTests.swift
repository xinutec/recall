import AVFoundation
import CryptoKit
import XCTest

@testable import RecallMic

/// Lossless means the decoded samples are the fed ones, bit for bit. Decoded by
/// Apple's FLAC decoder, not by code of ours.
final class FlacTests: XCTestCase {
    private var dir: URL!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    func testSpeechLikeAudioDecodesToTheSameSamples() throws {
        var rng = Lcg(seed: 7)
        let pcm = (0..<48_000 * 3).map { i in
            Int16(
                8000 * sin(2 * Double.pi * 220 * Double(i) / 48_000) + Double(rng.next(-300, 300)))
        }
        try roundTrip(pcm)
    }

    func testSilenceFullScaleNoiseAndAShortTailDecodeExactly() throws {
        var rng = Lcg(seed: 11)
        try roundTrip([Int16](repeating: 0, count: Flac.block * 2 + 300))
        try roundTrip((0..<Flac.block + 17).map { _ in Int16(rng.next(-32768, 32768)) })
        try roundTrip((0..<3).map { Int16($0 * 1000) })
        try roundTrip([Int16.min, Int16.max, Int16.min, Int16.max, 0])
    }

    func testASampleSplitAcrossTwoWritesIsPutBackTogether() throws {
        let pcm = (0..<5000).map { Int16($0 * 7 - 9000) }
        let bytes = Self.bytes(pcm)
        let url = dir.appendingPathComponent("split.flac")
        let flac = try FlacFile(creating: url, rate: 48_000)
        var at = 0
        var size = 1
        while at < bytes.count {
            let take = min(size, bytes.count - at)
            flac.write(bytes.subdata(in: at..<(at + take)))
            at += take
            size = size % 7 + 1
        }
        flac.finish()
        XCTAssertEqual(try Self.decode(url), pcm)
    }

    func testAMinuteOfSpeechLikeAudioIsFarSmallerThanItsWAV() throws {
        var rng = Lcg(seed: 3)
        let pcm = (0..<48_000 * 60).map { i in
            Int16(3000 * sin(2 * Double.pi * 180 * Double(i) / 48_000) + Double(rng.next(-20, 20)))
        }
        let size = try Data(contentsOf: encode(pcm)).count
        XCTAssertLessThan(size, pcm.count * 2 / 2, "\(size) bytes")
    }

    func testTheHeaderCarriesTheMD5OfTheSamples() throws {
        let pcm = (0..<Flac.block + 5).map { Int16($0 % 300) }
        let header = try Data(contentsOf: encode(pcm)).prefix(Flac.headerBytes)
        XCTAssertEqual(Data(header.suffix(16)), Data(Insecure.MD5.hash(data: Self.bytes(pcm))))
    }

    // MARK: -

    private func roundTrip(_ pcm: [Int16], file: StaticString = #filePath, line: UInt = #line)
        throws
    {
        XCTAssertEqual(try Self.decode(encode(pcm)), pcm, file: file, line: line)
    }

    private func encode(_ pcm: [Int16]) throws -> URL {
        let url = dir.appendingPathComponent("\(UUID().uuidString).flac")
        let flac = try FlacFile(creating: url, rate: 48_000)
        flac.write(Self.bytes(pcm))
        flac.finish()
        return url
    }

    private static func bytes(_ pcm: [Int16]) -> Data {
        var out = Data(capacity: pcm.count * 2)
        for s in pcm { withUnsafeBytes(of: s.littleEndian) { out.append(contentsOf: $0) } }
        return out
    }

    private static func decode(_ url: URL) throws -> [Int16] {
        let file = try AVAudioFile(
            forReading: url, commonFormat: .pcmFormatInt16, interleaved: true)
        XCTAssertEqual(file.fileFormat.sampleRate, 48_000)
        XCTAssertEqual(file.fileFormat.channelCount, 1)
        guard
            let buffer = AVAudioPCMBuffer(
                pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length))
        else { return [] }
        try file.read(into: buffer)
        guard let samples = buffer.int16ChannelData?[0] else { return [] }
        return Array(UnsafeBufferPointer(start: samples, count: Int(buffer.frameLength)))
    }
}

/// A fixed-seed generator, so a failure reproduces.
private struct Lcg {
    private var state: UInt64

    init(seed: UInt64) { state = seed }

    /// In `lo..<hi`.
    mutating func next(_ lo: Int, _ hi: Int) -> Int {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        return lo + Int((state >> 33) % UInt64(hi - lo))
    }
}
