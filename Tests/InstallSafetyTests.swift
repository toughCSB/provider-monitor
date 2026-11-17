import XCTest
@testable import ProviderMonitor

@MainActor
final class InstallSafetyTests: XCTestCase {
    private func bundle(_ url: URL, contents: String) throws {
        let binary = url.appendingPathComponent("Contents/MacOS/Provider Monitor")
        try FileManager.default.createDirectory(at: binary.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try Data(contents.utf8).write(to: binary)
    }

    private func contents(_ url: URL) throws -> String {
        try String(contentsOf: url.appendingPathComponent("Contents/MacOS/Provider Monitor"),
                   encoding: .utf8)
    }

    func testMissingSourcePreservesInstalledAppAndUnownedStagingPath() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("InstallSafetyTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let destination = root.appendingPathComponent("Provider Monitor.app")
        let unowned = root.appendingPathComponent(
            ".(destination.lastPathComponent).(ProcessInfo.processInfo.processIdentifier)")
        try bundle(destination, contents: "installed")
        try bundle(unowned, contents: "other install")

        XCTAssertThrowsError(try AppBundleInstaller.copyIntoPlace(
            from: root.appendingPathComponent("missing.app"), to: destination))
        XCTAssertEqual(try contents(destination), "installed")
        XCTAssertEqual(try contents(unowned), "other install")
        XCTAssertEqual(Set(try FileManager.default.contentsOfDirectory(atPath: root.path)),
                       ["Provider Monitor.app", unowned.lastPathComponent])
    }

    func testConcurrentStagingDoesNotDeleteOtherAttemptsFiles() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("InstallSafetyTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let sources = ["A", "B"].map { root.appendingPathComponent("\($0).app") }
        for (index, source) in sources.enumerated() {
            try bundle(source, contents: "new\(index)")
        }
        let destination = root.appendingPathComponent("Applications/Provider Monitor.app")
        try bundle(destination, contents: "old")
        let lock = NSLock()
        var failures: [Error] = []
        var successes = 0
        DispatchQueue.concurrentPerform(iterations: sources.count) { index in
            do {
                try AppBundleInstaller.copyIntoPlace(from: sources[index], to: destination)
                lock.lock()
                successes += 1
                lock.unlock()
            } catch {
                lock.lock()
                failures.append(error)
                lock.unlock()
            }
        }
        XCTAssertGreaterThan(successes, 0, "\(failures)")
        XCTAssertTrue(["new0", "new1"].contains(try contents(destination)))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(
            atPath: destination.deletingLastPathComponent().path),
            ["Provider Monitor.app"])
    }
}
