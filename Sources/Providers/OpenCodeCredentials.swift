import Foundation
import SQLite3

/// The OpenCode Go key, borrowed from OpenCode's own sign-in.
///
/// OpenCode keeps its own sign-in in `auth.json` or `opencode.db`. A current
/// `oc_sk_` console key wins over a leftover `sk-` Go key; an OAuth sign-in
/// uses the inference route. Other provider identities are never claimed.
enum OpenCodeCredentials {
    struct Credential {
        let token: String
        let oauth: Bool
        let console: URL?
        let org: String?
        let expires: Date?
        var isConsoleKey: Bool { !oauth && token.hasPrefix("oc_sk_") }
    }

    static var authURL: URL {
        dataDirectory.appendingPathComponent("auth.json")
    }

    static var dataDirectory: URL {
        if let xdg = ProcessInfo.processInfo.environment["XDG_DATA_HOME"], !xdg.isEmpty {
            return URL(fileURLWithPath: xdg).appendingPathComponent("opencode")
        }
        return URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".local/share/opencode")
    }

    static func load(in directory: URL = dataDirectory) -> Credential? {
        let auth = directory.appendingPathComponent("auth.json")
        let file = readJSON(auth)
        let fileGo = file?["opencode-go"].flatMap(credential)
        let fileAccount = file?["opencode"].flatMap(credential)
        let stored = databaseCredentials(directory.appendingPathComponent("opencode.db"))

        // A current Console key wins even when a leftover sk- Go key is newer.
        for id in ["opencode", "opencode-go"] {
            if let found = stored.first(where: { $0.id == id && $0.value.isConsoleKey }) {
                return found.value
            }
        }
        if let fileAccount, fileAccount.isConsoleKey { return fileAccount }
        if let fileGo, fileGo.isConsoleKey { return fileGo }
        for id in ["opencode-go", "opencode"] {
            if let found = stored.first(where: { $0.id == id }) { return found.value }
        }
        return fileGo ?? (fileAccount?.oauth == true ? fileAccount : nil)
    }

    static func load(from url: URL = authURL) -> Credential? {
        guard let root = readJSON(url) else { return nil }
        let account = root["opencode"].flatMap(credential)
        let go = root["opencode-go"].flatMap(credential)
        if let account, account.isConsoleKey { return account }
        if let go, go.isConsoleKey { return go }
        return go ?? (account?.oauth == true ? account : nil)
    }

    private static func readJSON(_ url: URL) -> [String: Any]? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    private static func databaseCredentials(_ url: URL) -> [(id: String, value: Credential)] {
        guard let db = SQLiteStore.open(url) else { return [] }
        defer { sqlite3_close(db) }
        let rows = SQLiteStore.rows(in: db, sql: """
            SELECT integration_id, value FROM credential
            WHERE integration_id IN ('opencode-go', 'opencode')
              AND COALESCE(active, 1) != 0
            ORDER BY time_updated DESC
            """, columns: 2)
        return rows.compactMap { row in
            guard let data = row[1].data(using: .utf8),
                  let entry = try? JSONSerialization.jsonObject(with: data),
                  let value = credential(entry) else { return nil }
            return (row[0], value)
        }
    }

    private static func credential(_ entry: Any) -> Credential? {
        if let token = nonEmpty(entry as? String) {
            return Credential(token: token, oauth: false, console: nil, org: nil, expires: nil)
        }
        guard let object = entry as? [String: Any] else { return nil }
        if object["type"] as? String == "oauth" {
            guard let token = nonEmpty(object["access"] as? String) else { return nil }
            let metadata = object["metadata"] as? [String: Any]
            let expiry = (object["expires"] as? NSNumber)?.doubleValue
                ?? (object["expires"] as? String).flatMap(Double.init)
            return Credential(token: token, oauth: true,
                              console: (metadata?["server"] as? String).flatMap(URL.init(string:)),
                              org: nonEmpty(metadata?["orgID"] as? String),
                              expires: expiry.map { Date(timeIntervalSince1970: $0 / 1000) })
        }
        let token = ["key", "apiKey", "api_key", "token", "accessToken"]
            .compactMap { nonEmpty(object[$0] as? String) }.first
        return token.map { Credential(token: $0, oauth: false, console: nil, org: nil, expires: nil) }
    }


    /// An empty key would send a request that cannot succeed.
    private static func nonEmpty(_ value: String?) -> String? {
        value.flatMap { $0.isEmpty ? nil : $0 }
    }
}
