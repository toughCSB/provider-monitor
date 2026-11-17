import Foundation

/// A registered login is polled through the official CLI in its own home.
/// Claude credentials are never read by this provider.
actor ManagedAccountProvider: UsageProvider {
    nonisolated let record: ManagedAccountRecord
    nonisolated let home: URL
    private let archive: UsageArchive
    private var retryAt: Date?
    nonisolated var id: String { record.providerID }
    nonisolated var displayName: String { record.name }
    nonisolated var glyph: ProviderGlyph { record.family == .codex ? .openai : .claude }
    nonisolated var usageAccountID: String? { UsageAccountIdentity.fingerprint(record.identity.key) }
    nonisolated var signInRoute: SignInRoute { .guidance(L10n.t("Use Sign in again in the registered accounts section.")) }

    init(record: ManagedAccountRecord, home: URL, archive: UsageArchive = UsageArchive()) {
        self.record = record
        self.home = home
        self.archive = archive
        self.retryAt = archive.loadBackoffUntil(providerID: record.providerID)
    }

    nonisolated func account() -> ProviderAccount? {
        ProviderAccount(label: record.identity.email, plan: record.identity.plan,
                        source: L10n.t("Registered with the official CLI"), manageURL: nil)
    }

    func fetchSnapshot() async throws -> ProviderSnapshot {
        if let retryAt, retryAt > Date() { throw UsageProviderError.rateLimited(retryAfter: retryAt.timeIntervalSinceNow) }
        let reading: (windows: [LimitWindow], plan: String?)
        do {
            reading = try await ManagedAccountCLI.usage(family: record.family, home: home, expected: record.identity)
        } catch ManagedCLIError.identityChanged {
            throw UsageProviderError.accountChanged
        } catch ManagedCLIError.notSignedIn {
            throw UsageProviderError.needsAuth
        } catch ManagedCLIError.timedOut {
            throw UsageProviderError.timedOut
        } catch UsageProviderError.rateLimited(let delay) {
            retryAt = Date().addingTimeInterval(max(60, delay))
            archive.saveBackoffUntil(retryAt, providerID: id)
            throw UsageProviderError.rateLimited(retryAfter: max(60, delay))
        }
        retryAt = nil
        archive.saveBackoffUntil(nil, providerID: id)
        return ProviderSnapshot(id: id, displayName: displayName, glyph: glyph, fidelity: .official,
                                status: .ok, windows: reading.windows,
                                headlineID: Self.headline(in: reading.windows, family: record.family),
                                weeklyID: record.family == .codex ? "secondary" : "weekly_all",
                                accountNumber: record.slot,
                                plan: reading.plan ?? record.identity.plan,
                                observation: UsageObservation(measuredAt: Date(), source: .cli, accountID: usageAccountID))
    }

    static func headline(in windows: [LimitWindow], family: MonitoredAccounts.Family) -> String? {
        let preferred = family == .codex ? ["primary", "secondary"] : ["session", "weekly_all"]
        return preferred.first { id in windows.contains { $0.id == id } } ?? windows.first?.id
    }
}
