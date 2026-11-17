import AppKit
import Darwin

/// Installs the build that is currently running. The network updater uses the
/// same atomic bundle swap after it mounts a downloaded release image.
@MainActor
final class LocalInstall: ObservableObject {
    enum Outcome: Equatable {
        case idle
        case installing
        case alreadyThere
        case installed(URL)
        case failed(String)

        var message: String? {
            switch self {
            case .idle:            return nil
            case .installing:      return L10n.t("Installing…")
            case .alreadyThere:    return L10n.t("Provider Monitor is already the app in /Applications.")
            case .installed:       return L10n.t("Installed in /Applications. Quit and reopen Provider Monitor to run this build.")
            case .failed(let why): return L10n.t("Could not install: \(why)")
            }
        }

        /// Whether this is the one outcome worth colouring: reported as a
        /// failure rather than as a fact about the copy on disk.
        var isFailure: Bool {
            if case .failed = self { return true }
            return false
        }
    }

    @Published private(set) var outcome: Outcome = .idle

    /// Where an app belongs: with every other one on the machine, where
    /// Spotlight, the login item and the Finder all look for it.
    static var defaultDestination: URL {
        AppBundleInstaller.defaultDestination
    }

    /// Writable so the copy can be exercised without touching the real
    /// /Applications: under XCTest the app *is* the test host, so `Bundle.main`
    /// is the very build that would otherwise be copied there.
    var destination: URL = LocalInstall.defaultDestination

    /// The bundle that is running — the build being installed.
    ///
    /// Writable for the same reason `destination` is: the unit tests run inside
    /// the app as their host, so the real answer is the 100MB build under test,
    /// and a test that copied *that* would be measuring the disk rather than
    /// this rule.
    var runningFrom: URL = Bundle.main.bundleURL

    /// True when the running copy is the installed one, which is the state this
    /// button exists to reach and therefore has nothing left to do in.
    var isInstalledCopy: Bool {
        runningFrom.standardizedFileURL == destination.standardizedFileURL
    }

    func install() {
        guard !isInstalledCopy else {
            outcome = .alreadyThere
            return
        }
        outcome = .installing
        do {
            try AppBundleInstaller.copyIntoPlace(from: runningFrom, to: destination)
            outcome = .installed(destination)
        } catch {
            outcome = .failed(error.localizedDescription)
        }
    }
}

/// The one bundle replacement primitive used by both local installs and
/// downloaded updates. It is intentionally not tied to UI or the main actor so
/// a large release can be staged without freezing Settings.
enum AppBundleInstaller {
    static var defaultDestination: URL {
        URL(fileURLWithPath: "/Applications", isDirectory: true)
            .appendingPathComponent("Provider Monitor.app", isDirectory: true)
    }

    /// Copies the bundle in beside its destination and swaps it into place.
    ///
    /// Staged rather than copied over the top, so the swap can be atomic: an
    /// install that is interrupted — a full disk, a permission prompt dismissed
    /// — leaves whatever was already there untouched rather than half a bundle.
    ///
    /// `ditto` rather than `FileManager.copyItem`, because a bundle is more than
    /// the files inside it: extended attributes, the symlinks inside a framework
    /// and the resource envelope the code signature covers all have to survive,
    /// and `ditto` is the tool that keeps them.
    static func copyIntoPlace(from source: URL, to destination: URL) throws {
        try FileManager.default.createDirectory(
            at: destination.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        // Reserve a private sibling directory. Another install (or an older build's
        // fixed staging path) must never be removed by this attempt's cleanup.
        let stagingDirectory = destination.deletingLastPathComponent()
            .appendingPathComponent(".\(destination.lastPathComponent).install-\(UUID().uuidString)")
        guard mkdir(stagingDirectory.path, 0o700) == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        defer { try? FileManager.default.removeItem(at: stagingDirectory) }
        let staging = stagingDirectory.appendingPathComponent(destination.lastPathComponent)

        let ditto = Process()
        ditto.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        ditto.arguments = [source.path, staging.path]
        let complaint = Pipe()
        ditto.standardError = complaint
        try ditto.run()
        let said = complaint.fileHandleForReading.readDataToEndOfFile()
        ditto.waitUntilExit()
        guard ditto.terminationStatus == 0 else {
            throw InstallProblem(why: String(decoding: said, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines))
        }

        if FileManager.default.fileExists(atPath: destination.path) {
            _ = try FileManager.default.replaceItemAt(destination, withItemAt: staging)
        } else {
            try FileManager.default.moveItem(at: staging, to: destination)
        }
    }
}

/// What `ditto` said, when it said anything at all.
private struct InstallProblem: LocalizedError {
    let why: String
    var errorDescription: String? {
        why.isEmpty ? L10n.t("Provider Monitor could not be written to /Applications.") : why
    }
}
