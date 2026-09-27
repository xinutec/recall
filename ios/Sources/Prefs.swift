import Foundation

/// Persisted settings, mirroring the Android `Prefs` (UserDefaults instead of
/// SharedPreferences). The device id is derived once and kept forever.
enum Prefs {
    private static let d = UserDefaults.standard
    private enum Key {
        static let host = "host"
        static let controlHost = "control_host"
        static let port = "port"
        static let enabled = "enabled"
        static let deviceID = "device_id"
        static let ingestToken = "ingest_token"
    }

    /// Isis (the fleet control plane), a name its front door serves on the VPN only:
    /// the out-of-the-box default. The stream still goes to the recorder `host`; only
    /// the API moved here. Read through `ApiBase`, which also takes a bare host.
    static let defaultControlHost = "https://recall.xinutec.org"

    /// The default before the front door (#1799). Stored on installs that edited the
    /// field, so it reads as "unset" rather than as a choice.
    private static let oldDefaultControlHost = "10.100.0.2"

    /// Recorder host the PCM stream connects to (the Mac's ingest, on the home LAN).
    /// Empty until the user sets it.
    static var host: String {
        get { d.string(forKey: Key.host) ?? "" }
        set { d.set(newValue, forKey: Key.host) }
    }

    /// Control-plane host for the capture API — pause/resume and the fleet liveness the
    /// Devices panel shows. That's Isis, not the recorder host: the Isis split put the API
    /// and the PCM ingest on different machines. Empty/unset falls back to
    /// `defaultControlHost`, so the controls and panel work out of the box.
    static var controlHost: String {
        get { effectiveControlHost(d.string(forKey: Key.controlHost) ?? "") }
        set { d.set(newValue, forKey: Key.controlHost) }
    }

    /// What a stored control setting means: unset and the old default both mean the fleet.
    static func effectiveControlHost(_ stored: String) -> String {
        stored.isEmpty || stored == oldDefaultControlHost ? defaultControlHost : stored
    }

    /// recalld's ingest plane (docs/architecture.md): the same server as the
    /// control plane. Not user-set until a reason appears.
    static let ingestBase = "https://recall.xinutec.org"

    /// The fourth credential plane's per-device bearer: `PUT` this phone's own
    /// segments to recalld, and nothing else. Empty = send no header.
    static var ingestToken: String {
        get { d.string(forKey: Key.ingestToken) ?? "" }
        set { d.set(newValue, forKey: Key.ingestToken) }
    }

    /// Shared ingest port (matches the recorder's DEFAULT_INGEST_PORT).
    static var port: Int {
        get {
            let p = d.integer(forKey: Key.port)
            return p == 0 ? 9999 : p
        }
        set { d.set(newValue, forKey: Key.port) }
    }

    /// Whether the user last pressed Start — drives auto-resume on launch.
    static var enabled: Bool {
        get { d.bool(forKey: Key.enabled) }
        set { d.set(newValue, forKey: Key.enabled) }
    }

    /// Pre-set fixed source id for this dedicated device — like the Pixels' `pixel9`/
    /// `pixel5`. Set to nil to auto-derive `<model>-<random>` on first run instead.
    static let presetID: String? = "iphone11"

    /// Stable source id; the pre-set value if any, else generated and stored on first read.
    static var deviceID: String {
        if let preset = presetID {
            if d.string(forKey: Key.deviceID) != preset { d.set(preset, forKey: Key.deviceID) }
            return preset
        }
        if let existing = d.string(forKey: Key.deviceID), !existing.isEmpty {
            return existing
        }
        let fresh = DeviceID.generate()
        d.set(fresh, forKey: Key.deviceID)
        return fresh
    }
}
