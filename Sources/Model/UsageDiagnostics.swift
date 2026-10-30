import CryptoKit
import Foundation

/// Account matching is local only. Neither identifiers nor credentials enter diagnostics.
enum UsageAccountIdentity {
    static func fingerprint(_ value: String) -> String {
        SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

struct UsageFailure: Codable, Equatable {
    enum Kind: String, Codable {
        case needsAuth, accessDenied, expired, signedOut, rateLimited, timedOut
        case unsupported, http, network, accountChanged
    }
    let kind: Kind
    var httpStatus: Int? = nil
    var retryAt: Date? = nil

    init(_ error: Error, at now: Date = Date()) {
        switch error {
        case UsageProviderError.needsAuth: kind = .needsAuth
        case UsageProviderError.accessDenied: kind = .accessDenied
        case UsageProviderError.credentialExpired: kind = .expired
        case UsageProviderError.signedOutByOwner: kind = .signedOut
        case UsageProviderError.rateLimited(let seconds):
            kind = .rateLimited
            retryAt = now.addingTimeInterval(max(0, seconds.isFinite ? seconds : 0))
        case UsageProviderError.timedOut: kind = .timedOut
        case UsageProviderError.nothingMetered: kind = .unsupported
        case UsageProviderError.badResponse(let status): kind = .http; httpStatus = status
        case UsageProviderError.accountChanged: kind = .accountChanged
        default: kind = .network
        }
    }

    func message(at now: Date) -> String {
        switch kind {
        case .needsAuth: return L10n.t("Usage authentication could not be confirmed. Check the signed-in account and plan.")
        case .accessDenied: return L10n.t("Saved login access was denied. Choose Allow access in Settings.")
        case .expired: return L10n.t("Saved login expired. Waiting for renewal.")
        case .signedOut: return L10n.t("The owning app removed its saved login.")
        case .rateLimited:
            let remaining = Int(min(Double(Int.max - 1024), retryAt.map { UsageRetryPolicy.remaining(until: $0, at: now) } ?? 0))
            return remaining > 0 ? L10n.t("Usage request limited. Retry in \(remaining) sec.")
                : L10n.t("Usage request limited. Waiting to retry.")
        case .timedOut: return L10n.t("Usage request timed out. Other providers continue updating.")
        case .unsupported: return L10n.t("This account did not report metered usage.")
        case .http: return L10n.t("Usage request failed (HTTP \(httpStatus ?? 0)).")
        case .network: return L10n.t("Usage could not be reached. Waiting to retry.")
        case .accountChanged: return L10n.t("Account changed during the request. Waiting for a new reading.")
        }
    }
}

struct UsageDiagnostic: Codable, Equatable {
    let providerID: String
    var lastAttemptAt: Date?
    var lastSuccessfulReadAt: Date?
    var measuredAt: Date?
    var source: UsageObservation.Source?
    var failure: UsageFailure?
    var inFlight = false
    var attempts = 0
}

enum UsageDiagnostics {
    /// Deliberately excludes account IDs, tokens, paths, response bodies and transcripts.
    static func data(_ records: [String: UsageDiagnostic], now: Date = Date()) throws -> Data {
        struct Report: Encodable {
            let version = 1
            let capturedAt: Date
            let providers: [UsageDiagnostic]
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        return try encoder.encode(Report(capturedAt: now, providers: records.values.sorted { $0.providerID < $1.providerID }))
    }

    static func persist(_ records: [String: UsageDiagnostic]) {
        guard !Runtime.isUnderTest, let bytes = try? data(records) else { return }
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/ProviderMonitor", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try bytes.write(to: directory.appendingPathComponent("monitoring.json"), options: .atomic)
        } catch {
            Log.usage.error("could not write the local monitoring diagnostics")
        }
    }
}
