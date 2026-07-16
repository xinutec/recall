import SwiftUI

/// Single-screen UI mirroring the Android app: status card, live mic-level meter,
/// household pause banner (Pause / Still-away / Resume), the Devices fleet panel,
/// host field, device id, and Start/Stop.
struct ContentView: View {
    @ObservedObject var state: MicState
    var onStart: () -> Void
    var onStop: () -> Void
    var onPause: () -> Void  // "Pause recording" and "Still away (24h)" (snooze re-pauses)
    var onResume: () -> Void
    var onHostChanged: () -> Void

    @State private var host: String = Prefs.host
    @State private var controlHost: String = Prefs.controlHost
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

    // MARK: household pause banner (shown whenever the API is reachable)

    @ViewBuilder private var captureBanner: some View {
        if state.capture.reachable {
            // The banner follows the DESIRED state, with an explicit in-between while
            // the mic hasn't confirmed — a press can't flap back on the next poll.
            let paused = !state.capture.desiredRunning
            let transitioning = state.capture.micReachable && !state.capture.settled
            VStack(alignment: .leading, spacing: 10) {
                // TimelineView ticks the "auto-resumes in Xh Ym" countdown every 30s
                // without a manual timer (minute-granularity text stays within a minute).
                TimelineView(.periodic(from: .now, by: 30)) { ctx in
                    Text(bannerTitle(paused: paused, transitioning: transitioning, now: ctx.date))
                        .font(.headline)
                }
                // Buttons stay enabled while transitioning: intent is cheap and
                // idempotent, so pressing again (or the other way) just overwrites
                // the target — always abortable, never locked out.
                if paused {
                    HStack(spacing: 12) {
                        Button("Still away (24h)", action: onPause)
                            .buttonStyle(.bordered).frame(maxWidth: .infinity)
                        Button("Resume now", action: onResume)
                            .buttonStyle(.borderedProminent).frame(maxWidth: .infinity)
                    }
                } else {
                    Button("Pause recording", action: onPause)
                        .buttonStyle(.bordered).frame(maxWidth: .infinity)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .background((paused ? Color.orange : Color.gray).opacity(0.15))
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
    }

    private func bannerTitle(paused: Bool, transitioning: Bool, now: Date) -> String {
        guard state.capture.micReachable else {
            return "Recorder not reporting — state unconfirmed"
        }
        if transitioning { return paused ? "Pausing…" : "Resuming…" }
        guard paused else { return "Recording active" }
        return Banner.pausedText(
            pausedUntil: state.capture.pausedUntil, now: now, timeZone: .current)
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

    // The control-plane host (Isis). Separate from the recorder host because the Isis
    // split put the capture API on a different machine than the PCM ingest; this drives
    // the pause banner and Devices panel. Editable any time — it doesn't touch the stream.
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
        case .paused: return .orange
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
