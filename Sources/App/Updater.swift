import AppKit
import CryptoKit
import Foundation

/// Checks Provider Monitor's own GitHub releases and installs the macOS disk image.
///
/// Earlier builds queried the original Codenotch Sparkle feed and deliberately refused to
/// install from it. That made both controls in Settings misleading for this fork: "Check now"
/// checked a different product and there was no route to update Provider Monitor at all.
@MainActor
final class Updater: ObservableObject {
    struct Release: Equatable, Sendable {
        let version: String
        let pageURL: URL
        let assetURL: URL
        let sha256: String?
        let expectedSize: Int64?
    }

    enum Outcome: Equatable {
        case idle
        case checking
        case upToDate(Date)
        case found(String)
        case downloading(String)
        case installing(String)
        case installed(String)
        case unreachable
        case failed(String)

        var message: String? {
            switch self {
            case .idle:                 return nil
            case .checking:             return L10n.t("Checking…")
            case .upToDate:             return L10n.t("Provider Monitor is up to date.")
            case .found(let version):   return L10n.t("Provider Monitor \(version) is available.")
            case .downloading(let version):
                return L10n.t("Downloading Provider Monitor \(version)…")
            case .installing(let version):
                return L10n.t("Installing Provider Monitor \(version)…")
            case .installed(let version):
                return L10n.t("Provider Monitor \(version) is installed. Restarting…")
            case .unreachable:
                return L10n.t("Couldn't reach the update server. Check your connection and try again.")
            case .failed(let why):      return why
            }
        }
    }

    @Published private(set) var outcome: Outcome = .idle
    @Published private(set) var availableRelease: Release?
    @Published var automatic: Bool {
        didSet {
            defaults.set(automatic, forKey: Self.automaticKey)
            if automatic { checkNow() }
        }
    }

    private let defaults: UserDefaults
    private let session: URLSession
    private var work: Task<Void, Never>?

    private static let automaticKey = "ProviderMonitorAutomaticallyChecksForUpdates"
    private static let lastCheckedKey = "ProviderMonitorLastUpdateCheck"
    private nonisolated static let latestReleaseURL = URL(
        string: "https://api.github.com/repos/toughCSB/provider-monitor/releases/latest"
    )!

    init(defaults: UserDefaults = .standard, session: URLSession = .shared) {
        self.defaults = defaults
        self.session = session
        self.automatic = defaults.object(forKey: Self.automaticKey) as? Bool ?? true
    }

