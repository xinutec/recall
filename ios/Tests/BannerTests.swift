import XCTest

@testable import RecallMic

final class BannerTests: XCTestCase {
    // The reference: the website's banner reads
    // "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)".
    // The phones must match it byte-for-byte (durationUntil + dayKey HH:mm).
    private let london = TimeZone(identifier: "Europe/London")!

    private func date(_ iso: String) -> Date {
        let f = ISO8601DateFormatter()
        return f.date(from: iso)!
    }

    func testMatchesWebsitePhrasingWithHoursAndMinutes() {
        let until = date("2026-07-04T07:30:00Z")  // 08:30 BST
        let now = date("2026-07-04T02:07:00Z")  // 03:07 BST
        XCTAssertEqual(
            "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)",
            Banner.pausedText(pausedUntil: until, now: now, timeZone: london))
    }

    func testMinutesOnlyWhenUnderAnHour() {
        let until = date("2026-07-04T07:30:00Z")
        let now = date("2026-07-04T07:07:00Z")
        XCTAssertEqual(
            "Recording paused — auto-resumes in 23m (by 2026-07-04 08:30)",
            Banner.pausedText(pausedUntil: until, now: now, timeZone: london))
    }

    func testReadsNowWhenDeadlinePassed() {
        // Never a negative countdown — a past resume time reads "now", as on the web.
        let until = date("2026-07-04T07:30:00Z")
        let now = date("2026-07-04T09:00:00Z")
        XCTAssertEqual(
            "Recording paused — auto-resumes in now (by 2026-07-04 08:30)",
            Banner.pausedText(pausedUntil: until, now: now, timeZone: london))
    }

    func testBareWhenNoResumeTime() {
        let now = date("2026-07-04T09:00:00Z")
        XCTAssertEqual(
            "Recording paused", Banner.pausedText(pausedUntil: nil, now: now, timeZone: london))
    }
}
