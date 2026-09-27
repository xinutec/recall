import Foundation

/// The base URL of a recall API. A value with a scheme is used as written (the fleet,
/// `https://recall.xinutec.org`); a bare host is `http://<host>:8000`, the shape the
/// Mac's LAN beat relay answers.
enum ApiBase {
    private static let relayPort = 8000  // `recall beat-relay`, the LAN backstop

    static func of(_ hostOrUrl: String) -> String {
        if hostOrUrl.contains("://") {
            var url = hostOrUrl
            while url.hasSuffix("/") { url.removeLast() }
            return url
        }
        return "http://\(hostOrUrl):\(relayPort)"
    }
}
