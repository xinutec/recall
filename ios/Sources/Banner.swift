import Foundation

/// The paused-banner text, identical to the web app's (`app.html`, `format.ts`):
/// "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)".
enum Banner {
    static func pausedText(pausedUntil: Date?, now: Date, timeZone: TimeZone) -> String {
        guard let until = pausedUntil else { return "Recording paused" }
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH:mm"
        f.timeZone = timeZone
        let by = f.string(from: until)
        return "Recording paused — auto-resumes in \(remaining(until: until, now: now)) (by \(by))"
    }

    /// Time left as "5h 23m", "23m" or "now", as format.ts `durationUntil`.
    private static func remaining(until: Date, now: Date) -> String {
        let mins = max(0, Int((until.timeIntervalSince(now) / 60).rounded()))
        if mins == 0 { return "now" }
        let h = mins / 60
        let m = mins % 60
        return h > 0 ? "\(h)h \(m)m" : "\(m)m"
    }
}
