import XCTest

@testable import RecallMic

final class WatchdogTests: XCTestCase {
    private let t0 = Date(timeIntervalSince1970: 1_000_000)

    func testNoBufferYetIsAGracePeriodNotAStall() {
        XCTAssertFalse(Watchdog.isStalled(lastBufferAt: nil, now: t0))
    }

    func testFreshBuffersAreHealthy() {
        XCTAssertFalse(Watchdog.isStalled(lastBufferAt: t0.addingTimeInterval(-1), now: t0))
    }

    func testSilenceBeyondThresholdIsAStall() {
        XCTAssertTrue(Watchdog.isStalled(lastBufferAt: t0.addingTimeInterval(-6), now: t0))
    }
}
