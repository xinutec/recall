import XCTest

@testable import RecallMic

/// Mirrors the Android `LevelsTest.kt` cases so the two meters can't drift apart.
final class LevelsTests: XCTestCase {
    func testSilenceReadsZero() {
        XCTAssertEqual(Levels.meter(fromSamples: [Int16](repeating: 0, count: 32)), 0)
    }

    func testFullScaleReadsOne() {
        XCTAssertEqual(Levels.meter(fromSamples: [32767]), 1, accuracy: 1e-3)
    }

    func testBelowFloorReadsZero() {
        // Amplitude 1 is about -90 dBFS, below the -70 floor.
        XCTAssertEqual(Levels.meter(fromSamples: [1]), 0)
    }

    func testFarFieldSpeechLandsMidMeter() {
        // ~-44 dBFS (a loud distant voice) should sit visibly mid-meter, not at
        // the bottom — the point of the dBFS scale.
        let amp = Int16(32768.0 * pow(10.0, -44.0 / 20.0))
        let level = Levels.meter(fromSamples: [amp])
        XCTAssertGreaterThan(level, 0.3)
        XCTAssertLessThan(level, 0.5)
    }

    func testTierMatchesAndroidBoundaries() {
        // Android meterTier: OFF at/after `lit`, HIGH above 85%, MID above 60%.
        let segments = 24
        XCTAssertEqual(Levels.tier(index: 5, lit: 5, segments: segments), .off)
        XCTAssertEqual(Levels.tier(index: 5, lit: 24, segments: segments), .low)
        XCTAssertEqual(Levels.tier(index: 14, lit: 24, segments: segments), .low)  // 14 < 14.4
        XCTAssertEqual(Levels.tier(index: 15, lit: 24, segments: segments), .mid)  // > 60%
        XCTAssertEqual(Levels.tier(index: 20, lit: 24, segments: segments), .mid)  // 20 < 20.4
        XCTAssertEqual(Levels.tier(index: 21, lit: 24, segments: segments), .high)  // > 85%
    }
}
