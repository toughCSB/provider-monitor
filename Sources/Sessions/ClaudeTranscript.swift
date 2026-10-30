import Foundation

/// What a Claude Code session is doing, read from its transcript.
///
/// Claude Code writes `status` into `~/.claude/sessions/<pid>.json` from one
/// place only: an effect inside the terminal interface's render loop. A session
/// hosted by the Claude desktop app runs the same binary with no terminal
/// interface, so that effect never fires and the record carries no status at
/// all — which is why every desktop session read as permanently idle, and why
/// the usage ring never polled hard while one was working.
///
/// The transcript is the other thing Claude Code writes, and the engine writes
/// it rather than the interface, so it is there for both surfaces:
/// `~/.claude/projects/<slug>/<sessionId>.jsonl`, one JSON object per line,
/// appended as the turn goes.
enum ClaudeTranscript {
    /// Whether the session is mid-turn.
    ///
    /// Two cases and not three, deliberately. Nothing in the transcript
    /// separates a tool waiting on your permission from a tool that is simply
    /// taking its time — there is no record for a permission prompt — so this
    /// never claims `waiting`. Only the terminal interface knows that, and only
    /// the terminal interface writes it to the registry.
    enum Turn: Equatable { case inFlight, finished }

    /// How much of the end of the file to look at. The last few records settle
    /// it, and these transcripts run to megabytes.
    static let tailBytes = 64 * 1024

    // MARK: - Where the file is

    /// The directory Claude Code files a working directory's transcripts under:
    /// the path with every `/` and `.` turned into `-`, so
    /// `/Users/x/app/.claude/worktrees/y` becomes
    /// `-Users-x-app--claude-worktrees-y`.
    static func projectSlug(forCWD cwd: String) -> String {
        String(cwd.map { $0 == "/" || $0 == "." ? "-" : $0 })
    }

    /// The transcript for one session, or nil if it has not been written yet.
    ///
    /// `scanning` is what makes this affordable to call on a timer: the slug is
    /// derived from the session's own `cwd` and is right almost always, so the
    /// usual cost is a single `stat`. The directory scan is the fallback for a
    /// session that has moved since it started — resumed in a worktree, say —
    /// and keeps its transcript where it was first written.
    static func transcript(
        forSessionID sessionID: String,
        cwd: String,
        in projects: URL,
        scanning: Bool,
        fileManager: FileManager = .default
    ) -> URL? {
        let file = sessionID + ".jsonl"
        let direct = projects
            .appendingPathComponent(projectSlug(forCWD: cwd))
            .appendingPathComponent(file)
        if fileManager.fileExists(atPath: direct.path) { return direct }
        guard scanning else { return nil }

        let folders = (try? fileManager.contentsOfDirectory(atPath: projects.path)) ?? []
        for folder in folders {
            let candidate = projects
                .appendingPathComponent(folder)
                .appendingPathComponent(file)
            if fileManager.fileExists(atPath: candidate.path) { return candidate }
        }
        return nil
    }

    // MARK: - What it says

    static func turn(at url: URL, bytes: Int = tailBytes) -> Turn? {
        tail(of: url, bytes: bytes).flatMap(turn(inTail:))
    }

    /// The last records of the file, without reading the rest of it.
    static func tail(of url: URL, bytes: Int = tailBytes) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let end = try? handle.seekToEnd() else { return nil }
        let start = end > UInt64(bytes) ? end - UInt64(bytes) : 0
        guard (try? handle.seek(toOffset: start)) != nil else { return nil }
        return try? handle.readToEnd()
    }

    /// The state machine, kept pure so it can be tested without a file.
    ///
    /// Only `user` and `assistant` lines are allowed to decide. The transcript
    /// also carries bookkeeping — `bridge-session`, `atis-latch`, `frame-link`,
    /// `custom-title` and more — which Claude Code keeps appending while a
    /// session sits doing nothing. That is why "was the file touched recently",
    /// which is all the Codex monitor can manage, is not good enough here: on
    /// this Mac it reported a session that had been parked for an hour as
    /// working. An allow-list rather than a list of kinds to skip, because the
    /// bookkeeping kinds are added to between releases.
    ///
    /// Nil when the tail holds no conversation at all — a session that has been
    /// opened and not yet used. The caller leaves such a session as it found it
    /// rather than guessing.
    static func turn(inTail data: Data) -> Turn? {
        let lines = data.split(separator: UInt8(ascii: "\n"), omittingEmptySubsequences: true)
        for index in lines.indices.reversed() {
            let line = lines[index]
            // The first line of the tail is usually cut in half; it fails to
            // parse and is skipped. A malformed later line may be a record
            // still being appended, so an older finished turn is not proof.
            guard let json = (try? JSONSerialization.jsonObject(with: Data(line)))
                    as? [String: Any] else {
                if index == lines.startIndex { continue }
                return nil
            }
            // A subagent's records are interleaved with its parent's, and the
            // parent's turn is the one being reported on.
            if json["isSidechain"] as? Bool == true { continue }

            switch json["type"] as? String {
            case "assistant":
                // `tool_use` is the only stop reason that means the turn goes
                // on; only an explicit end reason proves that it finished.
                // A partially written assistant record cannot make a session idle.
                let message = json["message"] as? [String: Any]
                switch message?["stop_reason"] as? String {
                case "tool_use": return .inFlight
                case "end_turn", "stop_sequence", "max_tokens": return .finished
                default: return nil
                }
            case "user":
                return isInterruption(json) ? .finished : .inFlight
            default:
                continue
            }
        }
        return nil
    }

    /// Pressing Esc writes a user turn saying so, which is what makes "stopped"
    /// something the notch can know rather than wait out with a timeout. Two
    /// wordings exist, and both start the same way.
    private static let interruption = "[Request interrupted by user"

    static func isInterruption(_ json: [String: Any]) -> Bool {
        guard let message = json["message"] as? [String: Any] else { return false }
        switch message["content"] {
        case let text as String:
            return text.hasPrefix(interruption)
        case let blocks as [Any]:
            return blocks.contains { block in
                ((block as? [String: Any])?["text"] as? String)?.hasPrefix(interruption) == true
            }
        default:
            return false
        }
    }
}

