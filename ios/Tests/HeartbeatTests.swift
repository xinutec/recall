import XCTest

@testable import RecallMic

/// The beat's body is a contract with the server's `HeartbeatIn`, so it is built by
/// a pure function and pinned here — a renamed key would otherwise fail silently,
/// as a field the server defaults rather than as an error anyone sees.
final class HeartbeatTests: XCTestCase {
    private let started = ISO8601DateFormatter().date(from: "2026-08-11T07:00:00Z")!

    private func body(streaming: Bool = true, charging: Bool? = true) -> [String: Any] {
        Heartbeat.body(
            device: "iphone11", version: "1.4.0 (37)", startedAt: started,
            streaming: streaming, charging: charging)
    }

    func testCarriesTheFieldsTheServerReads() {
        let b = body()
        XCTAssertEqual(b["device"] as? String, "iphone11")
        XCTAssertEqual(b["app"] as? String, "ios")
        XCTAssertEqual(b["version"] as? String, "1.4.0 (37)")
        XCTAssertEqual(b["startedAt"] as? String, "2026-08-11T07:00:00Z")
        XCTAssertEqual(b["streaming"] as? Bool, true)
        XCTAssertEqual(b["charging"] as? Bool, true)
    }

    func testAPausedHouseholdStillBeats() {
        // The whole point: capture is normally paused for days, and the app being
        // alive through that is the fact nothing else in recall could report.
        XCTAssertEqual(body(streaming: false)["streaming"] as? Bool, false)
    }

    func testUnknownChargeIsOmittedRatherThanGuessed() {
        // The simulator and a device with battery monitoring off both say `.unknown`.
        // Sending "discharging" there would invent the one reading a mains-powered
        // room phone is watched for.
        XCTAssertNil(body(charging: nil)["charging"])
        XCTAssertEqual(body(charging: false)["charging"] as? Bool, false)
    }

    func testTheBodyIsValidJSON() {
        XCTAssertTrue(JSONSerialization.isValidJSONObject(body()))
        XCTAssertNotNil(try? JSONSerialization.data(withJSONObject: body()))
    }

    func testStartedAtIsFixedForTheProcess() {
        // "Alive now" and "alive since Tuesday" are different answers, and only the
        // second tells a stable app from one relaunching between beats.
        XCTAssertEqual(Heartbeat.startedAt, Heartbeat.startedAt)
    }

    func testTheCadenceMatchesWhatTheGraderWasToldToExpect() {
        // recall.mic_alive.BEAT_EVERY_MINUTES is 60; the fleetwatch thresholds are
        // written as multiples of it. Drifting apart here would silently make every
        // threshold describe a cadence nothing sends.
        XCTAssertEqual(Heartbeat.every, 3600)
    }
}
