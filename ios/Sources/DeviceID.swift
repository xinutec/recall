import Foundation

/// Auto-derived per-device source id, mirroring the Android app's `Prefs`
/// derivation: `<sanitised-model>-<8-hex-suffix>`, lowercased, non-alphanumerics
/// folded to hyphens, capped at 40 chars. Used only when `Prefs.presetID` is nil —
/// this build ships a fixed `"iphone11"` for the one dedicated device (source
/// continuity, like the Pixels' `pixel9`/`pixel5`); a SECOND iOS device must get a
/// different preset (or nil, to auto-derive) or it would merge into the first's
/// recording history. A handshake id becomes a source id (and a directory name) on
/// the recorder, so it must be stable and filesystem-safe.
enum DeviceID {
    /// Hardware model identifier, e.g. "iPhone15,2".
    static func hardwareModel() -> String {
        var info = utsname()
        uname(&info)
        let machine = withUnsafeBytes(of: &info.machine) { raw -> String in
            let bytes = raw.prefix { $0 != 0 }
            return String(decoding: bytes, as: UTF8.self)
        }
        return machine.isEmpty ? "iphone" : machine
    }

    /// Filesystem/handshake-safe id: lowercase, [a-z0-9] kept, everything else a
    /// hyphen, collapsed and trimmed, capped at 40 characters.
    static func sanitize(_ raw: String) -> String {
        var out = ""
        var lastHyphen = false
        for ch in raw.lowercased() {
            if ch.isLetter || ch.isNumber {
                out.append(ch)
                lastHyphen = false
            } else if !lastHyphen {
                out.append("-")
                lastHyphen = true
            }
        }
        let trimmed = out.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        return String(trimmed.prefix(40))
    }

    /// A fresh id: sanitised model + an 8-char random hex suffix (cap respected).
    static func generate() -> String {
        let suffix = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased().prefix(
            8)
        let base = sanitize(hardwareModel())
        let capped = String(base.prefix(40 - 1 - suffix.count))
        return "\(capped)-\(suffix)"
    }
}
