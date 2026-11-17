import Foundation
import Combine

/// Metadata only. The provider's CLI owns credentials in each private home.
struct ManagedAccountRecord: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let family: MonitoredAccounts.Family
    let slot: Int
    let directory: UUID
    let identity: ManagedCLIIdentity

    var providerID: String { "\(family.rawValue)-managed-\(id.uuidString.lowercased())" }
    var name: String { "\(family.name) \(slot)" }
}

@MainActor
final class ManagedAccountController: ObservableObject {
    struct Pending: Identifiable {
        let id: UUID
        let family: MonitoredAccounts.Family
        let home: URL
        let identity: ManagedCLIIdentity
        let replacing: ManagedAccountRecord?
    }

    typealias Login = @Sendable (MonitoredAccounts.Family, URL, ManagedCLICancellation) async throws -> ManagedCLIIdentity
    typealias IdentityReader = @Sendable (MonitoredAccounts.Family, URL, ManagedCLICancellation) async throws -> ManagedCLIIdentity

    @Published private(set) var records: [ManagedAccountRecord] = []
    @Published private(set) var busyFamily: MonitoredAccounts.Family?
    @Published private(set) var pending: Pending?
    @Published private(set) var errorMessage: String?
    @Published private(set) var isConfirming = false
    var onChanged: ((MonitoredAccounts.Family) -> Void)?

    private let defaults: UserDefaults
    private let root: URL
    private let login: Login
    private let readIdentity: IdentityReader
    private var generation: UUID?
    private var cancellation: ManagedCLICancellation?
    private var confirmationCancellation: ManagedCLICancellation?
    private var task: Task<Void, Never>?
    private var corruptRegistry = false
    private static let key = "managedMonitoringAccounts.v1"

