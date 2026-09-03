import XCTest

@testable import RecallMic

/// Mirrors the Android `PcmSpoolTest.kt` cases so the two recorders cannot drift
/// apart in what they do when the host cannot keep up.
final class PcmSpoolTests: XCTestCase {
    func testDeliversWhatWasCapturedInOrder() {
        let spool = PcmSpool(capacityBytes: 64)
        spool.offer(Data([1, 2, 3]))
        spool.offer(Data([4, 5]))
        XCTAssertEqual(spool.drain(), Data([1, 2, 3, 4, 5]))
        XCTAssertEqual(spool.dropped, 0)
    }

    func testCaptureNeverBlocksWhenTheSenderStalls() {
        // The point of the type: a stalled Mac must never stop the phone reading
        // its own microphone, and the spool must stay bounded while it happens.
        let spool = PcmSpool(capacityBytes: 8)
        for _ in 0..<100 { spool.offer(Data(repeating: 7, count: 4)) }
        XCTAssertLessThanOrEqual(spool.count, 8)
    }

    func testAnOverrunDropsTheOldestAudioAndSaysHowMuch() {
        // Dropping is a real loss either way, so it must be COUNTED — the phone is
        // the only place that knows. Oldest-first: in a memory aid the newest
        // speech is what someone is most likely to come looking for.
        let spool = PcmSpool(capacityBytes: 4)
        spool.offer(Data([1, 2, 3, 4]))
        spool.offer(Data([5, 6]))
        XCTAssertEqual(spool.drain(), Data([3, 4, 5, 6]))
        XCTAssertEqual(spool.dropped, 2)
    }

    func testDrainEmptiesSoTheNextDrainSeesOnlyNewAudio() {
        let spool = PcmSpool(capacityBytes: 64)
        spool.offer(Data([1, 2]))
        _ = spool.drain()
        spool.offer(Data([3]))
        XCTAssertEqual(spool.drain(), Data([3]))
    }

    func testAChunkLargerThanTheWholeSpoolKeepsItsTail() {
        let spool = PcmSpool(capacityBytes: 3)
        spool.offer(Data([1, 2, 3, 4, 5]))
        XCTAssertEqual(spool.drain(), Data([3, 4, 5]))
        XCTAssertEqual(spool.dropped, 2)
    }
}
