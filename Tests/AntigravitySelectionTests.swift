import XCTest
@testable import ProviderMonitor

final class AntigravitySelectionTests: XCTestCase {
    func testMissingSelectedFamilyDoesNotBorrowAnotherFamilysLimits() {
        let provider = AntigravityProvider(localQuota: { nil }, headlineModel: { .gemini })
        let windows = [
            LimitWindow(id: "3p-hourly", label: "Claude and GPT models", usedFraction: 0.8,
                        duration: 5 * 3600),
            LimitWindow(id: "3p-weekly", label: "Weekly Limit", usedFraction: 0.9,
                        duration: 7 * 86400)
        ]

        XCTAssertEqual(provider.resolveHeadlineID(for: windows), "gemini-hourly")
        XCTAssertEqual(provider.resolveRingWindow(in: windows, cadence: .weekly), "gemini-hourly")
        XCTAssertNil(provider.resolveWeeklyID(for: windows))
        XCTAssertEqual(provider.ringCadences(in: windows), [.automatic])
    }

    func testGenericRequestCountRemainsAvailableWithoutModelLimits() {
        let provider = AntigravityProvider(localQuota: { nil })
        let windows = [LimitWindow(id: "requests", label: "Requests", used: 3)]
        XCTAssertEqual(provider.resolveHeadlineID(for: windows), "requests")
    }

    func testWeeklyCadenceDoesNotCrossToOtherFamilyWhenSelectedFamilyHasOnlyHourly() {
        let provider = AntigravityProvider(localQuota: { nil }, headlineModel: { .gemini })
        let windows = [
            LimitWindow(id: "gemini-hourly", label: "Gemini Models", usedFraction: 0.3,
                        duration: 5 * 3600),
            LimitWindow(id: "3p-weekly", label: "Weekly Limit", usedFraction: 0.9,
                        duration: 7 * 86400)
        ]
        XCTAssertEqual(provider.resolveRingWindow(in: windows, cadence: .weekly), "gemini-hourly")
        XCTAssertNil(provider.resolveWeeklyID(for: windows))
        XCTAssertEqual(provider.ringCadences(in: windows), [.fiveHour])
    }

    func testMissingFamilyAndMissingShortWindowDoNotBorrowOtherCountdowns() {
        let provider = AntigravityProvider(headlineModel: { .gemini })
        let other = LimitWindow(id: "3p-hourly", label: "Claude", usedFraction: 0.9,
                                resetsAt: Date().addingTimeInterval(3600), duration: 5 * 3600)
        var snapshot = ProviderSnapshot(id: "gemini", displayName: "Antigravity", glyph: .antigravity,
                                        fidelity: .official, status: .ok, windows: [other],
                                        headlineID: provider.resolveHeadlineID(for: [other]))
        XCTAssertNil(snapshot.headline)
        XCTAssertTrue(snapshot.resetSummaryIDs.isEmpty)
        let weekly = LimitWindow(id: "gemini-weekly", label: "Gemini", usedFraction: 0.3,
                                 resetsAt: Date().addingTimeInterval(86400), duration: 7 * 86400)
        snapshot.windows = [weekly, other]
        snapshot.headlineID = weekly.id
        snapshot.weeklyID = weekly.id
        snapshot.offeredCadences = provider.ringCadences(in: snapshot.windows) ?? []
        XCTAssertEqual(snapshot.resetSummaryIDs, [weekly.id])
    }
}
