import XCTest

@testable import RecallMic

/// The fleet moved from `http://10.100.0.2:8000` to `https://recall.xinutec.org` (#1799),
/// while the Mac's LAN beat relay still answers a bare host on :8000. One setting holds
/// either, so how it becomes a URL is pinned here, as on Android.
final class ApiBaseTests: XCTestCase {
    func testAValueWithASchemeIsUsedAsWritten() {
        XCTAssertEqual(ApiBase.of("https://recall.xinutec.org"), "https://recall.xinutec.org")
        XCTAssertEqual(ApiBase.of("https://recall.xinutec.org/"), "https://recall.xinutec.org")
    }

    func testABareHostIsTheRelayShapeOn8000() {
        XCTAssertEqual(ApiBase.of("192.168.1.20"), "http://192.168.1.20:8000")
    }

    func testAnUnsetOrOldDefaultControlSettingBecomesTheFleetName() {
        XCTAssertEqual(Prefs.effectiveControlHost(""), Prefs.defaultControlHost)
        XCTAssertEqual(Prefs.effectiveControlHost("10.100.0.2"), Prefs.defaultControlHost)
        XCTAssertEqual(Prefs.defaultControlHost, "https://recall.xinutec.org")
    }

    func testAHostSomebodyChoseIsKept() {
        XCTAssertEqual(Prefs.effectiveControlHost("192.168.1.20"), "192.168.1.20")
    }
}
