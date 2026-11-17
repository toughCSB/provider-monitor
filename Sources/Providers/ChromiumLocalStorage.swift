import Foundation

/// A deliberately bounded, read-only LevelDB lookup for Chromium Local Storage.
/// It never opens a DB through LevelDB (which would take a lock or recover files).
enum ChromiumLocalStorage {
    enum ReadError: Error, Equatable {
        case corrupt
        case unsupported
        case tooLarge
        case changedDuringRead
    }

    private static let maxFile = 64 * 1024 * 1024
    private static let maxTotal = 192 * 1024 * 1024
    private static let maxFiles = 512
    private static let maxValue = 16 * 1024 * 1024

    struct Record {
        let key: Data
        let sequence: UInt64
        let value: Data?
    }

    private struct Version {
        var files = Set<UInt64>()
        var logNumber: UInt64?
        var previousLog: UInt64 = 0
    }

    private struct Candidate {
        let sequence: UInt64
        let value: Data?
    }

    private struct Fingerprint: Equatable {
        let size: UInt64
        let modified: Date?
        let inode: UInt64?
    }

    /// Missing and deleted keys both return nil. Corrupt, unsupported, oversized,
    /// or changing databases throw instead of returning a stale value.
    static func value(for key: Data, in directory: URL) throws -> Data? {
        let fm = FileManager.default
        let beforeNames = try relevantNames(in: directory)
        var budget = maxTotal
        let currentURL = directory.appendingPathComponent("CURRENT")
        let current = try read(currentURL, budget: &budget)
        guard let manifestName = String(data: Data(current), encoding: .utf8)?.trimmingCharacters(in: .newlines),
              manifestName.range(of: #"^MANIFEST-[0-9]{6,}$"#, options: .regularExpression) != nil else {
            throw ReadError.corrupt
        }
        let manifestURL = directory.appendingPathComponent(manifestName)
        let manifest = try read(manifestURL, budget: &budget)
        let version = try parseManifest(manifest)
        guard let logNumber = version.logNumber, version.files.count <= maxFiles else {
            throw ReadError.corrupt
        }

        var best: Candidate?
        var visited: [URL: Fingerprint] = [:]
        visited[currentURL] = try fingerprint(currentURL)
        visited[manifestURL] = try fingerprint(manifestURL)

        for number in version.files.sorted() {
            let stem = String(format: "%06llu", number)
            let ldb = directory.appendingPathComponent(stem + ".ldb")
            let sst = directory.appendingPathComponent(stem + ".sst")
            let url: URL
            if fm.fileExists(atPath: ldb.path) && !fm.fileExists(atPath: sst.path) { url = ldb }
            else if fm.fileExists(atPath: sst.path) && !fm.fileExists(atPath: ldb.path) { url = sst }
            else { throw ReadError.corrupt }
            let data = try read(url, budget: &budget)
            visited[url] = try fingerprint(url)
            for record in try tableRecords(data, matching: key) {
                try consider(record, best: &best)
            }
        }

        let logs = beforeNames.compactMap { name -> (UInt64, String)? in
            guard name.hasSuffix(".log"), let number = UInt64(name.dropLast(4)),
                  number >= logNumber || number == version.previousLog else { return nil }
            return (number, name)
        }.sorted { $0.0 < $1.0 }
        guard logs.count <= maxFiles else { throw ReadError.tooLarge }
        guard logNumber == 0 || logs.contains(where: { $0.0 == logNumber }) else {
            throw ReadError.changedDuringRead
        }
        // An empty current WAL can legitimately be absent immediately after rotation,
        // but a missing named WAL during this snapshot must not hide newer writes.
        for (_, name) in logs {
            let url = directory.appendingPathComponent(name)
            let data = try read(url, budget: &budget)
            visited[url] = try fingerprint(url)
            for record in try batchRecords(data, matching: key) {
                try consider(record, best: &best)
            }
        }

        guard try relevantNames(in: directory) == beforeNames,
              try read(currentURL, budget: &budget) == current else {
            throw ReadError.changedDuringRead
        }
        for (url, old) in visited where try fingerprint(url) != old {
            throw ReadError.changedDuringRead
        }
        return best?.value
    }

