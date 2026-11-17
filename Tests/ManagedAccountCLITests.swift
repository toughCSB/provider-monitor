import Foundation
import XCTest
@testable import ProviderMonitor

final class ManagedAccountCLITests: XCTestCase {
    func testCodexManagedDurationLabelsMatchExistingAccountFormatting() throws {
        for minutes in [300.0, 10080.0, 43200.0, 100800.0] {
            let rows = try ManagedAccountCLI.parseCodexLimits([
                "rateLimits": ["primary": ["usedPercent": 12, "windowDurationMins": minutes, "resetsAt": 1_800_000_000]]
            ])
            XCTAssertEqual(rows[0].label, CodexUsage.label(windowSeconds: minutes * 60, fallback: "primary"))
            XCTAssertEqual(rows[0].resetsAt, Date(timeIntervalSince1970: 1_800_000_000))
        }
    }
    private func directory() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("managed-cli-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func executable(_ source: String, in home: URL) throws -> URL {
        let url = home.appendingPathComponent("fake-cli")
        try source.write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: url.path)
        return url
    }

    private func auth(home: URL, subject: String = "person-1", workspace: String = "workspace-1") throws {
        let claims: [String: Any] = [
            "sub": subject, "email": "User@Example.com",
            "https://api.openai.com/auth": ["chatgpt_account_id": workspace, "chatgpt_plan_type": "plus"]
        ]
        let payload = try JSONSerialization.data(withJSONObject: claims).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        let value = ["tokens": ["id_token": "header.\(payload).signature", "account_id": workspace]]
        try JSONSerialization.data(withJSONObject: value).write(to: home.appendingPathComponent("auth.json"))
    }

    private let rpcScript = """
    #!/bin/sh
    printf '%s' "$HOME" > "$CODEX_HOME/observed-system-home"
    printf '%s' "$CODEX_HOME" > "$CODEX_HOME/observed-config-home"
    while IFS= read -r line; do
      case "$line" in
        *'"method":"initialize"'*) echo '{"id":0,"result":{}}' ;;
        *'"method":"account/read"'*) echo '{"id":1,"result":{"account":{"type":"chatgpt","email":"user@example.com","planType":"plus"}}}' ;;
        *'"method":"account/rateLimits/read"'*) echo '{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":1800000000},"secondary":{"usedPercent":40,"windowDurationSeconds":604800}}}}' ;;
      esac
    done
    """

    func testCodexRPCUsesOwnedHomeAndParsesBothDurations() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        try auth(home: home)
        let binary = try executable(rpcScript, in: home)
        try await ManagedAccountCLI.$codexBinaryOverride.withValue(binary) {
            let identity = try await ManagedAccountCLI.identity(family: .codex, home: home)
            XCTAssertEqual(identity.subject, "person-1")
            XCTAssertEqual(identity.workspaceID, "workspace-1")
            XCTAssertEqual(identity.email, "user@example.com")
            let usage = try await ManagedAccountCLI.usage(family: .codex, home: home, expected: identity)
            XCTAssertEqual(usage.windows.map(\.id), ["primary", "secondary"])
            XCTAssertEqual(usage.windows[0].duration, 18_000)
            XCTAssertEqual(usage.windows[1].duration, 604_800)
            XCTAssertEqual(usage.windows[0].usedFraction, 0.25)
            XCTAssertEqual(usage.plan, "plus")
            XCTAssertEqual(try String(contentsOf: home.appendingPathComponent("observed-system-home"), encoding: .utf8),
                           ProcessInfo.processInfo.environment["HOME"] ?? NSHomeDirectory())
            XCTAssertEqual(try String(contentsOf: home.appendingPathComponent("observed-config-home"), encoding: .utf8), home.path)
        }
    }

    func testWrongWorkspaceIsRejectedBeforeUsage() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        try auth(home: home)
        let binary = try executable(rpcScript, in: home)
        try await ManagedAccountCLI.$codexBinaryOverride.withValue(binary) {
            let identity = try await ManagedAccountCLI.identity(family: .codex, home: home)
            let wrong = ManagedCLIIdentity(subject: identity.subject, email: identity.email,
                                           workspaceID: "other-workspace", plan: identity.plan)
            do {
                _ = try await ManagedAccountCLI.usage(family: .codex, home: home, expected: wrong)
                XCTFail("Expected identity mismatch")
            } catch let error as ManagedCLIError {
                XCTAssertEqual(error, .identityChanged)
            }
        }
    }

    func testWorkspaceSwitchDuringRateLimitReadIsRejected() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        try auth(home: home, workspace: "workspace-1")
        let original = try Data(contentsOf: home.appendingPathComponent("auth.json"))
        try auth(home: home, workspace: "workspace-2")
        try FileManager.default.moveItem(at: home.appendingPathComponent("auth.json"),
                                         to: home.appendingPathComponent("auth-next.json"))
        try original.write(to: home.appendingPathComponent("auth.json"))
        let script = rpcScript.replacingOccurrences(
            of: "*'\"method\":\"account/rateLimits/read\"'*) ",
            with: "*'\"method\":\"account/rateLimits/read\"'*) cp \"$CODEX_HOME/auth-next.json\" \"$CODEX_HOME/auth.json\"; ")
        let binary = try executable(script, in: home)
        try await ManagedAccountCLI.$codexBinaryOverride.withValue(binary) {
            let identity = try await ManagedAccountCLI.identity(family: .codex, home: home)
            do {
                _ = try await ManagedAccountCLI.usage(family: .codex, home: home, expected: identity)
                XCTFail("Expected identity switch")
            } catch let error as ManagedCLIError {
                XCTAssertEqual(error, .identityChanged)
            }
        }
    }

    func testClaudeStatusUsesEmailAndOrgEvenWithoutAccountID() throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        let withoutAccountID = Data("""
        {"loggedIn":true,"authMethod":"claude.ai","email":" User@Example.com ","orgId":"team"}
        """.utf8)
        let withAccountID = Data("""
        {"loggedIn":true,"authMethod":"claude.ai","email":"user@example.com","orgId":"team","accountId":"person"}
        """.utf8)
        let first = try ManagedAccountCLI.claudeIdentity(withoutAccountID, home: home)
        let second = try ManagedAccountCLI.claudeIdentity(withAccountID, home: home)
        XCTAssertEqual(first.subject, "user@example.com")
        XCTAssertEqual(first.email, "user@example.com")
        XCTAssertEqual(first.workspaceID, "team")
        XCTAssertEqual(first.key, second.key)
    }

    func testClaudeLoginKeepsSystemHomeWhileIsolatingAccountDirectory() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        let binary = try executable("""
        #!/bin/sh
        printf '%s' "$HOME" > "$CLAUDE_CONFIG_DIR/observed-system-home"
        printf '%s' "$CLAUDE_CONFIG_DIR" > "$CLAUDE_CONFIG_DIR/observed-config-home"
        printf '{"loggedIn":true,"authMethod":"claude.ai","email":"user@example.com","orgId":"team"}\\n'
        """, in: home)
        try await ManagedAccountCLI.$claudeBinaryOverride.withValue(binary) {
            let identity = try await ManagedAccountCLI.login(family: .claude, home: home,
                                                            cancellation: ManagedCLICancellation())
            XCTAssertEqual(identity.email, "user@example.com")
            let observed = try String(contentsOf: home.appendingPathComponent("observed-system-home"), encoding: .utf8)
            XCTAssertEqual(observed, ProcessInfo.processInfo.environment["HOME"] ?? NSHomeDirectory(),
                           "Changing HOME hides the user's login keychain from Security.framework")
            XCTAssertEqual(try String(contentsOf: home.appendingPathComponent("observed-config-home"), encoding: .utf8), home.path)
        }
    }

    func testClaudeStatusRejectsAPIKeyAndWrongConfigDirectory() throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        let apiKey = Data("""
        {"loggedIn":true,"authMethod":"api_key","email":"user@example.com","orgId":"team"}
        """.utf8)
        XCTAssertThrowsError(try ManagedAccountCLI.claudeIdentity(apiKey, home: home)) {
            XCTAssertEqual($0 as? ManagedCLIError, .notSignedIn)
        }
        let status = Data("""
        {"loggedIn":true,"authMethod":"claude.ai","email":"user@example.com","orgId":"team","configDirectory":"/other"}
        """.utf8)
        XCTAssertThrowsError(try ManagedAccountCLI.claudeIdentity(status, home: home)) {
            XCTAssertEqual($0 as? ManagedCLIError, .identityChanged)
        }
    }

    func testCancellingLoginStopsOnlyItsChild() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        let binary = try executable("#!/bin/sh\nwhile :; do :; done\n", in: home)
        let cancellation = ManagedCLICancellation()
        try await ManagedAccountCLI.$claudeBinaryOverride.withValue(binary) {
            let task = Task { try await ManagedAccountCLI.login(family: .claude, home: home, cancellation: cancellation) }
            try await Task.sleep(for: .milliseconds(100))
            cancellation.cancel()
            do { _ = try await task.value; XCTFail("Expected cancellation") }
            catch let error as ManagedCLIError { XCTAssertEqual(error, .cancelled) }
        }
    }

    func testTimeoutDoesNotRevealChildOutput() async throws {
        let home = try directory()
        defer { try? FileManager.default.removeItem(at: home) }
        let binary = try executable("#!/bin/sh\necho secret-from-child >&2\nwhile :; do :; done\n", in: home)
        try await ManagedAccountCLI.$claudeBinaryOverride.withValue(binary) {
            try await ManagedAccountCLI.$timeoutOverride.withValue(0.15) {
                do {
                    _ = try await ManagedAccountCLI.login(family: .claude, home: home,
                                                          cancellation: ManagedCLICancellation())
                    XCTFail("Expected timeout")
                } catch let error as ManagedCLIError {
                    XCTAssertEqual(error, .timedOut)
                    XCTAssertFalse(error.localizedDescription.contains("secret-from-child"))
                }
            }
        }
    }
}
