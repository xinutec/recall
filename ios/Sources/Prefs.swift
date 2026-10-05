import Foundation

/// Saved settings, as Android's `Prefs`, in UserDefaults.
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

    /// Isis, a name served on the VPN only. Read through `ApiBase`, which also takes a
    /// bare host.
    static let defaultControlHost = "https://recall.xinutec.org"

    /// The previous default (#1799), stored by installs that edited the field; read as
    /// unset.
    private static let oldDefaultControlHost = "10.100.0.2"

    /// The recorder the PCM stream goes to (the Mac, on the home LAN); empty until set.
    static var host: String {
        get { d.string(forKey: Key.host) ?? "" }
        set { d.set(newValue, forKey: Key.host) }
    }

    /// The capture-API host (Isis); `defaultControlHost` when unset.
    static var controlHost: String {
        get { effectiveControlHost(d.string(forKey: Key.controlHost) ?? "") }
        set { d.set(newValue, forKey: Key.controlHost) }
    }

    /// A stored control host; unset or the old default means `defaultControlHost`.
    static func effectiveControlHost(_ stored: String) -> String {
        stored.isEmpty || stored == oldDefaultControlHost ? defaultControlHost : stored
    }

    /// recalld's ingest, the same server. Not a setting.
    static let ingestBase = "https://recall.xinutec.org"

    /// The bearer for `PUT`ting this phone's own segments to recalld, and nothing
    /// else. Empty sends no header.
    static var ingestToken: String {
        get { d.string(forKey: Key.ingestToken) ?? "" }
        set { d.set(newValue, forKey: Key.ingestToken) }
    }

    /// The ingest port all devices share.
    static var port: Int {
        get {
            let p = d.integer(forKey: Key.port)
            return p == 0 ? 9999 : p
        }
        set { d.set(newValue, forKey: Key.port) }
    }

    /// Whether streaming should run: set by Start and Stop, read at launch.
    static var enabled: Bool {
        get { d.bool(forKey: Key.enabled) }
        set { d.set(newValue, forKey: Key.enabled) }
    }

    /// A fixed source id for this device; nil derives `<model>-<random>` on first run.
    static let presetID: String? = "iphone11"

    /// The source id: the preset, else made on first read and kept.
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