    /// Chromium's Local Storage value encoding: 0 = UTF-16LE, 1 = Latin-1.
    static func stringValue(for key: Data, in directory: URL) throws -> String? {
        guard let data = try value(for: key, in: directory) else { return nil }
        return try decodeString(data)
    }

    static func decodeString(_ data: Data) throws -> String {
        guard let marker = data.first else { throw ReadError.corrupt }
        let body = Data(data.dropFirst())
        switch marker {
        case 0:
            guard body.count.isMultiple(of: 2), let text = String(data: body, encoding: .utf16LittleEndian) else {
                throw ReadError.corrupt
            }
            return text
        case 1:
            guard let text = String(data: body, encoding: .isoLatin1) else { throw ReadError.corrupt }
            return text
        default: throw ReadError.unsupported
        }
    }

    /// Internal diagnostic hook. A standalone Swift target can compile this file
    /// alongside its main file and inspect counts without printing any values.
    static func tableRecordCount(in file: URL) throws -> Int {
        var budget = maxTotal
        return try tableRecords(read(file, budget: &budget)).count
    }

    /// Returns only matching key names for a local diagnostic. Never returns values.
    static func tableKeys(in file: URL, containing needle: Data) throws -> [Data] {
        var budget = maxTotal
        return try tableRecords(read(file, budget: &budget))
            .map(\.key).filter { $0.range(of: needle) != nil }
    }

    private static func consider(_ record: Record, best: inout Candidate?) throws {
        if let existing = best, existing.sequence == record.sequence {
            guard existing.value == record.value else { throw ReadError.corrupt }
        } else if best == nil || record.sequence > best!.sequence {
            best = Candidate(sequence: record.sequence, value: record.value)
        }
    }

    private static func relevantNames(in directory: URL) throws -> Set<String> {
        let names = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        return Set(names.filter { $0 == "CURRENT" || $0.hasPrefix("MANIFEST-") ||
            $0.hasSuffix(".log") || $0.hasSuffix(".ldb") || $0.hasSuffix(".sst") })
    }

    private static func fingerprint(_ url: URL) throws -> Fingerprint {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        guard attrs[.type] as? FileAttributeType == .typeRegular,
              let size = attrs[.size] as? NSNumber else { throw ReadError.corrupt }
        return Fingerprint(size: size.uint64Value,
                           modified: attrs[.modificationDate] as? Date,
                           inode: (attrs[.systemFileNumber] as? NSNumber)?.uint64Value)
    }

    private static func read(_ url: URL, budget: inout Int) throws -> [UInt8] {
        let before = try fingerprint(url)
        guard before.size <= maxFile, before.size <= budget else { throw ReadError.tooLarge }
        let bytes = [UInt8](try Data(contentsOf: url))
        guard bytes.count == before.size, try fingerprint(url) == before else {
            throw ReadError.changedDuringRead
        }
        budget -= bytes.count
        return bytes
    }

    private struct Cursor {
        let bytes: [UInt8]
        var offset = 0
        var remaining: Int { bytes.count - offset }

        mutating func byte() throws -> UInt8 {
            guard offset < bytes.count else { throw ReadError.corrupt }
            defer { offset += 1 }
            return bytes[offset]
        }

        mutating func fixed(_ count: Int) throws -> UInt64 {
            guard count <= 8, count <= remaining else { throw ReadError.corrupt }
            var result: UInt64 = 0
            for shift in 0..<count { result |= UInt64(try byte()) << (shift * 8) }
            return result
        }

        mutating func varint() throws -> UInt64 {
            var result: UInt64 = 0
            for shift in stride(from: 0, through: 63, by: 7) {
                let part = try byte()
                if shift == 63 && part > 1 { throw ReadError.corrupt }
                result |= UInt64(part & 0x7f) << shift
                if part & 0x80 == 0 { return result }
            }
            throw ReadError.corrupt
        }

