import Foundation
import Darwin

struct ManagedCLIIdentity: Codable, Equatable, Sendable {
    let subject: String
    let email: String
    let workspaceID: String?
    let plan: String?

    /// Length prefixes make the account and workspace tuple unambiguous.
    var key: String {
        let workspace = workspaceID ?? ""
        return "\(subject.utf8.count):\(subject)\(workspace.utf8.count):\(workspace)"
    }
}

enum ManagedCLIError: LocalizedError, Equatable {
    case unavailable, notSignedIn, invalidResponse, identityChanged, failed, timedOut, cancelled

    var errorDescription: String? {
        switch self {
        case .unavailable: return "The official CLI is not installed."
        case .notSignedIn: return "This account is not signed in."
        case .invalidResponse: return "The official CLI returned an unreadable response."
        case .identityChanged: return "The signed-in account changed during the request."
        case .failed: return "The official CLI request failed."
        case .timedOut: return "The official CLI request timed out."
        case .cancelled: return "The official CLI request was cancelled."
        }
    }
}

/// Owns at most one child at a time. It never enumerates or signals another CLI process.
final class ManagedCLICancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var reason: ManagedCLIError?

    func cancel() { stop(.cancelled) }

    fileprivate func install(_ child: Process) throws {
        lock.lock()
        defer { lock.unlock() }
        if let reason { throw reason }
        process = child
    }

    fileprivate func finish(_ child: Process) -> ManagedCLIError? {
        lock.lock()
        defer { lock.unlock() }
        if process === child { process = nil }
        return reason
    }

    fileprivate func timeout(_ child: Process) {
        lock.lock()
        let active = process === child && reason == nil
        lock.unlock()
        if active { stop(.timedOut) }
    }

    fileprivate var currentReason: ManagedCLIError? {
        lock.lock()
        defer { lock.unlock() }
        return reason
    }

    private func stop(_ why: ManagedCLIError) {
        lock.lock()
        if reason != nil { lock.unlock(); return }
        reason = why
        let child = process
        lock.unlock()
        guard let child, child.isRunning else { return }
        child.terminate()
        // SIGTERM is cooperative. Escalate only while this exact Process is still ours.
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 2) { [weak self, weak child] in
            guard let self, let child else { return }
            self.lock.lock()
            let owned = self.process === child && child.isRunning
            self.lock.unlock()
            if owned { Darwin.kill(child.processIdentifier, SIGKILL) }
        }
    }
}

/// An isolated, official CLI transport. The caller owns creation and removal of `home`.
enum ManagedAccountCLI {
    @TaskLocal static var codexBinaryOverride: URL?
    @TaskLocal static var claudeBinaryOverride: URL?
    @TaskLocal static var timeoutOverride: TimeInterval?

    static func login(family: MonitoredAccounts.Family, home: URL,
                      cancellation: ManagedCLICancellation) async throws -> ManagedCLIIdentity {
        let binary = try locate(family)
        let arguments = family == .codex
            ? ["-c", "cli_auth_credentials_store=\"file\"", "login"]
            : ["auth", "login", "--claudeai"]
        _ = try await run(binary: binary, arguments: arguments, family: family, home: home,
                          cancellation: cancellation, timeout: 300)
        if let stopped = cancellation.currentReason { throw stopped }
        let signedIn = try await identity(family: family, home: home)
        if let stopped = cancellation.currentReason { throw stopped }
        return signedIn
    }

    static func identity(family: MonitoredAccounts.Family, home: URL) async throws -> ManagedCLIIdentity {
        switch family {
        case .codex:
            let claims = try codexClaims(home: home)
            let rpc = try await codexRPC(home: home, methods: ["account/read"])
            guard let account = rpc[1]?["account"] as? [String: Any],
                  account["type"] as? String == "chatgpt",
                  let rpcEmail = clean(account["email"] as? String),
                  rpcEmail.caseInsensitiveCompare(claims.email) == .orderedSame else {
                throw ManagedCLIError.identityChanged
            }
            let after = try codexClaims(home: home)
            guard claims.subject == after.subject,
                  claims.workspaceID == after.workspaceID,
                  claims.email.caseInsensitiveCompare(after.email) == .orderedSame else {
                throw ManagedCLIError.identityChanged
            }
            return ManagedCLIIdentity(subject: claims.subject, email: claims.email,
                                      workspaceID: claims.workspaceID,
                                      plan: clean(account["planType"] as? String) ?? claims.plan)
        case .claude:
            let result = try await run(binary: locate(.claude), arguments: ["auth", "status"], family: .claude,
                                       home: home, cancellation: ManagedCLICancellation(), timeout: 20,
                                       acceptedExitCodes: [0, 1])
            return try claudeIdentity(result, home: home)
        }
    }

