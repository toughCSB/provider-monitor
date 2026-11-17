import XCTest
@testable import ProviderMonitor

@MainActor
final class TokenRenewalRecoveryTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private func at(_ seconds: TimeInterval) -> Date { now.addingTimeInterval(seconds) }

    func testFailedBurstReopensAfterOneHourAndStillHonorsCooldown() {
        let expiry = at(-60)
        var policy = TokenRenewalPolicy()
        for second in [0.0, 600, 1200] {
            XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: at(second), margin: 0, cooldown: 600))
            policy.recordAttempt(at: at(second))
        }
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: at(4799), margin: 0, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: at(4800), margin: 0, cooldown: 600))
        policy.recordAttempt(at: at(4800))
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: at(5399), margin: 0, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: at(5400), margin: 0, cooldown: 600))
    }

    func testSuccessfulExpiryStaysFinishedPastTheRecoveryHour() {
        let expiry = at(-60)
        var policy = TokenRenewalPolicy()
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: now, margin: 0, cooldown: 600))
        policy.recordAttempt(at: now)
        policy.finishForCurrentExpiry()
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: at(86_400), margin: 0, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: at(-30), now: at(86_400), margin: 0, cooldown: 600))
    }

    func testMissingCLIDefersWithoutSpendingThreeLaunches() {
        let expiry = at(-60)
        var policy = TokenRenewalPolicy()
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: now, margin: 0, cooldown: 600))
        policy.deferUnavailable(at: now)
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: at(3599), margin: 0, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: at(3600), margin: 0, cooldown: 600))
    }

    func testClaudeFindsCLIInstalledAfterMissingHour() async {
        let expiry = at(60)
        let renewed = at(8 * 3600)
        var available: URL?
        var afterCheck = expiry
        var launches = 0
        let refresher = ClaudeTokenRefresher(
            expiry: { expiry }, reload: { afterCheck }, cli: nil,
            locateCLI: { available },
            launcher: { _, _ in
                launches += 1
                afterCheck = renewed
                return (4242, { 1 })
            }
        )
        await refresher.considerRenewing(now: now)
        available = URL(fileURLWithPath: "/fake/claude")
        await refresher.considerRenewing(now: at(3599))
        XCTAssertEqual(launches, 0)
        await refresher.considerRenewing(now: at(3600))
        XCTAssertEqual(launches, 1)
        XCTAssertEqual(refresher.outcome, .refreshed(until: renewed))
    }

    func testGrokFindsCLIInstalledAfterMissingHour() async {
        let expired = at(-60)
        let renewed = at(6 * 3600)
        var available: URL?
        var diskExpiry = expired
        var launches = 0
        let refresher = GrokTokenRefresher(
            expiry: { diskExpiry }, cli: nil, locateCLI: { available },
            launcher: { _, _ in
                launches += 1
                diskExpiry = renewed
                return (4242, { 0 })
            }
        )
        await refresher.considerRenewing(now: now)
        available = URL(fileURLWithPath: "/fake/grok")
        await refresher.considerRenewing(now: at(3599))
        XCTAssertEqual(launches, 0)
        await refresher.considerRenewing(now: at(3600))
        XCTAssertEqual(launches, 1)
        XCTAssertEqual(refresher.outcome, .refreshed(until: renewed))
    }
}
