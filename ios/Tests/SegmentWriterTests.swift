import AVFoundation
import XCTest

@testable import RecallMic

/// The writer's files are what the uploader sends, so they are read back as files.
final class SegmentWriterTests: XCTestCase {
    private let source = "test-\(UUID().uuidString.prefix(8))"

    override func tearDownWithError() throws {
        for dir in [SegmentStore.root(), SegmentStore.open()] {
            for f in try FileManager.default.contentsOfDirectory(atPath: dir.path)
            where f.hasPrefix(source) {
                try FileManager.default.removeItem(at: dir.appendingPathComponent(f))
            }
        }
    }

    func testAFullMinuteClosesAsAPhoneFlacHoldingExactlyThatMinute() throws {
        let writer = SegmentWriter(source: source)
        var closed = 0
        writer.onSegmentClosed = { closed += 1 }
        let pcm = (0..<SegmentWriter.sampleRate * 90).map { Int16(truncatingIfNeeded: $0 * 13) }
        var bytes = Data(capacity: pcm.count * 2)
        for s in pcm { withUnsafeBytes(of: s.littleEndian) { bytes.append(contentsOf: $0) } }

        // In stream-sized pieces, as the drain task offers them.
        for at in stride(from: 0, to: bytes.count, by: 19_200) {
            writer.offer(bytes.subdata(in: at..<min(at + 19_200, bytes.count)))
        }

        XCTAssertEqual(closed, 1, "the second minute is still open")
        let files = try FileManager.default.contentsOfDirectory(atPath: SegmentStore.root().path)
            .filter { $0.hasPrefix(source) }
        XCTAssertEqual(files.count, 1)
        let name = try XCTUnwrap(files.first)
        XCTAssertTrue(name.hasSuffix(".phone.flac"), name)
        let file = try AVAudioFile(
            forReading: SegmentStore.root().appendingPathComponent(name),
            commonFormat: .pcmFormatInt16, interleaved: true)
        let buffer = try XCTUnwrap(
            AVAudioPCMBuffer(
                pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length)))
        try file.read(into: buffer)
        let decoded = Array(
            UnsafeBufferPointer(
                start: buffer.int16ChannelData?[0], count: Int(buffer.frameLength)))
        XCTAssertEqual(decoded, Array(pcm.prefix(SegmentWriter.sampleRate * 60)))
    }
}
