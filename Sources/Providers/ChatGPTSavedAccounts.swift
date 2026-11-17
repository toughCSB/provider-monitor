import Foundation

/// Borrows the saved account-switch sessions from Aside. Only this exact
/// ChatGPT key is read. No browser cookie, active account or credential is changed.
struct ChatGPTSavedAccount: Equatable, Codable {
    let directory: URL
    let userID: String
    let label: String?

    var providerID: String { "codex-web-" + UsageAccountIdentity.fingerprint(userID).prefix(16) }

    private func current() -> ChatGPTSavedAccounts.Entry? {
        (try? ChatGPTSavedAccounts.entries(in: directory))?.first { $0.userId == userID }
    }

    var identity: String? {
        do {
            guard try ChatGPTSavedAccounts.entries(in: directory).contains(where: { $0.userId == userID }) else { return nil }
        } catch {
            // A live browser may be compacting the database. That is not a logout.
        }
        return UsageAccountIdentity.fingerprint("codex-web:\(userID)")
    }

    var account: ProviderAccount? {
        guard let entry = current() else { return nil }
        return ProviderAccount(label: entry.email, plan: nil, source: "ChatGPT · Aside",
                               manageURL: URL(string: "https://chatgpt.com/#settings/Account"))
    }

    func credential(session: URLSession) async throws -> CodexCredentials.Credential {
        let entries: [ChatGPTSavedAccounts.Entry]
        do { entries = try ChatGPTSavedAccounts.entries(in: directory) }
        catch { throw URLError(.cannotParseResponse) }
        guard let entry = entries.first(where: { $0.userId == userID }) else { throw UsageProviderError.needsAuth }
        var request = URLRequest(url: URL(string: "https://chatgpt.com/api/auth/session")!,
                                 cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 15)
        request.httpShouldHandleCookies = false
        request.setValue("__Secure-next-auth.session-token=\(entry.sessionToken)", forHTTPHeaderField: "Cookie")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-cache, no-store", forHTTPHeaderField: "Cache-Control")
        let (data, response) = try await session.data(for: request)
        let http = response as? HTTPURLResponse
        let status = http?.statusCode ?? 0
        if status == 401 || status == 403 { throw UsageProviderError.needsAuth }
        if status == 429 {
            throw UsageProviderError.rateLimited(retryAfter: max(60, CodexLocalProvider.retryAfter(from: http, now: Date()) ?? 0))
        }
        guard (200..<300).contains(status) else { throw UsageProviderError.badResponse(status: status) }
        return try Self.decodeSession(data, expectedUserID: userID)
    }

    static func decodeSession(_ data: Data, expectedUserID: String) throws -> CodexCredentials.Credential {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let token = root["accessToken"] as? String, !token.isEmpty,
              let user = root["user"] as? [String: Any],
              let userID = user["id"] as? String else { throw UsageProviderError.needsAuth }
        guard userID == expectedUserID else { throw UsageProviderError.accountChanged }
        guard let claims = CodexCredentials.claims(inJWT: token),
              let auth = claims["https://api.openai.com/auth"] as? [String: Any],
              let accountID = auth["chatgpt_account_id"] as? String, !accountID.isEmpty else {
            throw UsageProviderError.needsAuth
        }
        return CodexCredentials.Credential(accessToken: token, accountID: accountID)
    }
}

enum ChatGPTSavedAccounts {
    struct Entry: Decodable {
        let userId: String
        let email: String?
        let sessionToken: String
    }

    static let key = Data("_https://chatgpt.com\0\u{01}oai/apps/accountSwitchSessions".utf8)

    private struct Cached {
        let signature: [String]
        let entries: [Entry]
    }
    private static let cacheLock = NSLock()
    private static var cache: [URL: Cached] = [:]

    private static func signature(_ directory: URL) throws -> [String] {
        try FileManager.default.contentsOfDirectory(at: directory,
            includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey])
            .filter { $0.lastPathComponent == "CURRENT" || $0.lastPathComponent.hasPrefix("MANIFEST-")
                || ["log", "ldb", "sst"].contains($0.pathExtension) }
            .map { url in
                let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
                return "\(url.lastPathComponent):\(values.fileSize ?? -1):\(values.contentModificationDate?.timeIntervalSince1970 ?? -1)"
            }.sorted()
    }

    static func entries(in directory: URL) throws -> [Entry] {
        cacheLock.lock()
        defer { cacheLock.unlock() }
        let before = try signature(directory)
        if let cached = cache[directory], cached.signature == before { return cached.entries }
        cache[directory] = nil
        let entries: [Entry]
        if let text = try ChromiumLocalStorage.stringValue(for: key, in: directory),
           let json = text.data(using: .utf8) {
            entries = try decodeEntries(json)
        } else {
            entries = []
        }
        guard try signature(directory) == before else { throw ChromiumLocalStorage.ReadError.changedDuringRead }
        cache[directory] = Cached(signature: before, entries: entries)
        return entries
    }

    static func decodeEntries(_ json: Data) throws -> [Entry] {
        let entries = try JSONDecoder().decode([Entry].self, from: json)
        var seen = Set<String>()
        return Array(entries.filter {
            !$0.userId.isEmpty && !$0.sessionToken.isEmpty
                && $0.sessionToken != "__current_account_session__"
                && !$0.sessionToken.contains(where: { $0.isWhitespace || $0 == ";" })
                && seen.insert($0.userId).inserted
        }.prefix(2))
    }

    static func discover(support: URL = URL(fileURLWithPath: NSHomeDirectory())
        .appendingPathComponent("Library/Application Support"),
        defaults: UserDefaults = .standard) -> [ChatGPTSavedAccount] {
        let aside = support.appendingPathComponent("Aside")
        let profiles = ((try? FileManager.default.contentsOfDirectory(atPath: aside.path)) ?? [])
            .filter { $0 == "Default" || $0.hasPrefix("Profile ") }.sorted()
        var seen = Set<String>()
        var accounts: [ChatGPTSavedAccount] = []
        for profile in profiles {
            let directory = aside.appendingPathComponent(profile).appendingPathComponent("Local Storage/leveldb")
            for entry in (try? entries(in: directory)) ?? [] where seen.insert(entry.userId).inserted {
                accounts.append(ChatGPTSavedAccount(directory: directory, userID: entry.userId, label: entry.email))
            }
        }
        // Logging in more recently must not swap the numbers or their settings.
        let selected = Array(accounts.sorted { $0.providerID < $1.providerID }.prefix(2))
        let key = "codexWebAccountReferences"
        if selected.count == 2 {
            // Only stable references, never session tokens or email addresses.
            let references = selected.map { ChatGPTSavedAccount(directory: $0.directory, userID: $0.userID, label: nil) }
            if let data = try? JSONEncoder().encode(references) { defaults.set(data, forKey: key) }
            return selected
        }
        // A signed-out account remains a labelled slot. A temporarily locked or
        // compacting browser store must not silently replace it with CLI usage.
        if let data = defaults.data(forKey: key),
           let saved = try? JSONDecoder().decode([ChatGPTSavedAccount].self, from: data), saved.count == 2 {
            return saved
        }
        return selected
    }
}

/// Saved-session cookies are only sent to their original endpoint. A redirect
/// must never carry a credential elsewhere or silently change the account.
final class CodexSessionRedirectPolicy: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }

    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieStorage = nil
        config.httpShouldSetCookies = false
        config.urlCredentialStorage = nil
        return URLSession(configuration: config, delegate: CodexSessionRedirectPolicy(), delegateQueue: nil)
    }
}
