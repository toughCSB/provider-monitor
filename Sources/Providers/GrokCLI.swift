import Foundation

/// Where the standalone Grok command lives, if it is installed.
///
/// The CLI is the only thing that renews `~/.grok/auth.json`. Its OIDC session
/// lives six hours, and this app must not mint one itself: the refresh rotates
/// the stored refresh token, so a second writer would race the CLI for the file.
/// A renewal is therefore a launch of this command — the same bargain
/// `ClaudeTokenRefresher` makes with `claude`.
///
/// The installer keeps a shim in `~/.local/bin` in front of the managed binary
/// in `~/.grok/bin`, so both are accepted, the shim first: that is what a shell
/// would run, and it is the one that survives the CLI updating itself.
enum GrokCLI {
    /// The usual install locations, in the order a shell would find them.
    static let candidates = [
        "~/.local/bin/grok",      // the installer's shim
        "/opt/homebrew/bin/grok", // Homebrew on Apple silicon
        "/usr/local/bin/grok",    // Homebrew on Intel, and the installer
        "~/.grok/bin/grok"        // the managed binary the shim execs
    ]

    /// `home` is a parameter so a test can point the "~" candidates at an empty
    /// directory instead of the developer's own, which is the difference between
    /// a test that always passes and one that reports what is installed.
    static func standalone(candidates: [String] = candidates,
                           home: String = NSHomeDirectory(),
                           fileManager: FileManager = .default) -> URL? {
        for path in candidates {
            let expanded: String
            if path.hasPrefix("~/") {
                expanded = (home as NSString).appendingPathComponent(String(path.dropFirst(2)))
            } else {
                expanded = path
            }
            guard fileManager.isExecutableFile(atPath: expanded) else { continue }
            return URL(fileURLWithPath: expanded)
        }
        return nil
    }
}
