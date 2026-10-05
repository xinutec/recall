import Foundation

/// A derived source id, as on Android: `<model>-<8 hex>`, lowercased, other
/// characters folded to hyphens, at most 40. Used only without `Prefs.presetID`; a
/// second iOS device needs another preset, or it would merge into the first's
/// source. The id is also a directory name on the recorder.
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
