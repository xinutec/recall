import SwiftUI

/// The one screen, as on Android: status, mic level, the recording window, the
/// devices, the settings and Start/Stop.
struct ContentView: View {
    @ObservedObject var state: MicState
    var onStart: () -> Void
    var onStop: () -> Void
    var onPress: (Press) -> Void
    var onHostChanged: () -> Void

    @State private var host: String = Prefs.host
    @State private var controlHost: String = Prefs.controlHost
    @State private var ingestToken: String = Prefs.ingestToken
    /// The "until" picker's time; the next one after now when pressed.
    @State private var untilTime = Date(timeIntervalSinceNow: 3600)
    private let segments = 24

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 20) {
                    statusCard
                    meter
                    captureBanner
                    devicesPanel
                    hostField
                    controlHostField
                    ingestTokenField
                    deviceRow
                    buttons
                }
                .padding()
            }
            .navigationTitle("Recall Mic")
        }
    }

    // MARK: status + meter

    private var statusCard: some View {
        HStack(spacing: 12) {
            Circle().fill(dotColor).frame(width: 12, height: 12)
            Text(state.statusText).font(.headline)
            Spacer()
        }
        .padding()
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private var meter: some View {
        let lit = Int(state.level * Float(segments))
        return HStack(spacing: 3) {
            ForEach(0..<segments, id: \.self) { i in
                RoundedRectangle(cornerRadius: 2)
                    .fill(segmentColor(Levels.tier(index: i, lit: lit, segments: segments)))
                    .frame(height: 24)
            }
        }
        .opacity(state.connected ? 1 : 0.4)
    }

    // MARK: the recording window (shown whenever the API is reachable)

    /// The windows one press opens, as the web app's menu.
    private let spans: [(String, TimeInterval)] = [("30 min", 1800), ("1 h", 3600), ("2 h", 7200)]

    @ViewBuilder private var captureBanner: some View {
        if state.capture.reachable {
            // The desired window, with "Starting…" etc. until the mic confirms.
            let window = state.capture.desiredWindow?.at(Date())
            let recording = window == nil || { if case .record = window { true } else { false } }()
            VStack(alignment: .leading, spacing: 10) {
                // Every 30 s, for the minute countdown.
                TimelineView(.periodic(from: .now, by: 30)) { ctx in
                    Text(bannerTitle(now: ctx.date)).font(.headline)
                }
                // Enabled mid-transition: a press just replaces the desired state.
                if window != .off {
                    HStack(spacing: 12) {
                        switch window {
                        case .record:
                            Button("Pause") { onPress(.pause) }
                                .buttonStyle(.bordered).frame(maxWidth: .infinity)
                        case .hold:
                            Button("Unpause") { onPress(.unpause) }
                                .buttonStyle(.borderedProminent).frame(maxWidth: .infinity)
                        default:
                            EmptyView()
                        }
                        Button("Stop") { onPress(.stop) }
                            .buttonStyle(.bordered).frame(maxWidth: .infinity)
                    }
                }
                Text(window == .off ? "Record for" : "Record from now for").font(.subheadline)
                HStack(spacing: 8) {
                    ForEach(spans, id: \.0) { label, seconds in
                        Button(label) {
                            onPress(.record(until: Date(timeIntervalSinceNow: seconds)))
                        }
                        .buttonStyle(.bordered).frame(maxWidth: .infinity)
                    }
                }
                HStack(spacing: 12) {
                    DatePicker("Until", selection: $untilTime, displayedComponents: .hourAndMinute)
                    Button("Record") {
                        onPress(.record(until: Self.next(untilTime, after: Date())))
                    }
                    .buttonStyle(.bordered)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .background((recording ? Color.red : Color.gray).opacity(0.15))
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }

    private func bannerTitle(now: Date) -> String {
        let capture = state.capture
        guard capture.micReachable else {
            return "Recorder not reporting — state unconfirmed"
        }
        let window = capture.desiredWindow?.at(now)
        if capture.micReachable && !capture.settled {
            switch window {
            case .hold: return "Pausing…"
            case .off: return "Stopping…"
            default: return "Starting…"
            }
        }
        return Banner.windowText(window, now: now, timeZone: .current)
    }

    /// The picked time of day, next after `now`: today, or tomorrow once it has passed.
    static func next(_ picked: Date, after now: Date) -> Date {
        let calendar = Calendar.current
        let hm = calendar.dateComponents([.hour, .minute], from: picked)
        return calendar.nextDate(after: now, matching: hm, matchingPolicy: .nextTime) ?? picked
    }

    // MARK: devices fleet panel

    @ViewBuilder private var devicesPanel: some View {
        if !state.sources.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Devices").font(.subheadline).bold()
                ForEach(state.sources) { s in
                    HStack(spacing: 10) {
                        Circle()
                            .fill(s.active ? Color.green : Color.secondary.opacity(0.3))
                            .frame(width: 10, height: 10)
                        Text(s.name)
                            .fontWeight(s.id == state.deviceID ? .bold : .regular)
                        if s.id == state.deviceID {
                            Text("this device").font(.caption2).foregroundStyle(Color.accentColor)
                        }
                        Spacer()
                        Text(activityLabel(s)).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .background(Color(.secondarySystemBackground))
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }

    private func activityLabel(_ s: SourceStatus) -> String {
        if s.active { return "active" }
        guard let la = s.lastActive else { return "no signal" }
        let secs = Int(Date().timeIntervalSince(la))
        if secs < 60 { return "\(secs)s ago" }
        if secs < 3600 { return "\(secs / 60)m ago" }
        return "\(secs / 3600)h ago"
    }

    // MARK: host / device / buttons

    private var hostField: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Recorder host (stream)").font(.caption).foregroundStyle(.secondary)
            TextField("192.168.1.81", text: $host)
                .textFieldStyle(.roundedBorder)
                .keyboardType(.numbersAndPunctuation)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .disabled(state.running)
                .onChange(of: host) { newValue in
                    Prefs.host = newValue
                    onHostChanged()
                }
        }
    }

    // Isis, for the recording window and the devices panel.
    private var controlHostField: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Control host (Isis)").font(.caption).foregroundStyle(.secondary)
            TextField(Prefs.defaultControlHost, text: $controlHost)
                .textFieldStyle(.roundedBorder)
                .keyboardType(.numbersAndPunctuation)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .onChange(of: controlHost) { newValue in
                    Prefs.controlHost = newValue
                    onHostChanged()
                }
        }
    }

    // The bearer for this phone's segments, write-only and for this source alone.
    private var ingestTokenField: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Segment token").font(.caption).foregroundStyle(.secondary)
            TextField("", text: $ingestToken)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .onChange(of: ingestToken) { newValue in
                    Prefs.ingestToken = newValue.trimmingCharacters(in: .whitespaces)
                }
        }
    }

    private var deviceRow: some View {
        HStack {
            Text("This device").foregroundStyle(.secondary)
            Spacer()
            Text(state.deviceID).font(.system(.footnote, design: .monospaced))
        }
        .font(.subheadline)
    }

    private var buttons: some View {
        HStack(spacing: 12) {
            Button(action: onStart) {
                Text("Start").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(state.running || host.trimmingCharacters(in: .whitespaces).isEmpty)

            Button(action: onStop) {
                Text("Stop").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .disabled(!state.running)
        }
    }

    private var dotColor: Color {
        switch state.phase {
        case .streaming: return .green
        case .off: return .orange
        case .waitingForHost: return .red
        case .stopped: return .gray
        }
    }

    private func segmentColor(_ tier: Levels.MeterTier) -> Color {
        switch tier {
        case .off: return Color(.tertiarySystemFill)
        case .low: return .green
        case .mid: return .orange
        case .high: return .red
        }
    }
}
