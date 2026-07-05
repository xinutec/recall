import XCTest

@testable import RecallMic

final class DeviceIDTests: XCTestCase {
    func testSanitizeFoldsToFilesystemSafe() {
        XCTAssertEqual(DeviceID.sanitize("iPhone12,1"), "iphone12-1")
        XCTAssertEqual(DeviceID.sanitize("  Weird__Name!! "), "weird-name")
        XCTAssertEqual(DeviceID.sanitize("---"), "")
    }

    func testSanitizeCapsLength() {
        XCTAssertLessThanOrEqual(DeviceID.sanitize(String(repeating: "a", count: 100)).count, 40)
    }

    func testGenerateShape() {
        let id = DeviceID.generate()
        XCTAssertLessThanOrEqual(id.count, 40)
        // <sanitised-model>-<8 hex chars>
        let suffix = id.split(separator: "-").last.map(String.init) ?? ""
        XCTAssertEqual(suffix.count, 8)
        XCTAssertNotEqual(DeviceID.generate(), id)  // random suffix differs
    }
}