    static func usage(family: MonitoredAccounts.Family, home: URL,
                      expected: ManagedCLIIdentity) async throws -> (windows: [LimitWindow], plan: String?) {
        let before = try await identity(family: family, home: home)
        guard before.key == expected.key else { throw ManagedCLIError.identityChanged }
        let windows: [LimitWindow]
        let plan: String?
        switch family {
        case .codex:
            let rpc = try await codexRPC(home: home, methods: ["account/read", "account/rateLimits/read"])
            guard let account = rpc[1]?["account"] as? [String: Any],
                  (account["email"] as? String)?.caseInsensitiveCompare(expected.email) == .orderedSame else {
                throw ManagedCLIError.identityChanged
            }
            windows = try parseCodexLimits(rpc[2] ?? [:])
            plan = clean(account["planType"] as? String) ?? before.plan
        case .claude:
            let data = try await run(binary: locate(.claude),
                                     arguments: ["--safe-mode"] + ClaudeUsageCLI.arguments, family: .claude,
                                     home: home, cancellation: ManagedCLICancellation(), timeout: 30)
            guard let output = String(data: data, encoding: .utf8) else { throw ManagedCLIError.invalidResponse }
            windows = try ClaudeUsageCLI.parse(output, now: Date())
            plan = ClaudeUsageCLI.plan(in: output) ?? before.plan
        }
        let after = try await identity(family: family, home: home)
        guard after.key == expected.key else { throw ManagedCLIError.identityChanged }
        return (windows, plan)
    }

    // MARK: - Identity metadata

    private static func clean(_ text: String?) -> String? {
        guard let text = text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }

    private static func codexClaims(home: URL) throws -> ManagedCLIIdentity {
        // Only the app-owned file and only identity claims. Never decode or return access/refresh tokens.
        let auth = home.appendingPathComponent("auth.json")
        guard let data = try? Data(contentsOf: auth, options: .mappedIfSafe), data.count <= 1_048_576,
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let tokens = root["tokens"] as? [String: Any],
              let idToken = tokens["id_token"] as? String,
              let claims = CodexCredentials.claims(inJWT: idToken),
              let subject = clean(claims["sub"] as? String),
              let email = clean(claims["email"] as? String) else { throw ManagedCLIError.notSignedIn }
        let authClaims = claims["https://api.openai.com/auth"] as? [String: Any]
        let claimWorkspace = clean(authClaims?["chatgpt_account_id"] as? String)
        let tokenWorkspace = clean(tokens["account_id"] as? String)
        if let claimWorkspace, let tokenWorkspace, claimWorkspace != tokenWorkspace {
            throw ManagedCLIError.identityChanged
        }
        let workspace = claimWorkspace ?? tokenWorkspace
        guard let workspace else { throw ManagedCLIError.invalidResponse }
        return ManagedCLIIdentity(subject: subject, email: email.lowercased(), workspaceID: workspace,
                                  plan: clean(authClaims?["chatgpt_plan_type"] as? String))
    }

    static func claudeIdentity(_ data: Data, home: URL) throws -> ManagedCLIIdentity {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              root["loggedIn"] as? Bool == true,
              let email = clean(root["email"] as? String) else { throw ManagedCLIError.notSignedIn }
        if let authMethod = clean(root["authMethod"] as? String),
           authMethod.lowercased() != "claude.ai" { throw ManagedCLIError.notSignedIn }
        if let path = clean(root["configDirectory"] as? String),
           URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
             != home.resolvingSymlinksInPath().standardizedFileURL { throw ManagedCLIError.identityChanged }
        // `accountId` is absent from some official status responses. Use the
        // same identity tuple whether that optional field appears or not.
        let normalizedEmail = email.lowercased()
        return ManagedCLIIdentity(subject: normalizedEmail, email: normalizedEmail,
                                  workspaceID: clean(root["orgId"] as? String),
                                  plan: clean(root["subscriptionType"] as? String))
    }

