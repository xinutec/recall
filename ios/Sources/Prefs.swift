import Foundation

/// Persisted settings, mirroring the Android `Prefs` (UserDefaults instead of
/// SharedPreferences). The device id is derived once and kept forever.
enum Prefs {
    private static let d = UserDefaults.standard
    private enum Key {
        static let host = "host"
        static let port = "port"
        static let enabled = "enabled"
        static let deviceID = "device_id"
    }

    /// Recorder host (IP or hostname). Empty until the user sets it.
    static var host: String {
        get { d.string(forKey: Key.host) ?? "" }
        set { d.set(newValue, forKey: Key.host) }
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
