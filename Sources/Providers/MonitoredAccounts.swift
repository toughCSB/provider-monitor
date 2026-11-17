import Foundation

/// Only paths are saved here. Credentials stay in the tool that owns them.
enum MonitoredAccounts {
    enum Family: String, CaseIterable, Codable, Sendable {
        case codex, claude
        var name: String { self == .codex ? "Codex" : "Claude" }
        func contains(_ id: String) -> Bool { id == rawValue || id.hasPrefix(rawValue + "-") }
    }

    enum RegistrationError: LocalizedError {
        case full, sameDirectory, sameAccount, missingCredential
        var errorDescription: String? {
            switch self {
            case .full: return L10n.t("Up to two accounts per provider can be monitored.")
            case .sameDirectory: return L10n.t("This account folder is already monitored.")
            case .sameAccount: return L10n.t("This account is already monitored. Choose a different account.")
            case .missingCredential: return L10n.t("No signed-in account was found in this folder. Sign in with the provider's own tool first.")
            }
        }
    }

    static func savedDirectory(_ family: Family, defaults: UserDefaults = .standard) -> URL? {
        defaults.string(forKey: "monitoredAccountDirectory.\(family.rawValue)").map {
            URL(fileURLWithPath: $0, isDirectory: true)
        }
    }

    static func save(_ directory: URL, family: Family, defaults: UserDefaults = .standard) {
        defaults.set(directory.resolvingSymlinksInPath().standardizedFileURL.path,
                     forKey: "monitoredAccountDirectory.\(family.rawValue)")
    }

    static func codex(home: URL = CodexProfile.homeDirectory,
                      defaults: UserDefaults = .standard) -> [CodexProfile] {
        let primary = CodexProfile.default(home: home)
        let saved = savedDirectory(.codex, defaults: defaults).map {
            CodexProfile(slug: "account-2", configDirectory: $0)
        }
        let candidates = saved.map { [$0] } ?? CodexProfile.discover(home: home).filter { $0.slug != nil }
        let firstID = CodexCredentials.usageAccountID(from: primary.authURL)
        let second = candidates.first {
            $0.configDirectory.resolvingSymlinksInPath().path != primary.configDirectory.resolvingSymlinksInPath().path
                && (saved != nil || CodexCredentials.usageAccountID(from: $0.authURL) != nil)
                && (CodexCredentials.usageAccountID(from: $0.authURL) == nil
                    || CodexCredentials.usageAccountID(from: $0.authURL) != firstID)
        }
        return [primary] + (second.map { [$0] } ?? [])
    }

    static func claude(home: URL = ClaudeProfile.homeDirectory,
                       defaults: UserDefaults = .standard,
                       hasCredential: (ClaudeProfile) -> Bool = ClaudeProfile.hasKeychainCredential) -> [ClaudeProfile] {
        let primary = ClaudeProfile.default(home: home)
        let saved = savedDirectory(.claude, defaults: defaults).map {
            ClaudeProfile(slug: "account-2", configDirectory: $0)
        }
        let candidates = saved.map { [$0] } ?? ClaudeProfile.discover(home: home, hasCredential: hasCredential)
            .filter { $0.slug != nil }
        let second = candidates.first {
            $0.configDirectory.resolvingSymlinksInPath().path != primary.configDirectory.resolvingSymlinksInPath().path
                && (claudeIdentity($0) == nil || claudeIdentity($0) != claudeIdentity(primary))
        }
        return [primary] + (second.map { [$0] } ?? [])
    }

    static func claudeIdentity(_ profile: ClaudeProfile) -> String? {
        guard let address = profile.signedInAddress() else { return nil }
        return (profile.organizationID() ?? "") + ":" + address.lowercased()
    }

    /// Numbering follows the profile identity, never the draggable display order.
    static func number(for id: String, among ids: [String]) -> Int? {
        guard let family = Family.allCases.first(where: { $0.contains(id) }) else { return nil }
        let members = Array(Set(ids.filter(family.contains))).sorted {
            if $0 == family.rawValue { return true }
            if $1 == family.rawValue { return false }
            return $0 < $1
        }
        guard members.count > 1, let index = members.firstIndex(of: id) else { return nil }
        return index + 1
    }

    static func name(for id: String, original: String, among ids: [String]) -> String {
        guard let number = number(for: id, among: ids),
              let family = Family.allCases.first(where: { $0.contains(id) }) else { return original }
        return "\(family.name) \(number)"
    }
}