/// Reads transcripts on a timer, and remembers enough not to read them twice.
///
/// One instance per monitor. A tick that finds a transcript untouched since the
/// last one costs a `stat` and nothing else — the tail is read only when the
/// file has actually grown.
final class ClaudeTranscriptReader {
    private struct Cached {
        let modified: Date
        let size: UInt64
        let fileNumber: UInt64?
        let turn: ClaudeTranscript.Turn?
    }

    private let projects: URL
    private let fileManager: FileManager
    private let clock: () -> Date
    private let negativeRetryInterval: TimeInterval
    /// Where each session's transcript turned out to be. Sessions come and go;
    /// this is bounded by how many have run since the app launched.
    private var paths: [String: URL] = [:]
    private var cache: [String: Cached] = [:]
    /// When each session last changed what it was doing.
    ///
    /// The transcript's timestamp is when it was last *appended to*, which
    /// moves every few seconds through a long turn. Reported as-is it would
    /// hold the tooltip's elapsed time at zero for as long as the session
    /// worked, and hand the notch a new value to animate on every tick. What
    /// the notch means by `since` is when the state was entered, so the moment
    /// of the change is kept and the appends in between are ignored.
    private var entered: [String: (turn: ClaudeTranscript.Turn, at: Date)] = [:]
    /// A failed fallback scan is retried after a bounded delay. The direct
    /// slug is still checked on every tick, so a new transcript there is immediate.
    private var nextScan: [String: Date] = [:]

    init(projects: URL, fileManager: FileManager = .default,
         negativeRetryInterval: TimeInterval = 30,
         clock: @escaping () -> Date = Date.init) {
        self.projects = projects
        self.fileManager = fileManager
        self.negativeRetryInterval = negativeRetryInterval
        self.clock = clock
    }

    /// What the session is doing, and when it last moved. Nil when there is no
    /// transcript to read, or nothing said in it yet.
    func activity(sessionID: String, cwd: String) -> (turn: ClaudeTranscript.Turn, since: Date)? {
        let now = clock()
        guard var url = path(sessionID: sessionID, cwd: cwd, now: now) else { return nil }
        var attributes = try? fileManager.attributesOfItem(atPath: url.path)
        if attributes == nil {
            // The file may have moved, or its directory may have been replaced.
            // Discard state tied to the old file and search the fallback now.
            paths.removeValue(forKey: sessionID)
            cache.removeValue(forKey: sessionID)
            entered.removeValue(forKey: sessionID)
            guard let relocated = path(sessionID: sessionID, cwd: cwd,
                                       now: now, forceScan: true) else { return nil }
            url = relocated
            attributes = try? fileManager.attributesOfItem(atPath: url.path)
        }
        guard let attributes,
              let modified = attributes[.modificationDate] as? Date else { return nil }
        let size = (attributes[.size] as? NSNumber)?.uint64Value ?? 0
        let fileNumber = (attributes[.systemFileNumber] as? NSNumber)?.uint64Value

        let turn: ClaudeTranscript.Turn?
        if let cached = cache[sessionID], cached.modified == modified,
           cached.size == size, cached.fileNumber == fileNumber {
            turn = cached.turn
        } else {
            if let cached = cache[sessionID], cached.fileNumber != fileNumber {
                entered.removeValue(forKey: sessionID)
            }
            turn = ClaudeTranscript.turn(at: url)
            cache[sessionID] = Cached(modified: modified, size: size,
                                      fileNumber: fileNumber, turn: turn)
        }

        guard let turn else { return nil }
        if let previous = entered[sessionID], previous.turn == turn {
            return (turn, previous.at)
        }
        // First sight of a change, so the file's own timestamp is the closest
        // thing there is to when it happened.
        entered[sessionID] = (turn, modified)
        return (turn, modified)
    }

    private func path(sessionID: String, cwd: String, now: Date,
                      forceScan: Bool = false) -> URL? {
        if let known = paths[sessionID] { return known }
        let direct = projects
            .appendingPathComponent(ClaudeTranscript.projectSlug(forCWD: cwd))
            .appendingPathComponent(sessionID + ".jsonl")
        if fileManager.fileExists(atPath: direct.path) {
            paths[sessionID] = direct
            nextScan.removeValue(forKey: sessionID)
            return direct
        }
        guard forceScan || now >= (nextScan[sessionID] ?? .distantPast) else { return nil }
        nextScan[sessionID] = now.addingTimeInterval(negativeRetryInterval)
        guard let found = ClaudeTranscript.transcript(
            forSessionID: sessionID, cwd: cwd, in: projects,
            scanning: true, fileManager: fileManager
        ) else { return nil }
        paths[sessionID] = found
        nextScan.removeValue(forKey: sessionID)
        return found
    }
}
