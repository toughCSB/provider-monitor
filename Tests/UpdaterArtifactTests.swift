import XCTest
@testable import ProviderMonitor

@MainActor
final class UpdaterArtifactTests: XCTestCase {
    private func release(assets: String) -> Data {
        Data("""
        {"tag_name":"v2.3.4",
         "html_url":"https://github.com/toughCSB/provider-monitor/releases/tag/v2.3.4",
         "assets":[\(assets)]}
        """.utf8)
    }

    func testOnlyExactVersionDiskImageIsInstallable() throws {
        let assets = """
        {"name":"ProviderMonitor-2.3.3-unsigned.dmg","browser_download_url":"https://example.invalid/old"},
        {"name":"OtherProduct-2.3.4.dmg","browser_download_url":"https://example.invalid/other"},
        {"name":"ProviderMonitor-2.3.4-unsigned.dmg","browser_download_url":"https://example.invalid/exact","size":1048576}
        """
        let selected = try Updater.release(from: release(assets: assets))
        XCTAssertEqual(selected.assetURL.absoluteString, "https://example.invalid/exact")
        XCTAssertEqual(selected.expectedSize, 1_048_576)
    }

    func testMissingInstallerDoesNotFallBackToAnotherDiskImage() {
        let assets = """
        {"name":"ProviderMonitor-2.3.3-unsigned.dmg","browser_download_url":"https://example.invalid/old"},
        {"name":"unrelated.dmg","browser_download_url":"https://example.invalid/unrelated"}
        """
        XCTAssertThrowsError(try Updater.release(from: release(assets: assets)))
    }

    func testFileDigestStreamsCorrectlyAcrossChunkBoundary() throws {
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("UpdaterArtifactTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: file) }
        let bytes = Data(repeating: 0x5a, count: 1_048_577)
        try bytes.write(to: file)
        XCTAssertEqual(try Updater.sha256(ofFile: file), Updater.sha256(of: bytes))
    }
}
