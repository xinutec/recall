import XCTest

@testable import RecallMic

/// The beat's body must match the server's `HeartbeatIn`: a renamed key would not
/// fail there, it would read as the field's default.
final class HeartbeatTests: XCTestCase {
    private let started = ISO8601DateFormatter().date(from: "2026-08-11T07:00:00Z")!

    private func body(
        streaming: Bool = true, charging: Bool? = true, micOk: Bool = true, droppedBytes: Int = 0
    ) -> [String: Any] {
        Heartbeat.body(
            device: "iphone11", version: "1.4.0 (37)", startedAt: started,
            streaming: streaming, charging: charging, micOk: micOk, droppedBytes: droppedBytes)
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
        // Capture is often paused for days; the beat is the only sign the app is alive.
        XCTAssertEqual(body(streaming: false)["streaming"] as? Bool, false)
    }

    func testUnknownChargeIsOmittedRatherThanGuessed() {
        // `.unknown` (the simulator, or monitoring off) must not read as discharging,
        // which is what a mains-powered phone is watched for.
        XCTAssertNil(body(charging: nil)["charging"])
        XCTAssertEqual(body(charging: false)["charging"] as? Bool, false)
    }

    func testTheBodyIsValidJSON() {
        XCTAssertTrue(JSONSerialization.isValidJSONObject(body()))
        XCTAssertNotNil(try? JSONSerialization.data(withJSONObject: body()))
    }

    func testStartedAtIsFixedForTheProcess() {
        // Tells a stable app from one relaunching between beats.
        XCTAssertEqual(Heartbeat.startedAt, Heartbeat.startedAt)
    }

    func testADeafAppSaysSoInsteadOfFallingSilent() {
        // #887: a failed `client.start()` used to clear `Prefs.enabled`, which also
        // stopped the beat that would have reported it.
        XCTAssertEqual(body(micOk: false)["micOk"] as? Bool, false)
        XCTAssertEqual(body()["micOk"] as? Bool, true)
    }

    func testAudioTheSpoolDroppedIsReported() {
        XCTAssertEqual(body(droppedBytes: 96000)["droppedBytes"] as? Int, 96000)
        XCTAssertEqual(body()["droppedBytes"] as? Int, 0)
    }

    func testABeatReportsTheDropsSinceTheLastBeatThatLanded() {
        var drops = Heartbeat.DropsSinceBeat()
        XCTAssertEqual(drops.pending(total: 500), 500)
        // Failed: still owed.
        XCTAssertEqual(drops.pending(total: 800), 800)
        drops.landed(total: 800)
        XCTAssertEqual(drops.pending(total: 800), 0)
        XCTAssertEqual(drops.pending(total: 900), 100)
    }

    func testTheVPNIsTriedBeforeTheLANSoTheFallbackStaysABackstop() {
        // #888: a phone at home with its tunnel off records fine but read as dead.
        // The LAN is the fallback, not the normal path; the relay marks beats it
        // carried.
        XCTAssertEqual(
            Heartbeat.hostsToTry(control: "10.100.0.2", lan: "192.168.1.81"),
            ["10.100.0.2", "192.168.1.81"])
        // A blank host is skipped, not tried.
        XCTAssertEqual(Heartbeat.hostsToTry(control: "", lan: "192.168.1.81"), ["192.168.1.81"])
        XCTAssertEqual(Heartbeat.hostsToTry(control: "10.100.0.2", lan: ""), ["10.100.0.2"])
        XCTAssertEqual(Heartbeat.hostsToTry(control: "", lan: ""), [])
        XCTAssertEqual(
            Heartbeat.hostsToTry(control: "10.100.0.2", lan: "10.100.0.2"), ["10.100.0.2"])
    }

    func testALandedBeatWaitsTheFullHour() {
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 0), Heartbeat.every)
    }

    func testAFailedBeatRetriesSoonNotAtTheNextHourMark() {
        // #886: the loop slept the full hour after a failure too, so a phone read as
        // silent for an hour after its tunnel came back.
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 1), 60)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 2), 120)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 3), 240)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 4), 480)
    }

    func testALongOutageCostsNoMoreThanTheHourlyCadence() {
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 7), Heartbeat.every)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 64), Heartbeat.every)
        // Only a success resets the counter, so it can grow without bound.
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: .max), Heartbeat.every)
    }

    func testAnOutageCostsAFewExtraRequestsThenSettles() {
        // Bounds the number of retries before the hourly cap, not the time they span.
        var delays: [TimeInterval] = []
        var n = 1
        while Heartbeat.nextDelay(consecutiveFailures: n) < Heartbeat.every {
            delays.append(Heartbeat.nextDelay(consecutiveFailures: n))
            n += 1
        }
        XCTAssertLessThanOrEqual(delays.count, 8, "an outage costs \(delays.count) retries")
        // Each wait is at least the one before.
        XCTAssertEqual(delays, delays.sorted())
    }

    func testASkippedBeatIsNotAFailure() {
        // A stopped app is not an unreachable server, so it does not back off.
        XCTAssertEqual(Heartbeat.Outcome.skipped.nextFailureCount(after: 0), 0)
        XCTAssertEqual(Heartbeat.Outcome.skipped.nextFailureCount(after: 3), 3)
        XCTAssertEqual(Heartbeat.Outcome.sent.nextFailureCount(after: 3), 0)
        XCTAssertEqual(Heartbeat.Outcome.failed.nextFailureCount(after: 3), 4)
    }

    func testTheCadenceMatchesWhatTheGraderWasToldToExpect() {
        // recalld::devices::BEAT_EVERY_MINUTES; fleetwatch's thresholds are multiples
        // of it.
        XCTAssertEqual(Heartbeat.every, 3600)
    }
}
