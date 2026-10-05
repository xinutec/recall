import XCTest

@testable import RecallMic

/// The cases of Android's `PcmSpoolTest.kt`.
final class PcmSpoolTests: XCTestCase {
    func testDeliversWhatWasCapturedInOrder() {
        let spool = PcmSpool(capacityBytes: 64)
        spool.offer(Data([1, 2, 3]))
        spool.offer(Data([4, 5]))
        XCTAssertEqual(spool.drain(), Data([1, 2, 3, 4, 5]))
        XCTAssertEqual(spool.dropped, 0)
    }

    func testCaptureNeverBlocksWhenTheSenderStalls() {
        // A stalled host never blocks the mic, and the spool stays bounded.
        let spool = PcmSpool(capacityBytes: 8)
        for _ in 0..<100 { spool.offer(Data(repeating: 7, count: 4)) }
        XCTAssertLessThanOrEqual(spool.count, 8)
    }

    func testAnOverrunDropsTheOldestAudioAndSaysHowMuch() {
        // Drops are counted, since only the phone knows. Oldest first: the newest
        // speech is the most likely to be looked for.
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