        mutating func slice(_ count: Int) throws -> [UInt8] {
            guard count >= 0, count <= remaining else { throw ReadError.corrupt }
            defer { offset += count }
            return Array(bytes[offset..<(offset + count)])
        }

        mutating func lengthPrefixed() throws -> [UInt8] {
            let count = try varint()
            guard count <= UInt64(remaining), count <= UInt64(maxValue) else { throw ReadError.tooLarge }
            return try slice(Int(count))
        }

        mutating func skipLengthPrefixed() throws {
            let count = try varint()
            guard count <= UInt64(remaining), count <= UInt64(maxValue) else { throw ReadError.tooLarge }
            offset += Int(count)
        }
    }

    private static func parseManifest(_ bytes: [UInt8]) throws -> Version {
        var version = Version()
        for edit in try logRecords(bytes, allowTruncatedTail: false) {
            var cursor = Cursor(bytes: edit)
            while cursor.remaining > 0 {
                switch try cursor.varint() {
                case 1:
                    guard try cursor.lengthPrefixed() == Array("leveldb.BytewiseComparator".utf8) else {
                        throw ReadError.unsupported
                    }
                case 2: version.logNumber = try cursor.varint()
                case 3, 4: _ = try cursor.varint()
                case 5:
                    _ = try cursor.varint()
                    _ = try cursor.lengthPrefixed()
                case 6:
                    let level = try cursor.varint()
                    let number = try cursor.varint()
                    guard level <= 6 else { throw ReadError.unsupported }
                    version.files.remove(number)
                case 7:
                    let level = try cursor.varint()
                    let number = try cursor.varint()
                    _ = try cursor.varint() // file size
                    _ = try cursor.lengthPrefixed() // smallest internal key
                    _ = try cursor.lengthPrefixed() // largest internal key
                    guard level <= 6 else { throw ReadError.unsupported }
                    version.files.insert(number)
                    guard version.files.count <= maxFiles else { throw ReadError.tooLarge }
                case 9: version.previousLog = try cursor.varint()
                default: throw ReadError.unsupported
                }
            }
        }
        return version
    }

    // LevelDB's 32 KiB physical-log framing. A partial tail is only allowed for
    // a live WAL, never for the MANIFEST.
    private static func logRecords(_ bytes: [UInt8], allowTruncatedTail: Bool) throws -> [[UInt8]] {
        var records: [[UInt8]] = []
        var fragment: [UInt8]?
        var offset = 0
        while offset < bytes.count {
            let blockRemaining = 32768 - offset % 32768
            if blockRemaining < 7 {
                let end = min(bytes.count, offset + blockRemaining)
                guard bytes[offset..<end].allSatisfy({ $0 == 0 }) else { throw ReadError.corrupt }
                offset = end
                continue
            }
            if bytes.count - offset < 7 {
                throw allowTruncatedTail ? ReadError.changedDuringRead : ReadError.corrupt
            }
            var cursor = Cursor(bytes: bytes)
            cursor.offset = offset
            let checksum = UInt32(try cursor.fixed(4))
            let length = Int(try cursor.fixed(2))
            let kind = try cursor.byte()
            guard length <= blockRemaining - 7 else { throw ReadError.corrupt }
            if bytes.count - cursor.offset < length {
                throw allowTruncatedTail ? ReadError.changedDuringRead : ReadError.corrupt
            }
            let part = try cursor.slice(length)
            offset = cursor.offset
            if kind == 0 && length == 0 && checksum == 0 {
                guard bytes[(offset - 7)..<min(bytes.count, (offset - 7) + blockRemaining)].allSatisfy({ $0 == 0 }) else {
                    throw ReadError.corrupt
                }
                offset = min(bytes.count, (offset - 7) + blockRemaining)
                continue
            }
            guard kind >= 1 && kind <= 4,
                  unmask(checksum) == crc32c([kind] + part) else { throw ReadError.corrupt }
            switch kind {
            case 1:
                guard fragment == nil else { throw ReadError.corrupt }
                records.append(part)
            case 2:
                guard fragment == nil else { throw ReadError.corrupt }
                fragment = part
            case 3, 4:
                guard var pending = fragment, pending.count + part.count <= maxFile else { throw ReadError.corrupt }
                pending += part
                if kind == 4 { records.append(pending); fragment = nil }
                else { fragment = pending }
            default: throw ReadError.corrupt
            }
        }
        if fragment != nil {
            throw allowTruncatedTail ? ReadError.changedDuringRead : ReadError.corrupt
        }
        return records
    }

