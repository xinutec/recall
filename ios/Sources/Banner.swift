import Foundation

/// The recording banner's text, as the web app's (`app.html`): "Recording until 11:00 ·
/// 45m left", "Paused · the window ends 11:00", with the date when the end is not today.
enum Banner {
    /// For the window asked for; nil is a fault, which records.
    static func windowText(_ window: Window?, now: Date, timeZone: TimeZone) -> String {
        guard let window else { return "Recording with no window set — a recorder fault" }
        switch window.at(now) {
        case .off:
            return "Not recording"
        case .record(let until):
            let ends = endsAt(until, now: now, timeZone: timeZone)
            return "Recording until \(ends) · \(remaining(until: until, now: now)) left"
        case .hold(let until):
            return "Paused · the window ends \(endsAt(until, now: now, timeZone: timeZone))"
        }
    }

    /// The end's time, with the date unless it is today.
    private static func endsAt(_ until: Date, now: Date, timeZone: TimeZone) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let f = DateFormatter()
        f.timeZone = timeZone
        f.dateFormat = calendar.isDate(until, inSameDayAs: now) ? "HH:mm" : "yyyy-MM-dd HH:mm"
        return f.string(from: until)
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
