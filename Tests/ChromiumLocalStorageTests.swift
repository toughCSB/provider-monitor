import Foundation
import XCTest
@testable import ProviderMonitor

final class ChromiumLocalStorageTests: XCTestCase {
    private let key = Data("_https://chatgpt.com\0\u{01}oai/apps/accountSwitchSessions".utf8)

    func testCurrentManifestIgnoresObsoleteTableAndHonorsTombstone() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        try table([entry(key, sequence: 1, value: [1, 111, 108, 100])]).write(to: directory.appendingPathComponent("000001.ldb"))
        try table([entry(key, sequence: 3, value: nil)]).write(to: directory.appendingPathComponent("000002.ldb"))
        let edits = [manifestEdit(add: [1]), manifestEdit(delete: [1], add: [2])]
        try log(edits.map { (1, $0) }).write(to: directory.appendingPathComponent("MANIFEST-000003"))
        try Data("MANIFEST-000003\n".utf8).write(to: directory.appendingPathComponent("CURRENT"))
        try Data().write(to: directory.appendingPathComponent("000004.log"))
        XCTAssertNil(try ChromiumLocalStorage.value(for: key, in: directory))
        XCTAssertEqual(try ChromiumLocalStorage.tableRecordCount(in: directory.appendingPathComponent("000001.ldb")), 1)
    }

    func testFragmentedCurrentWALBeatsSnappyTableAndPreviousWAL() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        try table([entry(key, sequence: 5, value: [1, 111, 108, 100])], compressed: true)
            .write(to: directory.appendingPathComponent("000002.sst"))
        try log([(1, manifestEdit(add: [2], logNumber: 4, previousLog: 3))])
            .write(to: directory.appendingPathComponent("MANIFEST-000001"))
        try Data("MANIFEST-000001\n".utf8).write(to: directory.appendingPathComponent("CURRENT"))
        try log([(1, batch(sequence: 6, put: [1, 112, 114, 101, 118]))])
            .write(to: directory.appendingPathComponent("000003.log"))
        // The FIRST record fills a physical 32 KiB log block exactly.
        let latest = fixed(7, count: 8) + fixed(2, count: 4)
            + [1] + prefixed(Array("noise".utf8)) + prefixed([UInt8](repeating: 120, count: 32_740))
            + [1] + prefixed([UInt8](key)) + prefixed([1, 110, 101, 119])
        try log([(2, Array(latest.prefix(32_761))), (4, Array(latest.dropFirst(32_761)))])
            .write(to: directory.appendingPathComponent("000004.log"))
        XCTAssertEqual(try ChromiumLocalStorage.stringValue(for: key, in: directory), "new")
        XCTAssertEqual(try ChromiumLocalStorage.tableKeys(in: directory.appendingPathComponent("000002.sst"),
                                                          containing: Data("accountSwitch".utf8)), [key])
    }

    func testWALDeleteWinsOverTableAndOlderPut() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        try table([entry(key, sequence: 1, value: [1, 111, 108, 100])])
            .write(to: directory.appendingPathComponent("000002.ldb"))
        try log([(1, manifestEdit(add: [2], logNumber: 4, previousLog: 3))])
            .write(to: directory.appendingPathComponent("MANIFEST-000001"))
        try Data("MANIFEST-000001\n".utf8).write(to: directory.appendingPathComponent("CURRENT"))
        try log([(1, batch(sequence: 2, put: [1, 111, 108, 100, 50]))])
            .write(to: directory.appendingPathComponent("000003.log"))
        try log([(1, batch(sequence: 3, put: nil))])
            .write(to: directory.appendingPathComponent("000004.log"))
        XCTAssertNil(try ChromiumLocalStorage.value(for: key, in: directory))
    }

    func testCorruptTableChecksumFailsClosed() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        var bytes = [UInt8](table([entry(key, sequence: 1, value: [1, 120])]))
        bytes[8] ^= 0xff
        try Data(bytes).write(to: directory.appendingPathComponent("000002.ldb"))
        try log([(1, manifestEdit(add: [2], logNumber: 4))])
            .write(to: directory.appendingPathComponent("MANIFEST-000001"))
        try Data("MANIFEST-000001\n".utf8).write(to: directory.appendingPathComponent("CURRENT"))
        try Data().write(to: directory.appendingPathComponent("000004.log"))
        XCTAssertThrowsError(try ChromiumLocalStorage.value(for: key, in: directory))
    }

    func testMissingCurrentLogFailsClosed() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        try table([entry(key, sequence: 1, value: [1, 120])])
            .write(to: directory.appendingPathComponent("000002.ldb"))
        try log([(1, manifestEdit(add: [2], logNumber: 4))])
            .write(to: directory.appendingPathComponent("MANIFEST-000001"))
        try Data("MANIFEST-000001\n".utf8).write(to: directory.appendingPathComponent("CURRENT"))
        XCTAssertThrowsError(try ChromiumLocalStorage.value(for: key, in: directory))
    }

    func testChromiumStringMarkers() throws {
        XCTAssertEqual(try ChromiumLocalStorage.decodeString(Data([1, 0x43, 0xe9])), "Cé")
        XCTAssertEqual(try ChromiumLocalStorage.decodeString(Data([0, 0x41, 0, 0x42, 0])), "AB")
        XCTAssertThrowsError(try ChromiumLocalStorage.decodeString(Data([0, 0x41])))
        XCTAssertThrowsError(try ChromiumLocalStorage.decodeString(Data([2, 0x41])))
    }

    private func scratch() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("chromium-local-storage-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func varint(_ value: UInt64) -> [UInt8] {
        var value = value
        var result: [UInt8] = []
        while value >= 128 {
            result.append(UInt8(value & 127) | 128)
            value >>= 7
        }
        result.append(UInt8(value))
        return result
    }

    private func fixed(_ value: UInt64, count: Int) -> [UInt8] {
        (0..<count).map { UInt8(truncatingIfNeeded: value >> ($0 * 8)) }
    }

    private func prefixed(_ bytes: [UInt8]) -> [UInt8] { varint(UInt64(bytes.count)) + bytes }

    private func manifestEdit(delete: [UInt64] = [], add: [UInt64], logNumber: UInt64 = 4,
                              previousLog: UInt64 = 0) -> [UInt8] {
        var bytes = varint(1) + prefixed(Array("leveldb.BytewiseComparator".utf8))
        bytes += varint(2) + varint(logNumber)
        if previousLog > 0 { bytes += varint(9) + varint(previousLog) }
        for file in delete { bytes += varint(6) + varint(0) + varint(file) }
        for file in add {
            bytes += varint(7) + varint(0) + varint(file) + varint(100)
            bytes += prefixed([0]) + prefixed([255])
        }
        return bytes
    }

    private func batch(sequence: UInt64, put value: [UInt8]?) -> [UInt8] {
        fixed(sequence, count: 8) + fixed(1, count: 4) + [value == nil ? 0 : 1] +
            prefixed([UInt8](key)) + (value.map { prefixed($0) } ?? [])
    }

    private func entry(_ key: Data, sequence: UInt64, value: [UInt8]?) -> ([UInt8], [UInt8]) {
        ([UInt8](key) + fixed((sequence << 8) | (value == nil ? 0 : 1), count: 8), value ?? [])
    }

    private func table(_ entries: [([UInt8], [UInt8])], compressed: Bool = false) -> Data {
        let dataBlock = block(entries)
        let dataStored = storedBlock(dataBlock, compressed: compressed)
        let indexKey = entries.last!.0
        let indexBlock = block([(indexKey, varint(0) + varint(UInt64(dataBlock.countForStored(compressed))))])
        let indexStored = storedBlock(indexBlock, compressed: false)
        let indexOffset = dataStored.count
        var footer = varint(0) + varint(0) + varint(UInt64(indexOffset)) + varint(UInt64(indexBlock.count))
        footer += [UInt8](repeating: 0, count: 40 - footer.count)
        footer += [0x57, 0xfb, 0x80, 0x8b, 0x24, 0x75, 0x47, 0xdb]
        return Data(dataStored + indexStored + footer)
    }

    private func block(_ entries: [([UInt8], [UInt8])]) -> [UInt8] {
        var bytes: [UInt8] = []
        for (key, value) in entries {
            bytes += varint(0) + varint(UInt64(key.count)) + varint(UInt64(value.count)) + key + value
        }
        return bytes + fixed(0, count: 4) + fixed(1, count: 4)
    }

    private func storedBlock(_ raw: [UInt8], compressed: Bool) -> [UInt8] {
        let payload = compressed ? snappyLiteral(raw) : raw
        let kind: UInt8 = compressed ? 1 : 0
        return payload + [kind] + fixed(UInt64(mask(crc(payload + [kind]))), count: 4)
    }

    private func snappyLiteral(_ bytes: [UInt8]) -> [UInt8] {
        var result = varint(UInt64(bytes.count))
        if bytes.count <= 60 { result.append(UInt8((bytes.count - 1) << 2)) }
        else { result.append(0xf0); result.append(UInt8(bytes.count - 1)) }
        return result + bytes
    }

    private func log(_ records: [(UInt8, [UInt8])]) -> Data {
        var result: [UInt8] = []
        for (kind, body) in records {
            result += fixed(UInt64(mask(crc([kind] + body))), count: 4)
            result += fixed(UInt64(body.count), count: 2) + [kind] + body
        }
        return Data(result)
    }

    private func mask(_ crc: UInt32) -> UInt32 {
        ((crc >> 15) | (crc << 17)) &+ 0xa282ead8
    }

    private func crc(_ bytes: [UInt8]) -> UInt32 {
        var crc: UInt32 = 0xffff_ffff
        for byte in bytes {
            crc ^= UInt32(byte)
            for _ in 0..<8 { crc = (crc >> 1) ^ (crc & 1 == 0 ? 0 : 0x82f6_3b78) }
        }
        return ~crc
    }
}

private extension Array where Element == UInt8 {
    func countForStored(_ compressed: Bool) -> Int {
        if !compressed { return count }
        return count + (count < 128 ? 1 : 2) + (count <= 60 ? 1 : 2)
    }
}