    static func parseCodexLimits(_ result: [String: Any]) throws -> [LimitWindow] {
        let buckets: [[String: Any]]
        if let byID = result["rateLimitsByLimitId"] as? [String: [String: Any]], !byID.isEmpty {
            buckets = byID.keys.sorted { $0 == "codex" || ($1 != "codex" && $0 < $1) }.compactMap { byID[$0] }
        } else if let one = result["rateLimits"] as? [String: Any] {
            buckets = [one]
        } else { throw ManagedCLIError.invalidResponse }
        var windows: [LimitWindow] = []
        for bucket in buckets {
            let limitID = clean(bucket["limitId"] as? String) ?? "codex"
            for (lane, field) in [("primary", "primary"), ("secondary", "secondary")] {
                guard let value = bucket[field] as? [String: Any],
                      let percent = (value["usedPercent"] as? NSNumber)?.doubleValue,
                      percent.isFinite, percent >= 0 else { continue }
                let minutes = (value["windowDurationMins"] as? NSNumber)?.doubleValue
                let seconds = (value["windowDurationSeconds"] as? NSNumber)?.doubleValue
                    ?? (minutes.map { $0 * 60 })
                let duration = seconds.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
                let reset = (value["resetsAt"] as? NSNumber)?.doubleValue
                let id = limitID == "codex" ? lane : "\(limitID)-\(lane)"
                let label = CodexUsage.label(windowSeconds: duration ?? 0, fallback: lane)
                windows.append(LimitWindow(id: id, group: limitID == "codex" ? nil : limitID,
                                           label: label, usedFraction: percent / 100,
                                           resetsAt: reset.flatMap { $0.isFinite && $0 > 0 ? Date(timeIntervalSince1970: $0) : nil },
                                           duration: duration))
            }
        }
        guard !windows.isEmpty else { throw ManagedCLIError.invalidResponse }
        return windows
    }

    // MARK: - Child process transport

    private static func locate(_ family: MonitoredAccounts.Family) throws -> URL {
        if family == .claude {
            if let override = claudeBinaryOverride { return override }
            guard let binary = ClaudeCLI.standalone() else { throw ManagedCLIError.unavailable }
            return binary
        }
        if let override = codexBinaryOverride { return override }
        let paths = ["/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
                     "/opt/homebrew/bin/codex", "/usr/local/bin/codex", "\(NSHomeDirectory())/.local/bin/codex"]
        guard let binary = paths.first(where: FileManager.default.isExecutableFile(atPath:)) else {
            throw ManagedCLIError.unavailable
        }
        return URL(fileURLWithPath: binary)
    }

    private static func codexRPC(home: URL, methods: [String]) async throws -> [Int: [String: Any]] {
        let binary = try locate(.codex)
        let data = try await run(binary: binary, arguments: ["app-server", "--listen", "stdio://"], family: .codex,
                                 home: home, cancellation: ManagedCLICancellation(), timeout: 30,
                                 rpcMethods: methods)
        var replies: [Int: [String: Any]] = [:]
        for line in data.split(separator: 10) {
            guard let object = try? JSONSerialization.jsonObject(with: Data(line)) as? [String: Any],
                  let id = object["id"] as? Int else { continue }
            if object["error"] != nil { throw ManagedCLIError.failed }
            if let result = object["result"] as? [String: Any] { replies[id] = result }
        }
        guard methods.indices.allSatisfy({ replies[$0 + 1] != nil }) else { throw ManagedCLIError.invalidResponse }
        return replies
    }

