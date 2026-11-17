import XCTest
@testable import ProviderMonitor

@MainActor
final class ManagedAccountsTests: XCTestCase {
    private let alice = ManagedCLIIdentity(subject: "alice", email: "alice@example.test", workspaceID: "personal", plan: "plus")
    private let bob = ManagedCLIIdentity(subject: "bob", email: "bob@example.test", workspaceID: "work", plan: "pro")

    private func fixture() throws -> (URL, UserDefaults) {
        let root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appendingPathComponent("managed-tests-\(UUID())")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let suite = "ManagedAccountsTests.\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        addTeardownBlock {
            try? FileManager.default.removeItem(at: root)
            defaults.removePersistentDomain(forName: suite)
        }
        return (root, defaults)
    }

    private func finishLogin(_ controller: ManagedAccountController) async {
        for _ in 0..<200 where controller.busyFamily != nil {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertNil(controller.busyFamily)
    }

    func testRegistrationRequiresConfirmationAndPersistsMetadataOnly() async throws {
        let (root, defaults) = try fixture()
        let identity = alice
        let controller = ManagedAccountController(defaults: defaults, root: root,
            login: { _, home, _ in
                XCTAssertEqual(home.deletingLastPathComponent().path, root.path)
                return identity
            }, readIdentity: { _, _ in identity })
        var changes: [MonitoredAccounts.Family] = []
        controller.onChanged = { changes.append($0) }
        controller.begin(.codex)
        await finishLogin(controller)
        XCTAssertTrue(controller.records.isEmpty)
        XCTAssertNotNil(controller.pending)
        XCTAssertTrue(changes.isEmpty)
        await controller.confirm()
        XCTAssertEqual(controller.records.count, 1)
        XCTAssertEqual(controller.records.first?.slot, 1)
        XCTAssertEqual(changes, [.codex])
        let restored = ManagedAccountController(defaults: defaults, root: root)
        XCTAssertEqual(restored.records, controller.records)
        let data = try XCTUnwrap(defaults.data(forKey: "managedMonitoringAccounts.v1"))
        let json = String(decoding: data, as: UTF8.self)
        XCTAssertFalse(json.contains("access_token"))
        XCTAssertFalse(json.contains("refresh_token"))
    }

    func testDuplicateRejectedAndRemovingDoesNotDeleteCLIFiles() async throws {
        let (root, defaults) = try fixture()
        let identity = alice
        let controller = ManagedAccountController(defaults: defaults, root: root,
            login: { _, _, _ in identity }, readIdentity: { _, _ in identity })
        controller.begin(.codex)
        await finishLogin(controller)
        await controller.confirm()
        let record = try XCTUnwrap(controller.records.first)
        let ownedFile = controller.home(for: record).appendingPathComponent("owned-by-cli")
        try Data("preserve".utf8).write(to: ownedFile)
        controller.begin(.codex)
        await finishLogin(controller)
        XCTAssertNil(controller.pending)
        XCTAssertNotNil(controller.errorMessage)
        XCTAssertEqual(controller.records.count, 1)
        controller.remove(record)
        XCTAssertTrue(controller.records.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: ownedFile.path))
    }

    func testIdentityChangedBeforeConfirmationNeverRegisters() async throws {
        let (root, defaults) = try fixture()
        let first = alice, second = bob
        let controller = ManagedAccountController(defaults: defaults, root: root,
            login: { _, _, _ in first }, readIdentity: { _, _ in second })
        controller.begin(.claude)
        await finishLogin(controller)
        await controller.confirm()
        XCTAssertTrue(controller.records.isEmpty)
        XCTAssertNotNil(controller.errorMessage)
        XCTAssertNil(defaults.data(forKey: "managedMonitoringAccounts.v1"))
    }

    func testCancelIgnoresLateLoginCompletion() async throws {
        let (root, defaults) = try fixture()
        let identity = alice
        let controller = ManagedAccountController(defaults: defaults, root: root,
            login: { _, _, _ in
                try? await Task.sleep(nanoseconds: 50_000_000)
                return identity
            }, readIdentity: { _, _ in identity })
        controller.begin(.codex)
        controller.cancel()
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertNil(controller.pending)
        XCTAssertTrue(controller.records.isEmpty)
    }

    func testCorruptRegistryIsPreservedAndBlocksNewLogin() throws {
        let (root, defaults) = try fixture()
        let data = Data("invalid".utf8)
        defaults.set(data, forKey: "managedMonitoringAccounts.v1")
        let controller = ManagedAccountController(defaults: defaults, root: root)
        XCTAssertFalse(controller.canAdd(.codex))
        controller.begin(.codex)
        XCTAssertNil(controller.busyFamily)
        XCTAssertEqual(defaults.data(forKey: "managedMonitoringAccounts.v1"), data)
    }

    func testFullRegistryKeepsSlotTwoAfterRemovingSlotOne() throws {
        let (root, defaults) = try fixture()
        let a = ManagedAccountRecord(id: UUID(), family: .codex, slot: 1, directory: UUID(), identity: alice)
        let b = ManagedAccountRecord(id: UUID(), family: .codex, slot: 2, directory: UUID(), identity: bob)
        defaults.set(try JSONEncoder().encode([a, b]), forKey: "managedMonitoringAccounts.v1")
        let controller = ManagedAccountController(defaults: defaults, root: root)
        XCTAssertFalse(controller.canAdd(.codex))
        XCTAssertTrue(controller.canAdd(.claude))
        controller.remove(a)
        XCTAssertEqual(controller.accounts(for: .codex).map(\.name), ["Codex 2"])
        XCTAssertTrue(controller.canAdd(.codex))
        let store = UsageStore(providers: controller.providers(for: .codex), archive: UsageArchive(defaults: defaults))
        defer { store.stop() }
        XCTAssertEqual(store.providerSummaries.map(\.name), ["Codex 2"])
        XCTAssertEqual(store.providerSummaries.first?.accountNumber, 2)
    }
}
