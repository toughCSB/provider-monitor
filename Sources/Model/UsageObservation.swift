import Foundation

/// The provenance of a usage reading, preserved when the same data is read again.
struct UsageObservation: Codable, Equatable, Sendable {
    enum Source: String, Codable, Sendable {
        case provider
        case remote
        case desktopCache
        case cli

        var label: String {
            switch self {
            case .provider: return L10n.t("Provider reading")
            case .remote: return L10n.t("Online reading")
            case .desktopCache: return L10n.t("Desktop cache")
            case .cli: return "CLI"
            }
        }
    }

    let measuredAt: Date
    let source: Source
    var accountID: String? = nil

    func note(at now: Date) -> String {
        "\(source.label) · \(ElapsedCopy.ago(since: measuredAt, now: now))"
    }
}
