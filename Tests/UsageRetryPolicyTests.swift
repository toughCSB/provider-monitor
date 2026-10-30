import XCTest
@testable import ProviderMonitor

final class UsageRetryPolicyTests: XCTestCase {
    func testSwiftAndWindowsShareRetryAfterAndDeadlineExamples() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let bytes = try Data(contentsOf: root.appendingPathComponent("windows/tests/fixtures/polling_policy.json"))
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: bytes) as? [String: Any])
        for item in try XCTUnwrap(fixture["retry_after"] as? [[String: Any]]) {
            let now = Date(timeIntervalSince1970: (item["now_ms"] as! NSNumber).doubleValue / 1000)
            let expected = (item["expected_secs"] as! NSNumber).doubleValue
            let fallback = (item["fallback_secs"] as! NSNumber).doubleValue
            let header = item["header"] as? String
            let parsed = UsageRetryPolicy.retryAfter(header: header, at: now)
            let actual = (parsed ?? 0) > 0 ? parsed! : fallback
            XCTAssertEqual(actual, expected, "header=\(header ?? "nil")")
        }
        for item in try XCTUnwrap(fixture["deadline"] as? [[String: Any]]) {
            let now = Date(timeIntervalSince1970: (item["now_ms"] as! NSNumber).doubleValue / 1000)
            let deadline = Date(timeIntervalSince1970: (item["deadline_ms"] as! NSNumber).doubleValue / 1000)
            XCTAssertEqual(UsageRetryPolicy.remaining(until: deadline, at: now), (item["expected_secs"] as! NSNumber).doubleValue)
        }
    }

    func testServerDeadlineIsNotShortenedByTheLocalBackoffCap() {
        XCTAssertEqual(UsageRetryPolicy.backoff(attempt: 20, retryAfter: 3600), 3600)
        XCTAssertEqual(UsageRetryPolicy.backoff(attempt: 20, retryAfter: nil), 900)
        XCTAssertEqual(UsageRetryPolicy.backoff(attempt: 0, retryAfter: 0), 60)
        XCTAssertNil(UsageRetryPolicy.retryAfter(header: "inf"))
        XCTAssertNil(UsageRetryPolicy.retryAfter(header: "NaN"))
    }
}
