import SwiftUI
import XCTest
@testable import ProviderMonitor

@MainActor
final class MonitoredAccountsTests: XCTestCase {
    private func fixture() throws -> (URL, UserDefaults) {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let suite = "MonitoredAccountsTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        addTeardownBlock {
            try? FileManager.default.removeItem(at: home)
            defaults.removePersistentDomain(forName: suite)
        }
        return (home, defaults)
    }

    private func codex(_ home: URL, directory: String, identity: String) throws -> CodexProfile {
        let url = home.appendingPathComponent(directory)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        // Expired on purpose: discovery must include it, polling must not send it.
        let token = "h." + Data("{\"exp\":1}".utf8).base64EncodedString() + ".s"
        let data = try JSONSerialization.data(withJSONObject: ["tokens": [
            "account_id": identity, "access_token": token
        ]])
        try data.write(to: url.appendingPathComponent("auth.json"))
        return CodexProfile(slug: directory == ".codex" ? nil : String(directory.dropFirst(7)), configDirectory: url)
    }

    func testNumberingDoesNotFollowDragOrderOrAffectOtherProviders() {
        let ids = ["codex-work", "claude-work", "cursor", "codex", "claude"]
        XCTAssertEqual(MonitoredAccounts.number(for: "codex", among: ids), 1)
        XCTAssertEqual(MonitoredAccounts.number(for: "codex-work", among: ids), 2)
        XCTAssertEqual(MonitoredAccounts.name(for: "claude-work", original: "Claude (work)", among: ids), "Claude 2")
        XCTAssertNil(MonitoredAccounts.number(for: "cursor", among: ids))
        XCTAssertNil(MonitoredAccounts.number(for: "codex", among: ["codex", "claude"]))
    }

    func testDiscoverySkipsSameAccountAndCapsEachFamilyAtTwo() throws {
        let (home, defaults) = try fixture()
        _ = try codex(home, directory: ".codex", identity: "a")
        _ = try codex(home, directory: ".codex-backup", identity: "a")
        _ = try codex(home, directory: ".codex-work", identity: "b")
        _ = try codex(home, directory: ".codex-z", identity: "c")
        let profiles = MonitoredAccounts.codex(home: home, defaults: defaults)
        XCTAssertEqual(profiles.map(\.id), ["codex", "codex-work"])
    }

    func testSavedProfileSurvivesSignOutWithoutFallingBackToAnotherAccount() throws {
        let (home, defaults) = try fixture()
        _ = try codex(home, directory: ".codex", identity: "a")
        let second = try codex(home, directory: "business account", identity: "b")
        MonitoredAccounts.save(second.configDirectory, family: .codex, defaults: defaults)
        try FileManager.default.removeItem(at: second.authURL)
        _ = try codex(home, directory: ".codex-work", identity: "c")
        let profiles = MonitoredAccounts.codex(home: home, defaults: defaults)
        XCTAssertEqual(profiles.count, 2)
        XCTAssertEqual(profiles[1].configDirectory.path, second.configDirectory.path)
        XCTAssertEqual(profiles[1].id, "codex-account-2")
    }

    func testAddingAccountUpdatesBothNamesImmediatelyAndRejectsThird() throws {
        let (home, defaults) = try fixture()
        let first = try codex(home, directory: ".codex", identity: "a")
        let second = try codex(home, directory: ".codex-work", identity: "b")
        let store = UsageStore(providers: [CodexLocalProvider(profile: first)], archive: UsageArchive(defaults: defaults))
        defer { store.stop() }
        XCTAssertEqual(store.providerSummaries.map(\.name), ["Codex"])
        var added: String?
        store.onAccountAdded = { added = $0.id }
        try store.addAccount(.codex, directory: second.configDirectory, defaults: defaults)
        XCTAssertEqual(added, "codex-account-2")
        XCTAssertEqual(store.providerSummaries.map(\.name), ["Codex 1", "Codex 2"])
        XCTAssertEqual(store.notchSnapshots.map(\.accountNumber), [1, 2])
        XCTAssertThrowsError(try store.addAccount(.codex, directory: home, defaults: defaults))
        store.disconnected = ["codex"]
        XCTAssertEqual(store.notchSnapshots.map(\.displayName), ["Codex 2"])
        XCTAssertEqual(store.providerSummaries.map(\.name), ["Codex 1", "Codex 2"])
    }

    func testSameLoginAndSymlinkCannotCreateADuplicateRing() throws {
        let (home, defaults) = try fixture()
        let first = try codex(home, directory: ".codex", identity: "a")
        let copy = try codex(home, directory: ".codex-backup", identity: "a")
        let alias = home.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: first.configDirectory)
        let store = UsageStore(providers: [CodexLocalProvider(profile: first)], archive: UsageArchive(defaults: defaults))
        XCTAssertThrowsError(try store.addAccount(.codex, directory: copy.configDirectory, defaults: defaults))
        XCTAssertThrowsError(try store.addAccount(.codex, directory: alias, defaults: defaults))
        XCTAssertEqual(store.providerSummaries.count, 1)
        XCTAssertNil(MonitoredAccounts.savedDirectory(.codex, defaults: defaults))
    }

    func testClaudeDiscoverySkipsDuplicateIdentityAndPlugins() throws {
        let (home, defaults) = try fixture()
        for name in [".claude", ".claude-backup", ".claude-work", ".claude-z", ".claude-mem"] {
            let directory = home.appendingPathComponent(name)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try Data().write(to: directory.appendingPathComponent("settings.json"))
            let address = name == ".claude" || name == ".claude-backup" ? "a@test" : "b@test"
            let url = name == ".claude" ? home.appendingPathComponent(".claude.json") : directory.appendingPathComponent(".claude.json")
            try JSONSerialization.data(withJSONObject: ["oauthAccount": ["emailAddress": address, "organizationUuid": "org"]]).write(to: url)
        }
        let profiles = MonitoredAccounts.claude(home: home, defaults: defaults, hasCredential: { $0.slug != "mem" })
        XCTAssertEqual(profiles.map(\.id), ["claude", "claude-work"])
    }

    func testDistinctAccountIconsRenderInTheActualCell() throws {
        let snapshots: [ProviderSnapshot] = [("codex", "Codex 1", ProviderGlyph.openai, 1, 0.2),
                                              ("codex-work", "Codex 2", .openai, 2, 0.7),
                                              ("claude", "Claude 1", .claude, 1, 0.3),
                                              ("claude-work", "Claude 2", .claude, 2, 0.6)].map { id, name, glyph, number, used in
            ProviderSnapshot(id: id, displayName: name, glyph: glyph, fidelity: .official, status: .ok,
                             windows: [LimitWindow(id: "session", label: "Session", usedFraction: used)],
                             accountNumber: number)
        }
        let renderer = ImageRenderer(content: HStack(spacing: 24) {
            ForEach(snapshots) { snapshot in
                VStack(spacing: 14) {
                    ProviderCell(snapshot: snapshot)
                    Text(snapshot.displayName).font(.system(size: 12, weight: .medium))
                }
            }
        }.padding(30).background(Color.black).foregroundStyle(.white).environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try XCTUnwrap(renderer.cgImage)
        let bitmap = NSBitmapImageRep(cgImage: image)
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: URL(fileURLWithPath: "/tmp/provider-monitor-multi-account-preview.png"))
        XCTAssertGreaterThan(image.width, 300)
        XCTAssertNotEqual(ProviderBrand.markColor(.openai, accountNumber: 1), ProviderBrand.markColor(.openai, accountNumber: 2))
        XCTAssertNotEqual(ProviderBrand.markColor(.claude, accountNumber: 1), ProviderBrand.markColor(.claude, accountNumber: 2))
    }
}