    private static func batchRecords(_ bytes: [UInt8], matching target: Data) throws -> [Record] {
        var records: [Record] = []
        for batch in try logRecords(bytes, allowTruncatedTail: true) {
            var cursor = Cursor(bytes: batch)
            let start = try cursor.fixed(8)
            let count = try cursor.fixed(4)
            guard count <= 1_000_000, start <= (UInt64.max >> 8) - count else { throw ReadError.tooLarge }
            for index in 0..<count {
                let kind = try cursor.byte()
                guard kind == 0 || kind == 1 else { throw ReadError.unsupported }
                let key = try cursor.lengthPrefixed()
                if Data(key) == target {
                    let value = kind == 1 ? Data(try cursor.lengthPrefixed()) : nil
                    records.append(Record(key: target, sequence: start + index, value: value))
                } else if kind == 1 {
                    try cursor.skipLengthPrefixed()
                }
            }
            guard cursor.remaining == 0 else { throw ReadError.corrupt }
        }
        return records
    }

    private static func tableRecords(_ bytes: [UInt8], matching target: Data? = nil) throws -> [Record] {
        guard bytes.count >= 48 else { throw ReadError.corrupt }
        var footer = Cursor(bytes: Array(bytes.suffix(48)))
        _ = try blockHandle(&footer) // metaindex
        let index = try blockHandle(&footer)
        guard Array(bytes.suffix(8)) == [0x57, 0xfb, 0x80, 0x8b, 0x24, 0x75, 0x47, 0xdb] else {
            throw ReadError.corrupt
        }
        var records: [Record] = []
        var previousIndexKey: Data?
        for (indexKey, handle) in try blockEntries(try block(bytes, handle: index)) {
            guard indexKey.count >= 8 else { throw ReadError.corrupt }
            let indexUserKey = Data(indexKey.dropLast(8))
            defer { previousIndexKey = indexUserKey }
            if let target {
                if indexUserKey.lexicographicallyPrecedes(target) { continue }
                if let previousIndexKey, target.lexicographicallyPrecedes(previousIndexKey) { break }
            }
            var cursor = Cursor(bytes: handle)
            let dataHandle = try blockHandle(&cursor)
            guard cursor.remaining == 0 else { throw ReadError.corrupt }
            for (internalKey, value) in try blockEntries(try block(bytes, handle: dataHandle)) {
                guard internalKey.count >= 8 else { throw ReadError.corrupt }
                let trailer = internalKey.suffix(8)
                var keyCursor = Cursor(bytes: Array(trailer))
                let packed = try keyCursor.fixed(8)
                let kind = UInt8(packed & 0xff)
                guard kind == 0 || kind == 1 else { throw ReadError.unsupported }
                guard value.count <= maxValue else { throw ReadError.tooLarge }
                let userKey = Data(internalKey.dropLast(8))
                if target == nil || userKey == target {
                    records.append(Record(key: userKey, sequence: packed >> 8,
                                          value: kind == 1 ? Data(value) : nil))
                }
            }
        }
        return records
    }

    private static func blockHandle(_ cursor: inout Cursor) throws -> (Int, Int) {
        let offset = try cursor.varint()
        let size = try cursor.varint()
        guard offset <= Int.max, size <= maxFile else { throw ReadError.tooLarge }
        return (Int(offset), Int(size))
    }

