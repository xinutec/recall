import XCTest

@testable import RecallMic

/// Mirrors the Android `HandshakeTest.kt` cases so the two announcements can't
/// drift apart.
final class HandshakeTests: XCTestCase {
    func testCarriesIdRateChannelsAndEpoch() {
        let data = Handshake.line(id: "iphone11", rate: 48000, epoch: 1_756_900_000.25)
        let line = String(decoding: data, as: UTF8.self)
        XCTAssertEqual(
            line,
            "{\"id\":\"iphone11\",\"rate\":48000,\"channels\":1,\"epoch\":1756900000.250}\n")
    }

    func testEpochIsPlainDecimalNeverExponentNotation() {
        let data = Handshake.line(id: "iphone11", rate: 48000, epoch: 1_756_900_000.007)
        let line = String(decoding: data, as: UTF8.self)
        XCTAssertFalse(line.contains("E"), line)
        XCTAssertTrue(line.contains("\"epoch\":1756900000.007"), line)
    }
}