    var currentVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
    }

    var lastChecked: Date? { defaults.object(forKey: Self.lastCheckedKey) as? Date }

    var canInstall: Bool { availableRelease != nil && !isBusy }

    var isBusy: Bool {
        switch outcome {
        case .checking, .downloading, .installing: return true
        default: return false
        }
    }

    func start() {
        guard automatic else { return }
        let due = lastChecked.map { Date().timeIntervalSince($0) >= 24 * 60 * 60 } ?? true
        if due { checkNow() }
    }

    func checkNow() {
        guard !isBusy else { return }
        outcome = .checking
        work?.cancel()
        work = Task { [weak self] in
            guard let self else { return }
            defer { self.work = nil }
            do {
                let release = try await Self.fetchLatest(using: self.session)
                guard !Task.isCancelled else { return }
                let checked = Date()
                self.defaults.set(checked, forKey: Self.lastCheckedKey)
                if Self.isNewer(release.version, than: self.currentVersion) {
                    self.availableRelease = release
                    self.outcome = .found(release.version)
                } else {
                    self.availableRelease = nil
                    self.outcome = .upToDate(checked)
                }
            } catch is CancellationError {
                return
            } catch let error as URLError {
                self.availableRelease = nil
                self.outcome = error.code == .cancelled ? .idle : .unreachable
            } catch {
                self.availableRelease = nil
                self.outcome = .failed(error.localizedDescription)
            }
        }
    }

    func installAvailable() {
        guard let release = availableRelease, !isBusy else { return }
        outcome = .downloading(release.version)
        work?.cancel()
        work = Task { [weak self] in
            guard let self else { return }
            defer { self.work = nil }
            do {
                let image = try await Self.download(release, using: self.session)
                defer { try? FileManager.default.removeItem(at: image) }
                guard !Task.isCancelled else { return }
                self.outcome = .installing(release.version)
                try await Task.detached(priority: .userInitiated) {
                    let destination = try Self.installDiskImage(at: image)
                    _ = try Self.run("/usr/bin/open", ["-n", destination.path])
                }.value
                self.outcome = .installed(release.version)
                try? await Task.sleep(for: .milliseconds(700))
                NSApp.terminate(nil)
            } catch is CancellationError {
                return
            } catch let error as URLError {
                self.outcome = error.code == .cancelled ? .idle : .unreachable
            } catch {
                self.outcome = .failed(error.localizedDescription)
            }
        }
    }

    nonisolated static func isNewer(_ candidate: String, than current: String) -> Bool {
        func components(_ value: String) -> [Int] {
            value.trimmingCharacters(in: CharacterSet(charactersIn: "vV"))
                .split(separator: "-", maxSplits: 1).first?
                .split(separator: ".").map { Int($0) ?? 0 } ?? []
        }
        let lhs = components(candidate), rhs = components(current)
        for index in 0..<max(lhs.count, rhs.count) {
            let left = index < lhs.count ? lhs[index] : 0
            let right = index < rhs.count ? rhs[index] : 0
            if left != right { return left > right }
        }
        return false
    }

    nonisolated static func release(from data: Data) throws -> Release {
        struct APIRelease: Decodable {
            struct Asset: Decodable {
                let name: String
                let browser_download_url: URL
                let digest: String?
                let size: Int64?
            }
            let tag_name: String
            let html_url: URL
            let assets: [Asset]
        }

        let decoded = try JSONDecoder().decode(APIRelease.self, from: data)
        let version = decoded.tag_name.trimmingCharacters(in: CharacterSet(charactersIn: "vV"))
        guard let asset = decoded.assets.first(where: {
            $0.name == "ProviderMonitor-\(version)-unsigned.dmg"
        }) else {
            throw ProviderUpdateError.missingDiskImage
        }
        let digest = asset.digest?.replacingOccurrences(of: "sha256:", with: "")
        return Release(version: version, pageURL: decoded.html_url,
                       assetURL: asset.browser_download_url, sha256: digest,
                       expectedSize: asset.size)
    }

    private nonisolated static func fetchLatest(using session: URLSession) async throws -> Release {
        var request = URLRequest(url: latestReleaseURL)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("Provider-Monitor-macOS", forHTTPHeaderField: "User-Agent")
        let (data, response) = try await session.data(for: request)
        try requireSuccess(response)
        return try release(from: data)
    }

    private nonisolated static func download(_ release: Release,
                                             using session: URLSession) async throws -> URL {
        let (temporary, response) = try await session.download(from: release.assetURL)
        do {
            try requireSuccess(response)
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            throw error
        }
        guard let expected = release.sha256, expected.count == 64 else {
            try? FileManager.default.removeItem(at: temporary)
            throw ProviderUpdateError.missingDigest
        }
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("ProviderMonitor-\(release.version)-\(UUID().uuidString).dmg")
        try FileManager.default.moveItem(at: temporary, to: destination)
        do {
            if let expectedSize = release.expectedSize {
                let actualSize = try destination.resourceValues(forKeys: [.fileSizeKey]).fileSize
                guard expectedSize > 0, actualSize.map(Int64.init) == expectedSize else {
                    throw ProviderUpdateError.sizeMismatch
                }
            }
            let actual = try sha256(ofFile: destination)
            guard actual.caseInsensitiveCompare(expected) == .orderedSame else {
                throw ProviderUpdateError.digestMismatch
            }
            return destination
        } catch {
            try? FileManager.default.removeItem(at: destination)
            throw error
        }
    }

    private nonisolated static func requireSuccess(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ProviderUpdateError.badResponse((response as? HTTPURLResponse)?.statusCode)
        }
    }

    nonisolated static func sha256(of data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    nonisolated static func sha256(ofFile url: URL) throws -> String {
        let input = try FileHandle(forReadingFrom: url)
        defer { try? input.close() }
        var digest = SHA256()
        while let chunk = try input.read(upToCount: 1024 * 1024), !chunk.isEmpty {
            digest.update(data: chunk)
        }
        return digest.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private nonisolated static func installDiskImage(at image: URL) throws -> URL {
        let attached = try run("/usr/bin/hdiutil", ["attach", image.path, "-nobrowse", "-readonly", "-plist"])
        let plist = try PropertyListSerialization.propertyList(from: attached, options: [], format: nil)
        guard let root = plist as? [String: Any],
              let entities = root["system-entities"] as? [[String: Any]],
              let mountPath = entities.compactMap({ $0["mount-point"] as? String }).last else {
            throw ProviderUpdateError.couldNotMount
        }
        let mount = URL(fileURLWithPath: mountPath, isDirectory: true)
        defer { _ = try? run("/usr/bin/hdiutil", ["detach", mount.path]) }

        let source = mount.appendingPathComponent("Provider Monitor.app", isDirectory: true)
        guard FileManager.default.fileExists(atPath: source.path) else {
            throw ProviderUpdateError.missingApplication
        }
        let destination = AppBundleInstaller.defaultDestination
        try AppBundleInstaller.copyIntoPlace(from: source, to: destination)
        return destination
    }

    @discardableResult
    private nonisolated static func run(_ executable: String, _ arguments: [String]) throws -> Data {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        let output = Pipe(), error = Pipe()
        process.standardOutput = output
        process.standardError = error
        try process.run()
        process.waitUntilExit()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        let complaint = error.fileHandleForReading.readDataToEndOfFile()
        guard process.terminationStatus == 0 else {
            let why = String(decoding: complaint, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw ProviderUpdateError.commandFailed(why.isEmpty ? executable : why)
        }
        return data
    }
}

private enum ProviderUpdateError: LocalizedError {
    case badResponse(Int?)
    case missingDiskImage
    case missingDigest
    case sizeMismatch
    case digestMismatch
    case couldNotMount
    case missingApplication
    case commandFailed(String)

    var errorDescription: String? {
        switch self {
        case .badResponse(let status):
            return status.map { "The update server returned HTTP \($0)." }
                ?? "The update server returned an invalid response."
        case .missingDiskImage:
            return "The latest release has no Provider Monitor disk image."
        case .missingDigest:
            return "The latest release has no SHA-256 checksum."
        case .sizeMismatch:
            return "The downloaded update does not match its published size."
        case .digestMismatch:
            return "The downloaded update did not match its published SHA-256 checksum."
        case .couldNotMount:
            return "The downloaded disk image could not be mounted."
        case .missingApplication:
            return "The downloaded disk image does not contain Provider Monitor.app."
        case .commandFailed(let why):
            return why
        }
    }
}