    private static func environment(home: URL, family: MonitoredAccounts.Family) -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        for key in env.keys where ["CODEX_", "CLAUDE_", "ANTHROPIC_", "OPENAI_", "AZURE_OPENAI_"].contains(where: key.hasPrefix) {
            env.removeValue(forKey: key)
        }
        // Keep the OS user's home: Security.framework resolves the login
        // keychain through it. Only the CLI's own account directory is isolated.
        env[family == .codex ? "CODEX_HOME" : "CLAUDE_CONFIG_DIR"] = home.path
        env["NO_COLOR"] = "1"
        return env
    }

    private static func run(binary: URL, arguments: [String], family: MonitoredAccounts.Family, home: URL,
                            cancellation: ManagedCLICancellation, timeout: TimeInterval,
                            rpcMethods: [String] = [], acceptedExitCodes: Set<Int32> = [0]) async throws -> Data {
        let deadline = timeoutOverride ?? timeout
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                do {
                    let process = Process()
                    process.executableURL = binary
                    process.arguments = arguments
                    process.currentDirectoryURL = home
                    process.environment = environment(home: home, family: family)
                    let output = Pipe(), errors = Pipe(), input = Pipe()
                    process.standardOutput = output
                    process.standardError = errors
                    process.standardInput = input
                    try cancellation.install(process)
                    do { try process.run() } catch {
                        _ = cancellation.finish(process)
                        throw ManagedCLIError.failed
                    }
                    if cancellation.currentReason != nil && process.isRunning { process.terminate() }
                    DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + deadline) {
                        cancellation.timeout(process)
                    }
                    DispatchQueue.global(qos: .utility).async {
                        while !errors.fileHandleForReading.availableData.isEmpty { /* discard secrets */ }
                    }
                    if rpcMethods.isEmpty { try? input.fileHandleForWriting.close() }
                    else {
                        let initialize = "{\"method\":\"initialize\",\"id\":0,\"params\":{\"clientInfo\":{\"name\":\"provider_monitor\",\"title\":\"Provider Monitor\",\"version\":\"1\"}}}\n"
                        input.fileHandleForWriting.write(Data(initialize.utf8))
                    }
                    var collected = Data(), pending = Data(), received = Set<Int>()
                    var sentRequests = false, complete = false, overflow = false
                    while true {
                        let chunk = output.fileHandleForReading.availableData
                        if chunk.isEmpty { break }
                        if collected.count + chunk.count > 1_048_576 {
                            overflow = true
                            if process.isRunning { process.terminate() }
                            continue
                        }
                        collected.append(chunk)
                        if !rpcMethods.isEmpty {
                            pending.append(chunk)
                            while let newline = pending.firstIndex(of: 10) {
                                let line = Data(pending[..<newline])
                                pending.removeSubrange(...newline)
                                guard let message = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
                                      let id = message["id"] as? Int else { continue }
                                if id == 0 && !sentRequests && message["result"] != nil {
                                    sentRequests = true
                                    var requests = Data("{\"method\":\"initialized\",\"params\":{}}\n".utf8)
                                    for (index, method) in rpcMethods.enumerated() {
                                        let params = method == "account/read" ? ",\"params\":{\"refreshToken\":false}" : ""
                                        requests.append(Data("{\"method\":\"\(method)\",\"id\":\(index + 1)\(params)}\n".utf8))
                                    }
                                    input.fileHandleForWriting.write(requests)
                                } else if id > 0 { received.insert(id) }
                            }
                            if received.count == rpcMethods.count {
                                complete = true
                                try? input.fileHandleForWriting.close()
                                if process.isRunning { process.terminate() }
                                break
                            }
                        }
                    }
                    process.waitUntilExit()
                    let stopped = cancellation.finish(process)
                    if let stopped { throw stopped }
                    if overflow { throw ManagedCLIError.invalidResponse }
                    if !rpcMethods.isEmpty && !complete { throw ManagedCLIError.invalidResponse }
                    if rpcMethods.isEmpty && !acceptedExitCodes.contains(process.terminationStatus) {
                        throw ManagedCLIError.failed
                    }
                    continuation.resume(returning: collected)
                } catch let error as ManagedCLIError {
                    continuation.resume(throwing: error)
                } catch {
                    continuation.resume(throwing: ManagedCLIError.failed)
                }
            }
        }
    }
}
