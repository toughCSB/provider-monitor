import Foundation

/// Keeps Grok's saved sign-in from ageing out.
///
/// `~/.grok/auth.json` holds an OIDC session that lives six hours, and only the
/// Grok command ever renews it. Measured on a real machine: the file is rewritten
/// with `expires_at` exactly six hours ahead of the write, so on a Mac where the
/// CLI is not run by hand the session is expired for most of the day — and every
/// Grok reading stops, with nothing the user can do from inside Provider Monitor.
/// That is the hole this fills.
///
/// **How it renews, and why that is a compatibility mechanism rather than an
/// interface.** `grok agent --reauth stdio` with an empty stdin starts the CLI
/// headlessly — which is where it refreshes the session — and then exits because
/// no prompt ever arrives. Measured against a copy of the credential on a real
/// machine: about three seconds, no output, no browser, no session file, and
/// `expires_at` moved six hours forward.
///
/// The gate is exact, and it is why this asks so late. A session with sixty
/// seconds left is left alone; one that expired sixty seconds ago is renewed on
/// the spot. The margin is therefore zero rather than a few minutes: asking
/// earlier is a launch the CLI answers with no change at all, which this would
/// then correctly judge a failure and stop retrying.
///
/// This app never performs the OIDC refresh itself. It rotates the stored refresh
/// token, and a second writer would race the CLI for the file.
@MainActor
final class GrokTokenRefresher: ObservableObject {
    enum Outcome: Equatable {
        case idle
        case refreshed(until: Date)
        /// Ran, or could not be run, and the session is still expired. The
        /// string is what the user should be told.
        case failed(String)
    }

    /// A started command: its pid, known synchronously, and a way to wait for it.
    typealias Launcher = @MainActor (URL, TimeInterval) throws
        -> (pid: Int32, exit: () async -> Int32?)

    @Published private(set) var outcome: Outcome = .idle
    /// Set while the command runs. Nothing scans for it today — the headless
    /// run writes no session file — but a launch must never be mistakable for
    /// work, so it is recorded while it is alive.
    private(set) var launchedPID: Int32?

    /// How close to expiry is close enough. Zero: see the note above.
    private let margin: TimeInterval
    private let cooldown: TimeInterval
    private let timeout: TimeInterval
    private let interval: TimeInterval

    private let cli: URL?
    private let launcher: Launcher
    /// The session's expiry, read from disk. One small file, no keychain and no
    /// network: the freshest answer is also the cheapest, which is what lets the
    /// tick ask every minute and the after-check read the same thing again.
    private let expiry: @MainActor () async -> Date?

    private var timer: Timer?
    private var isRunning = false
    /// Exposed for the test that proves `isRunning` is claimed *before* the
    /// first `await` — set any later, a second overlapping call would still see
    /// it as false and both would launch.
    var isRunningForTesting: Bool { isRunning }
    private var lastAttempt: Date?
    /// The expiry a launch was already spent on. One attempt per session, which
    /// is what makes a failure stop instead of looping: a session that did not
    /// renew has the same expiry next tick, and is refused.
    private var attemptedFor: Date?

    init(
        expiry: @escaping @MainActor () async -> Date?,
        cli: URL? = GrokCLI.standalone(),
        margin: TimeInterval = 0,
        cooldown: TimeInterval = 10 * 60,
        timeout: TimeInterval = 45,
        interval: TimeInterval = 60,
        launcher: @escaping Launcher = GrokTokenRefresher.run
    ) {
        self.expiry = expiry
        self.cli = cli
        self.margin = margin
        self.cooldown = cooldown
        self.timeout = timeout
        self.interval = interval
        self.launcher = launcher
    }