    nonisolated static var defaultRoot: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Provider Monitor/managed-accounts", isDirectory: true)
    }

    init(defaults: UserDefaults = .standard, root: URL = defaultRoot,
         login: @escaping Login = { try await ManagedAccountCLI.login(family: $0, home: $1, cancellation: $2) },
         readIdentity: @escaping IdentityReader = { try await ManagedAccountCLI.identity(family: $0, home: $1, cancellation: $2) }) {
        self.defaults = defaults
        self.root = root.standardizedFileURL
        self.login = login
        self.readIdentity = readIdentity
        if let data = defaults.data(forKey: Self.key) {
            if let saved = try? JSONDecoder().decode([ManagedAccountRecord].self, from: data), Self.valid(saved) {
                records = saved
            } else {
                corruptRegistry = true
                errorMessage = L10n.t("Saved account settings could not be read. They have been preserved.")
            }
        }
    }

    private static func valid(_ records: [ManagedAccountRecord]) -> Bool {
        guard Set(records.map(\.id)).count == records.count,
              Set(records.map(\.directory)).count == records.count else { return false }
        return MonitoredAccounts.Family.allCases.allSatisfy { family in
            let members = records.filter { $0.family == family }
            return members.count <= 2 && members.allSatisfy { (1...2).contains($0.slot) && !$0.identity.email.isEmpty && !$0.identity.subject.isEmpty }
                && Set(members.map(\.slot)).count == members.count
                && Set(members.map { $0.identity.key }).count == members.count
        }
    }

    func accounts(for family: MonitoredAccounts.Family) -> [ManagedAccountRecord] {
        records.filter { $0.family == family }.sorted { $0.slot < $1.slot }
    }

    func home(for record: ManagedAccountRecord) -> URL {
        root.appendingPathComponent(record.directory.uuidString.lowercased(), isDirectory: true)
    }

    func providers(for family: MonitoredAccounts.Family) -> [UsageProvider] {
        accounts(for: family).map { ManagedAccountProvider(record: $0, home: home(for: $0)) }
    }

    func canAdd(_ family: MonitoredAccounts.Family) -> Bool {
        !corruptRegistry && busyFamily == nil && pending == nil && accounts(for: family).count < 2
    }

    func begin(_ family: MonitoredAccounts.Family, replacing record: ManagedAccountRecord? = nil) {
        guard !corruptRegistry, busyFamily == nil, pending == nil else { return }
        guard record.map({ records.contains($0) && $0.family == family }) ?? (accounts(for: family).count < 2) else {
            errorMessage = L10n.t("Up to two accounts per provider can be monitored.")
            return
        }
        let generation = UUID()
        let directory = UUID()
        let home = root.appendingPathComponent(directory.uuidString.lowercased(), isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            guard root.resolvingSymlinksInPath().path == root.path else { throw URLError(.noPermissionsToReadFile) }
            try FileManager.default.createDirectory(at: home, withIntermediateDirectories: false,
                                                    attributes: [.posixPermissions: 0o700])
        } catch {
            errorMessage = L10n.t("A private account folder could not be created.")
            return
        }
        self.generation = generation
        busyFamily = family
        errorMessage = nil
        let cancellation = ManagedCLICancellation()
        self.cancellation = cancellation
        task = Task { [weak self, login] in
            do {
                let identity = try await login(family, home, cancellation)
                guard let self, self.generation == generation, !Task.isCancelled else { return }
                guard !identity.subject.isEmpty, !identity.email.isEmpty else { throw UsageProviderError.needsAuth }
                if let record, identity.key != record.identity.key { throw UsageProviderError.accountChanged }
                guard !self.accounts(for: family).contains(where: { $0.id != record?.id && $0.identity.key == identity.key }) else {
                    self.errorMessage = L10n.t("This account is already registered. Sign in with the other account.")
                    self.busyFamily = nil
                    self.generation = nil
                    return
                }
                self.pending = Pending(id: directory, family: family, home: home, identity: identity, replacing: record)
                self.busyFamily = nil
                self.cancellation = nil
            } catch {
                guard let self, self.generation == generation else { return }
                self.busyFamily = nil
                self.generation = nil
                if (error as? ManagedCLIError) == .identityChanged {
                    self.errorMessage = L10n.t("A different account signed in. The registered account has not changed.")
                } else if (error as? ManagedCLIError) == .unavailable {
                    self.errorMessage = L10n.t("Install the official Codex or Claude CLI before adding this account.")
                } else if (error as? ManagedCLIError) == .timedOut {
                    self.errorMessage = L10n.t("Sign-in timed out. Your existing accounts have been preserved. Try again.")
                } else if case UsageProviderError.accountChanged = error {
                    self.errorMessage = L10n.t("A different account signed in. The registered account has not changed.")
                } else {
                    // Never display raw process output, URLs or credentials as an error.
                    self.errorMessage = L10n.t("Sign-in did not finish. Check that the official CLI is installed and finish sign-in in the browser, then try again.")
                }
            }
        }
    }

    func cancel() {
        generation = nil
        cancellation?.cancel()
        cancellation = nil
        confirmationCancellation?.cancel()
        confirmationCancellation = nil
        task?.cancel()
        task = nil
        busyFamily = nil
        pending = nil
        isConfirming = false
        // Never log out or delete a provider credential as a side effect of Cancel.
    }

    func confirm() async {
        guard let pending, !isConfirming, !corruptRegistry else { return }
        isConfirming = true
        let cancellation = ManagedCLICancellation()
        confirmationCancellation = cancellation
        defer { isConfirming = false; confirmationCancellation = nil }
        do {
            // Recheck after the user read the identity: a changed home cannot be registered.
            let current = try await withTaskCancellationHandler {
                try await readIdentity(pending.family, pending.home, cancellation)
            } onCancel: { cancellation.cancel() }
            guard self.pending?.id == pending.id, !Task.isCancelled else { return }
            guard current.key == pending.identity.key else { throw UsageProviderError.accountChanged }
            var next = records
            let slot: Int
            let id: UUID
            if let replacing = pending.replacing {
                guard next.contains(replacing) else { throw UsageProviderError.accountChanged }
                next.removeAll { $0.id == replacing.id }
                slot = replacing.slot
                id = replacing.id
            } else {
                let occupied = Set(next.filter { $0.family == pending.family }.map(\.slot))
                guard let free = (1...2).first(where: { !occupied.contains($0) }) else { throw UsageProviderError.accountChanged }
                slot = free
                id = UUID()
            }
            next.append(ManagedAccountRecord(id: id, family: pending.family, slot: slot,
                                              directory: pending.id, identity: current))
            guard Self.valid(next) else { throw UsageProviderError.accountChanged }
            let data = try JSONEncoder().encode(next)
            defaults.set(data, forKey: Self.key)
            records = next
            self.pending = nil
            generation = nil
            errorMessage = nil
            onChanged?(pending.family)
        } catch {
            guard self.pending?.id == pending.id else { return }
            errorMessage = L10n.t("The account could not be confirmed. Your existing accounts have been preserved.")
        }
    }

    func remove(_ record: ManagedAccountRecord) {
        guard !corruptRegistry, busyFamily == nil, pending == nil, records.contains(record) else { return }
        let next = records.filter { $0.id != record.id }
        guard let data = try? JSONEncoder().encode(next) else { return }
        defaults.set(data, forKey: Self.key)
        records = next
        // Remove monitoring registration only. The CLI owns sign-out and its files.
        onChanged?(record.family)
    }
}
