import XCTest

@testable import RecallMic

final class BannerTests: XCTestCase {
    // The web app's banner (`app.html`), which the phones match exactly.
    private let london = TimeZone(identifier: "Europe/London")!

    private func date(_ iso: String) -> Date {
        let f = ISO8601DateFormatter()
        return f.date(from: iso)!
    }

    private var end: Date { date("2026-07-04T07:30:00Z") }  // 08:30 BST

    func testAnOpenWindowSaysItsEndAndTheTimeLeft() {
        let now = date("2026-07-04T02:07:00Z")  // 03:07 BST
        XCTAssertEqual(
            "Recording until 08:30 · 5h 23m left",
            Banner.windowText(.record(until: end), now: now, timeZone: london))
    }

    func testAnEndOnAnotherDayCarriesItsDate() {
        let now = date("2026-07-03T22:00:00Z")  // 23:00 BST, the day before
        XCTAssertEqual(
            "Paused · the window ends 2026-07-04 08:30",
            Banner.windowText(.hold(until: end), now: now, timeZone: london))
    }

    func testOffAndAFaultSaySo() {
        let now = date("2026-07-04T02:07:00Z")
        XCTAssertEqual("Not recording", Banner.windowText(.off, now: now, timeZone: london))
        // A window past its end is off.
        XCTAssertEqual(
            "Not recording", Banner.windowText(.record(until: end), now: end, timeZone: london))
        XCTAssertEqual(
            "Recording with no window set — a recorder fault",
            Banner.windowText(nil, now: now, timeZone: london))
    }

    func testTheDesiredHalfNamesTheWindow() {
        var state = CaptureState(running: false, reachable: true)
        XCTAssertEqual(.off, state.desiredWindow)
        state.desiredUntil = end
        XCTAssertEqual(.record(until: end), state.desiredWindow)
        state.desiredHeld = true
        XCTAssertEqual(.hold(until: end), state.desiredWindow)
        state.desiredUntil = nil
        state.desiredRunning = true
        XCTAssertNil(state.desiredWindow)
    }
}