    func start() {
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    private func tick() {
        Task { await considerRenewing() }
    }

    // MARK: - The gate

    /// Whether a launch is worth making. Pure, so every branch is testable
    /// without a clock or a subprocess.
    static func shouldRenew(
        expiry: Date?,
        now: Date,
        margin: TimeInterval,
        attemptedFor: Date?,
        lastAttempt: Date?,
        cooldown: TimeInterval
    ) -> Bool {
        // Nothing read yet: never launch on a guess.
        guard let expiry else { return false }
        // Still live — which is also exactly where the command's own gate is
        // shut, so a launch here would change nothing.
        guard expiry.timeIntervalSince(now) < margin else { return false }
        // Already spent an attempt on this exact session. This is the whole
        // no-retry-loop guarantee: a launch that failed to move the expiry
        // leaves the same value here next tick, and never runs again.
        guard expiry != attemptedFor else { return false }
        if let lastAttempt, now.timeIntervalSince(lastAttempt) < cooldown { return false }
        return true
    }

    func considerRenewing(now: Date = Date()) async {
        // Claimed here, synchronously, before anything is awaited: that is what
        // makes "at most one launch" true regardless of how two calls interleave.
        guard !isRunning else { return }
        isRunning = true
        defer { isRunning = false; launchedPID = nil }

        let current = await expiry()
        guard Self.shouldRenew(expiry: current, now: now, margin: margin,
                               attemptedFor: attemptedFor, lastAttempt: lastAttempt,
                               cooldown: cooldown),
              let current
        else { return }

        lastAttempt = now
        attemptedFor = current

        guard let cli else {
            fail("Grok's saved sign-in has expired and the `grok` command isn't "
               + "installed to renew it. Run `grok login` once to sign in again.")
            return
        }
        Log.usage.notice("grok session expired \(now.timeIntervalSince(current), format: .fixed(precision: 0))s ago; renewing via \(cli.path, privacy: .public)")

        let status: Int32?
        do {
            let (pid, exit) = try launcher(cli, timeout)
            launchedPID = pid          // recorded before it can be mistaken for work
            status = await exit()
        } catch {
            fail("Couldn't run `grok` to renew Grok's saved sign-in.")
            Log.usage.error("grok token renewal could not start: \(String(describing: error), privacy: .public)")
            return
        }

        // Judged on the outcome, never on the exit status.
        let after = await expiry()
        guard let after, after > current else {
            fail("Grok usage needs its sign-in renewed — run `grok login` once in a terminal.")
            Log.usage.error("grok token renewal ran (exit \(status ?? -1)) but the expiry did not move; still \(String(describing: after), privacy: .public)")
            return
        }
        outcome = .refreshed(until: after)
        Log.usage.notice("grok session renewed, now expires \(after, privacy: .public)")
    }

    private func fail(_ message: String) {
        outcome = .failed(message)
    }

    // MARK: - Running it

    /// `agent --reauth stdio` is the headless start that refreshes the session.
    /// Output goes nowhere — there is nothing in it worth keeping, and a token
    /// could in principle be echoed into it.
    static let arguments = ["agent", "--reauth", "stdio"]

    static func run(_ cli: URL, timeout: TimeInterval) throws
        -> (pid: Int32, exit: () async -> Int32?) {
        let process = Process()
        process.executableURL = cli
        process.arguments = Self.arguments
        var environment = ProcessInfo.processInfo.environment
        // The child must renew the very session this app reads. An inherited
        // override would point it at a different auth file, which would look
        // like a renewal that changed nothing here.
        environment.removeValue(forKey: "GROK_HOME")
        environment.removeValue(forKey: "GROK_AUTH")
        environment.removeValue(forKey: "GROK_AUTH_PATH")
        // Not the app's own working directory: the CLI discovers config, hooks
        // and plugins from where it is started, and `/` from Finder is not a
        // project the user asked it to read.
        if let scratch = try? ClaudeUsageCLI.scratchDirectory() {
            process.currentDirectoryURL = scratch
            environment["PWD"] = scratch.path
        }
        process.environment = environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()

        let wait: () async -> Int32? = {
            let deadline = Date().addingTimeInterval(timeout)
            while process.isRunning, Date() < deadline {
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            guard !process.isRunning else {
                // A renewal that hangs is never left to linger, and the kill is
                // confirmed rather than assumed.
                process.terminate()
                try? await Task.sleep(nanoseconds: 500_000_000)
                if process.isRunning {
                    kill(process.processIdentifier, SIGKILL)
                    while process.isRunning {
                        try? await Task.sleep(nanoseconds: 50_000_000)
                    }
                }
                return nil
            }
            return process.terminationStatus
        }
        return (process.processIdentifier, wait)
    }
}
