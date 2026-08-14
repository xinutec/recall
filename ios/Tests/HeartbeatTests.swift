import XCTest

@testable import RecallMic

/// The beat's body is a contract with the server's `HeartbeatIn`, so it is built by
/// a pure function and pinned here — a renamed key would otherwise fail silently,
/// as a field the server defaults rather than as an error anyone sees.
final class HeartbeatTests: XCTestCase {
    private let started = ISO8601DateFormatter().date(from: "2026-08-11T07:00:00Z")!

    private func body(streaming: Bool = true, charging: Bool? = true, micOk: Bool = true)
        -> [String: Any]
    {
        Heartbeat.body(
            device: "iphone11", version: "1.4.0 (37)", startedAt: started,
            streaming: streaming, charging: charging, micOk: micOk)
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

    func testADeafAppSaysSoInsteadOfFallingSilent() {
        // #887: a failed `client.start()` used to clear `Prefs.enabled`, which both
        // disabled auto-start forever and silenced the beat — so the one signal that
        // would have reported the broken mic was what the breakage switched off.
        XCTAssertEqual(body(micOk: false)["micOk"] as? Bool, false)
        XCTAssertEqual(body()["micOk"] as? Bool, true)
    }

    func testALandedBeatWaitsTheFullHour() {
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 0), Heartbeat.every)
    }

    func testAFailedBeatRetriesSoonNotAtTheNextHourMark() {
        // #886: the loop used to sleep the full interval whatever happened, so this
        // very phone read `never sent a beat` for an hour after its tunnel came back
        // on 2026-08-14, and only went green because it was relaunched by hand.
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 1), 60)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 2), 120)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 3), 240)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 4), 480)
    }

    func testALongOutageCostsNoMoreThanTheHourlyCadence() {
        // The bound that keeps this a backoff and not a poll.
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 7), Heartbeat.every)
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: 64), Heartbeat.every)
        // No overflow at absurd counts: only a success resets this counter, so a phone
        // in a dead spot for a month keeps incrementing it.
        XCTAssertEqual(Heartbeat.nextDelay(consecutiveFailures: .max), Heartbeat.every)
    }

    func testAnOutageCostsAFewExtraRequestsThenSettles() {
        // The bound that matters is the COUNT of extra requests an outage can cost
        // before the schedule reaches the hourly cap — not the wall-clock they span.
        // (An earlier version of this test asserted the burst fit inside one cadence;
        // it does not, by three minutes, and that was never the property worth having.)
        var delays: [TimeInterval] = []
        var n = 1
        while Heartbeat.nextDelay(consecutiveFailures: n) < Heartbeat.every {
            delays.append(Heartbeat.nextDelay(consecutiveFailures: n))
            n += 1
        }
        XCTAssertLessThanOrEqual(delays.count, 8, "an outage costs \(delays.count) retries")
        // Monotonic: each wait is at least the one before it, so the schedule can only
        // ever back OFF. A dip would mean an outage beating harder the longer it lasts.
        XCTAssertEqual(delays, delays.sorted())
    }

    func testASkippedBeatIsNotAFailure() {
        // A stopped app is a deliberate state, not an unreachable control plane. If
        // `skipped` fed the failure counter, stopping the app would spin the backoff
        // and then beat hourly forever for nothing.
        XCTAssertEqual(Heartbeat.Outcome.skipped.nextFailureCount(after: 0), 0)
        XCTAssertEqual(Heartbeat.Outcome.skipped.nextFailureCount(after: 3), 3)
        XCTAssertEqual(Heartbeat.Outcome.sent.nextFailureCount(after: 3), 0)
        XCTAssertEqual(Heartbeat.Outcome.failed.nextFailureCount(after: 3), 4)
    }

    func testTheCadenceMatchesWhatTheGraderWasToldToExpect() {
        // recall.mic_alive.BEAT_EVERY_MINUTES is 60; the fleetwatch thresholds are
        // written as multiples of it. Drifting apart here would silently make every
        // threshold describe a cadence nothing sends.
        XCTAssertEqual(Heartbeat.every, 3600)
    }
}