    private static func block(_ file: [UInt8], handle: (Int, Int)) throws -> [UInt8] {
        let (offset, size) = handle
        guard offset <= file.count, file.count - offset >= 5,
              size <= file.count - offset - 5 else {
            throw ReadError.corrupt
        }
        let payload = Array(file[offset..<(offset + size)])
        let kind = file[offset + size]
        var crcCursor = Cursor(bytes: Array(file[(offset + size + 1)..<(offset + size + 5)]))
        let checksum = UInt32(try crcCursor.fixed(4))
        guard unmask(checksum) == crc32c(payload + [kind]) else { throw ReadError.corrupt }
        switch kind {
        case 0: return payload
        case 1: return try snappy(payload)
        default: throw ReadError.unsupported
        }
    }

    private static func blockEntries(_ bytes: [UInt8]) throws -> [([UInt8], [UInt8])] {
        guard bytes.count >= 4 else { throw ReadError.corrupt }
        var tail = Cursor(bytes: Array(bytes.suffix(4)))
        let restarts = try tail.fixed(4)
        guard restarts > 0, restarts <= UInt64((bytes.count - 4) / 4) else { throw ReadError.corrupt }
        let end = bytes.count - 4 - Int(restarts) * 4
        var cursor = Cursor(bytes: Array(bytes.prefix(end)))
        var previous: [UInt8] = []
        var entries: [([UInt8], [UInt8])] = []
        while cursor.remaining > 0 {
            let shared = try cursor.varint()
            let nonShared = try cursor.varint()
            let valueSize = try cursor.varint()
            guard shared <= previous.count, nonShared <= UInt64(cursor.remaining),
                  valueSize <= UInt64(cursor.remaining) - nonShared else { throw ReadError.corrupt }
            let key = Array(previous.prefix(Int(shared))) + (try cursor.slice(Int(nonShared)))
            let value = try cursor.slice(Int(valueSize))
            entries.append((key, value))
            previous = key
            guard entries.count <= 1_000_000 else { throw ReadError.tooLarge }
        }
        return entries
    }

    private static func snappy(_ bytes: [UInt8]) throws -> [UInt8] {
        var cursor = Cursor(bytes: bytes)
        let length = try cursor.varint()
        guard length <= maxFile else { throw ReadError.tooLarge }
        var output: [UInt8] = []
        output.reserveCapacity(Int(length))
        while cursor.remaining > 0 {
            let tag = try cursor.byte()
            if tag & 3 == 0 {
                let short = Int(tag >> 2)
                let count: Int
                if short < 60 { count = short + 1 }
                else { count = Int(try cursor.fixed(short - 59)) + 1 }
                guard count <= Int(length) - output.count else { throw ReadError.corrupt }
                output += try cursor.slice(count)
            } else {
                let count: Int
                let distance: Int
                switch tag & 3 {
                case 1:
                    count = 4 + Int((tag >> 2) & 7)
                    distance = Int(tag & 0xe0) << 3 | Int(try cursor.byte())
                case 2:
                    count = 1 + Int(tag >> 2)
                    distance = Int(try cursor.fixed(2))
                default:
                    count = 1 + Int(tag >> 2)
                    distance = Int(try cursor.fixed(4))
                }
                guard distance > 0, distance <= output.count,
                      count <= Int(length) - output.count else { throw ReadError.corrupt }
                for _ in 0..<count { output.append(output[output.count - distance]) }
            }
        }
        guard output.count == length else { throw ReadError.corrupt }
        return output
    }

    private static func unmask(_ masked: UInt32) -> UInt32 {
        let value = masked &- 0xa282ead8
        return (value >> 17) | (value << 15)
    }

    private static let crcTable: [UInt32] = (0..<256).map { index in
        var crc = UInt32(index)
        for _ in 0..<8 { crc = (crc >> 1) ^ (crc & 1 == 0 ? 0 : 0x82f6_3b78) }
        return crc
    }

    private static func crc32c(_ bytes: [UInt8]) -> UInt32 {
        var crc: UInt32 = 0xffff_ffff
        for byte in bytes {
            crc = (crc >> 8) ^ crcTable[Int((crc ^ UInt32(byte)) & 0xff)]
        }
        return ~crc
    }
}
