import XCTest
@testable import ProviderMonitor

@MainActor
final class UsageConnectionTests: XCTestCase {
    private final class Provider: UsageProvider, @unchecked Sendable {
        let id = "claude"
        let displayName = "Claude"
        let glyph = ProviderGlyph.claude
        var usageAccountID: String? = "opaque-account-A"
        var error: UsageProviderError?
        var changeAccountWhileFetching = false
        var calls = 0
        var nextErrorOnly = false
        func fetchSnapshot() async throws -> ProviderSnapshot {
            calls += 1
            if let error {
                if nextErrorOnly { self.error = nil }
                throw error
            }
            if changeAccountWhileFetching { usageAccountID = "opaque-account-B" }
            return ProviderSnapshot(id: id, displayName: displayName, glyph: glyph,
                                    fidelity: .official, status: .ok,
                                    windows: [LimitWindow(id: "session", label: "5h", usedFraction: 0.4)])
        }
        func signOut() async {}
    }

    private func setup(_ provider: Provider) -> (UsageStore, UsageArchive) {
        let name = "UsageConnectionTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        let archive = UsageArchive(defaults: defaults)
        return (UsageStore(providers: [provider], archive: archive), archive)
    }

    func testAuthenticationFailureKeepsOnlyConfirmedSameAccountHistory() async throws {
        let provider = Provider()
        let (store, archive) = setup(provider)
        await store.refresh()
        let measuredAt = try XCTUnwrap(archive.load()[provider.id]?.fetchedAt)
        provider.error = .needsAuth
        await store.refresh()
        let snapshot = try XCTUnwrap(store.snapshots.first)
        XCTAssertEqual(snapshot.usedFraction, 0.4)
        XCTAssertEqual(snapshot.status, .stale(since: measuredAt))
        XCTAssertEqual(snapshot.connectionFailure?.kind, .needsAuth)
        XCTAssertNotNil(store.providerSummaries.first?.connectionNote)
        XCTAssertEqual(archive.load()[provider.id]?.fetchedAt, measuredAt)
    }

    func testMissingIdentityDropsHistoryInsteadOfShowingAnUnverifiedAccount() async {
        let provider = Provider()
        let (store, archive) = setup(provider)
        await store.refresh()
        provider.usageAccountID = nil
        provider.error = .needsAuth
        await store.refresh()
        XCTAssertFalse(store.snapshots.first?.hasReading ?? true)
        XCTAssertNil(archive.load()[provider.id])
    }

    func testAccountSwitchDropsOldReadingEvenWhenTheNewRequestFailsTransiently() async {
        let provider = Provider()
        let (store, archive) = setup(provider)
        await store.refresh()
        provider.usageAccountID = "opaque-account-B"
        provider.error = .badResponse(status: 503)
        await store.refresh()
        XCTAssertFalse(store.snapshots.first?.hasReading ?? true)
        XCTAssertNil(archive.load()[provider.id])
        XCTAssertEqual(store.diagnostics[provider.id]?.failure?.httpStatus, 503)
    }

    func testAnAccountSwitchDuringFetchCannotPublishTheOldResponse() async {
        let provider = Provider()
        let (store, archive) = setup(provider)
        provider.changeAccountWhileFetching = true
        await store.refresh()
        XCTAssertFalse(store.snapshots.first?.hasReading ?? true)
        XCTAssertNil(archive.load()[provider.id])
        XCTAssertEqual(store.diagnostics[provider.id]?.failure?.kind, .accountChanged)
    }

    func testARecoveredConnectionClearsFailureWithoutLosingItsReading() async throws {
        let provider = Provider()
        let (store, _) = setup(provider)
        await store.refresh()
        provider.error = .badResponse(status: 503)
        await store.refresh()
        XCTAssertTrue(store.snapshots.first?.hasReading ?? false)
        XCTAssertNotNil(store.snapshots.first?.connectionFailure)
        provider.error = nil
        await store.refresh()
        XCTAssertNil(store.snapshots.first?.connectionFailure)
        XCTAssertNil(store.diagnostics[provider.id]?.failure)
        XCTAssertNotNil(store.diagnostics[provider.id]?.lastSuccessfulReadAt)
        let data = try UsageDiagnostics.data(store.diagnostics)
        XCTAssertFalse(String(decoding: data, as: UTF8.self).contains("opaque-account"))
    }

    func testRateLimitRetriesAtItsDeadlineAndManualRefreshCannotBypassIt() async {
        let provider = Provider()
        let (store, _) = setup(provider)
        provider.error = .rateLimited(retryAfter: 0.3)
        provider.nextErrorOnly = true
        await store.refresh()
        await store.refresh(providerID: provider.id)?.value
        XCTAssertEqual(provider.calls, 1)
        try? await Task.sleep(nanoseconds: 800_000_000)
        XCTAssertEqual(provider.calls, 2, "retry should not wait the five-minute idle interval")
        XCTAssertNil(store.diagnostics[provider.id]?.failure)
        store.stop()
    }

    func testDisconnectCancelsThePendingRetry() async {
        let provider = Provider()
        let (store, _) = setup(provider)
        provider.error = .rateLimited(retryAfter: 0.3)
        provider.nextErrorOnly = true
        await store.refresh()
        store.disconnected.insert(provider.id)
        try? await Task.sleep(nanoseconds: 600_000_000)
        XCTAssertEqual(provider.calls, 1)
        XCTAssertTrue(store.snapshots.isEmpty)
        store.stop()
    }
}
