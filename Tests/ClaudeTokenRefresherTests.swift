import XCTest
@testable import ProviderMonitor

/// Which of the several `claude` binaries on a Mac is the right one.
final class ClaudeCLITests: XCTestCase {
    /// The copy inside the desktop app keeps its token in the app's own store
    /// and never writes the login keychain — the item Provider Monitor reads. Renewing
    /// with it would look like it worked and change nothing at all, which is a
    /// far worse failure than finding no command.
    func testTheDesktopAppsOwnCopyIsRefused() {
        let desktop = URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent("Library/Application Support/Claude/claude-code/2.1.260/claude.app/Contents/MacOS/claude")
        XCTAssertTrue(ClaudeCLI.isDesktopOwned(desktop))
        XCTAssertFalse(ClaudeCLI.isDesktopOwned(URL(fileURLWithPath: "/opt/homebrew/bin/claude")))
    }

    /// An empty home rather than the developer's own: `standalone` also looks
    /// in the Node version trees now, and left pointing at the real home this
    /// passed or failed depending on whether the person running it happened to
    /// have installed Claude Code with npm.
    func testNothingInstalledIsNotAnError() throws {
        let home = try makeHome(executablesAt: [])

        XCTAssertNil(ClaudeCLI.standalone(candidates: ["/nowhere/claude"], home: home.path))
    }

    /// npm is still how most people install Claude Code, and under a Node
    /// version manager the binary sits in a directory named for the Node
    /// version — a path no entry in `candidates` can spell. Without it,
    /// `standalone` returns nil on those machines and the renewal that keeps
    /// the ring alive simply never runs.
    func testItFindsAnNpmInstallUnderNVM() throws {
        let home = try makeHome(executablesAt: [".nvm/versions/node/v22.22.3/bin/claude"])

        XCTAssertNotNil(ClaudeCLI.standalone(candidates: [], home: home.path))
    }

    /// Upgrading Node leaves every older tree in place; only the current one is
    /// certainly the install being run.
    func testTheNewestNodeVersionWins() throws {
        let home = try makeHome(executablesAt: [".nvm/versions/node/v20.20.2/bin/claude",
                                                ".nvm/versions/node/v22.22.3/bin/claude"])

        let found = ClaudeCLI.standalone(candidates: [], home: home.path)?.path

        XCTAssertEqual(found?.contains("v22.22.3"), true, "expected v22.22.3, got \(found ?? "nil")")
    }

    func testItFindsAVoltaShim() throws {
        let home = try makeHome(executablesAt: [".volta/bin/claude"])

        XCTAssertNotNil(ClaudeCLI.standalone(candidates: [], home: home.path))
    }

    /// The fixed locations still come first: a copy Claude Code's own installer
    /// maintains is the one to renew with.
    func testAnInstallerCopyOutranksANodeManager() throws {
        let home = try makeHome(executablesAt: [".nvm/versions/node/v22.22.3/bin/claude",
                                                "installer/claude"])

        let found = ClaudeCLI.standalone(
            candidates: [home.appendingPathComponent("installer/claude").path],
            home: home.path
        )?.path

        XCTAssertEqual(found?.hasSuffix("installer/claude"), true)
    }

    private func makeHome(executablesAt paths: [String]) throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ClaudeCLITests.\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }

        for path in paths {
            let url = root.appendingPathComponent(path)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            FileManager.default.createFile(atPath: url.path, contents: Data(),
                                           attributes: [.posixPermissions: 0o755])
        }
        return root
    }
}

/// The gate, the cooldown and the failure path around renewing the token.
@MainActor
final class ClaudeTokenRefresherTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private func inSeconds(_ s: TimeInterval) -> Date { now.addingTimeInterval(s) }

    // MARK: - The gate

    /// Under four minutes, and only then. The margin has to stay *below* Claude
    /// Code's own five, or the command's start-up decides there is nothing to
    /// do and the launch is wasted.
    func testItOnlyRunsInsideTheMargin() {
        var policy = TokenRenewalPolicy()
        func shouldRenew(_ remaining: TimeInterval) -> Bool {
            policy.shouldAttempt(expiry: inSeconds(remaining), now: now,
                                 margin: 4 * 60, cooldown: 600)
        }
        XCTAssertFalse(shouldRenew(8 * 60), "eight minutes out, nothing to do")
        XCTAssertFalse(shouldRenew(5 * 60), "five is still outside our margin")
        XCTAssertTrue(shouldRenew(3 * 60))
        XCTAssertTrue(shouldRenew(-60), "already expired counts too")
    }

    /// Never on a guess: no reading yet means no idea how long is left.
    func testItNeverRunsWithoutAnExpiry() {
        var policy = TokenRenewalPolicy()
        XCTAssertFalse(policy.shouldAttempt(expiry: nil, now: now,
                                            margin: 4 * 60, cooldown: 600))
    }

    /// The policy permits another try only after cooldown and resets its
    /// budget when a different expiry is observed.
    func testThePolicyBoundsRetriesPerExpiry() {
        let expiry = inSeconds(60)
        var policy = TokenRenewalPolicy()
        for attempt in 0..<TokenRenewalPolicy.maxAttempts {
            let instant = now.addingTimeInterval(Double(attempt) * 600)
            XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: instant,
                                               margin: 4 * 60, cooldown: 600))
            policy.recordAttempt(at: instant)
        }
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: now.addingTimeInterval(1800),
                                            margin: 4 * 60, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: inSeconds(120),
                                           now: now.addingTimeInterval(1800),
                                           margin: 4 * 60, cooldown: 600))
    }

    func testTheCooldownHoldsOffASecondLaunch() {
        var policy = TokenRenewalPolicy()
        let expiry = inSeconds(60)
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: now,
                                           margin: 4 * 60, cooldown: 600))
        policy.recordAttempt(at: now)
        XCTAssertFalse(policy.shouldAttempt(expiry: expiry, now: inSeconds(599),
                                            margin: 4 * 60, cooldown: 600))
        XCTAssertTrue(policy.shouldAttempt(expiry: expiry, now: inSeconds(600),
                                           margin: 4 * 60, cooldown: 600))
    }

    // MARK: - Running it

    private final class Spy: @unchecked Sendable {
        var launches = 0
        var pidWhileRunning: Int32??
        var exitStatus: Int32? = 1
        var throwsOnLaunch = false
    }

    private struct Boom: Error {}

    private func refresher(
        spy: Spy,
        before: Date?,
        after: Date?,
        cli: URL? = URL(fileURLWithPath: "/opt/homebrew/bin/claude"),
        // A hook for tests that need to control *when* the expiry read
        // resolves — the concurrency test below, specifically — without every
        // other test having to know that hook exists.
        wrapExpiry: (@escaping @MainActor () async -> Date?)
            -> (@MainActor () async -> Date?) = { $0 }
    ) -> ClaudeTokenRefresher {
        var made: ClaudeTokenRefresher?
        let refresher = ClaudeTokenRefresher(
            expiry: wrapExpiry({ before }),
            reload: { after },
            cli: cli,
            launcher: { _, _ in
                spy.launches += 1
                if spy.throwsOnLaunch { throw Boom() }
                return (4242, {
                    // Read where the session monitor would read it: while the
                    // command is alive, which is the only moment it matters.
                    await MainActor.run { spy.pidWhileRunning = made?.launchedPID }
                    return spy.exitStatus
                })
            }
        )
        made = refresher
        return refresher
    }

    func testItRenewsWhenTheExpiryMoves() async {
        let spy = Spy()
        let before = inSeconds(60)
        let after = inSeconds(8 * 3600)
        let refresher = refresher(spy: spy, before: before, after: after)

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 1)
        XCTAssertEqual(refresher.outcome, .refreshed(until: after))
    }

    /// Judged on the outcome, never on the exit status. Refusing an empty
    /// prompt *is* a non-zero exit, and it is also a successful renewal — so
    /// reading success off the status code would get it exactly backwards.
    func testANonZeroExitIsStillASuccessfulRenewal() async {
        let spy = Spy()
        spy.exitStatus = 1
        let after = inSeconds(8 * 3600)
        let refresher = refresher(spy: spy, before: inSeconds(60), after: after)

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(refresher.outcome, .refreshed(until: after))
    }

    /// The compatibility guard: if a future release stops renewing at start-up,
    /// the expiry simply does not move, and this has to say so rather than
    /// carry on believing it worked.
    func testAnUnmovedExpiryIsAFailure() async {
        let spy = Spy()
        let stuck = inSeconds(60)
        let refresher = refresher(spy: spy, before: stuck, after: stuck)

        await refresher.considerRenewing(now: now)

        guard case .failed(let message) = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
        XCTAssertTrue(message.contains("renew"), "the message has to say what to do: \(message)")
    }

    /// A failed after-check gets two more tries, never a fourth. A later
    /// expiry change gets a fresh budget even without recreating the refresher.
    func testFailureRetriesThreeTimesThenNewExpiryResetsBudget() async {
        let spy = Spy()
        var current = inSeconds(60)
        let refresher = ClaudeTokenRefresher(
            expiry: { current }, reload: { current },
            cli: URL(fileURLWithPath: "/opt/homebrew/bin/claude"),
            launcher: { _, _ in spy.launches += 1; return (4242, { 1 }) }
        )

        await refresher.considerRenewing(now: now)
        await refresher.considerRenewing(now: inSeconds(600))
        await refresher.considerRenewing(now: inSeconds(1200))
        await refresher.considerRenewing(now: inSeconds(1800))
        XCTAssertEqual(spy.launches, 3)

        current = inSeconds(120)
        await refresher.considerRenewing(now: inSeconds(1800))
        XCTAssertEqual(spy.launches, 4)
    }

    func testFirstFailureThenCooldownThenSuccessDoesNotRepeatOnStaleExpiry() async {
        let spy = Spy()
        let stuck = inSeconds(60)
        let after = inSeconds(8 * 3600)
        var afterCheck = stuck
        let refresher = ClaudeTokenRefresher(
            expiry: { stuck }, reload: { afterCheck },
            cli: URL(fileURLWithPath: "/opt/homebrew/bin/claude"),
            launcher: { _, _ in
                spy.launches += 1
                if spy.launches == 2 { afterCheck = after }
                return (4242, { 1 })
            }
        )

        await refresher.considerRenewing(now: now)
        await refresher.considerRenewing(now: inSeconds(599))
        XCTAssertEqual(spy.launches, 1)
        await refresher.considerRenewing(now: inSeconds(600))
        XCTAssertEqual(refresher.outcome, .refreshed(until: after))
        await refresher.considerRenewing(now: inSeconds(1200))
        XCTAssertEqual(spy.launches, 2)
    }

    func testACommandThatWillNotStartIsReported() async {
        let spy = Spy()
        spy.throwsOnLaunch = true
        let refresher = refresher(spy: spy, before: inSeconds(60), after: inSeconds(8 * 3600))

        await refresher.considerRenewing(now: now)

        guard case .failed = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
    }

    func testWithNoCommandInstalledItSaysSoAndLaunchesNothing() async {
        let spy = Spy()
        let refresher = refresher(spy: spy, before: inSeconds(60),
                                  after: inSeconds(8 * 3600), cli: nil)

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 0)
        guard case .failed(let message) = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
        XCTAssertTrue(message.contains("isn't installed"), message)
    }

    /// The pid has to be readable *while* the command runs — that is the whole
    /// window in which the session monitor could otherwise pick it up — and
    /// gone once it has finished.
    func testThePidIsVisibleWhileItRunsAndClearedAfter() async {
        let spy = Spy()
        let refresher = refresher(spy: spy, before: inSeconds(60), after: inSeconds(8 * 3600))

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.pidWhileRunning, 4242)
        XCTAssertNil(refresher.launchedPID)
    }

    func testItDoesNothingWhileTheTokenIsHealthy() async {
        let spy = Spy()
        let refresher = refresher(spy: spy, before: inSeconds(3 * 3600),
                                  after: inSeconds(8 * 3600))

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 0)
        XCTAssertEqual(refresher.outcome, .idle)
    }

    func testStartAndStopAreIdempotent() {
        let refresher = refresher(spy: Spy(), before: nil, after: nil)
        defer { refresher.stop() }

        refresher.start()
        let first = refresher.timerForTesting
        XCTAssertNotNil(first)
        refresher.start()
        XCTAssertTrue(first === refresher.timerForTesting)

        refresher.stop()
        refresher.stop()
        XCTAssertNil(refresher.timerForTesting)

        refresher.start()
        XCTAssertNotNil(refresher.timerForTesting)
        XCTAssertFalse(first === refresher.timerForTesting)
    }

    // MARK: - No concurrent launches

    /// `isRunning` has to be claimed *before* `considerRenewing`'s only
    /// `await`, not after it — otherwise there is a window, while that first
    /// read is in flight, where a second overlapping call would find nothing
    /// claimed yet and go on to launch its own process too.
    ///
    /// Worth being exact about what this is testing, because two *other*
    /// mechanisms can look like they cover this from the outside: the policy's
    /// attempt count and cooldown can also block a second call because the
    /// MainActor is serial — whichever call resumes first runs its whole
    /// synchronous stretch, writing both of those, before the second call ever
    /// reaches its own check. That is real, and worth having, but it is
    /// incidental: it depends on the two calls agreeing on roughly the same
    /// `now` and the same token, which any two genuinely concurrent ticks would,
    /// but which is not a guarantee `considerRenewing` itself makes. `isRunning`
    /// is the one thing that is supposed to make "at most one launch" true by
    /// construction, not by coincidence — so it is what gets tested directly:
    /// claimed synchronously, before anything is awaited, full stop.
    func testIsRunningIsClaimedBeforeTheFirstAwait() async {
        let spy = Spy()
        let gate = Gate()
        let refresher = refresher(
            spy: spy, before: inSeconds(60), after: inSeconds(8 * 3600),
            wrapExpiry: { original in { await gate.wait(); return await original() } }
        )

        let task = Task { await refresher.considerRenewing(now: now) }
        // Long enough for the call to reach the gate; the gate itself proves
        // it got there, since nothing after it can run without passing through.
        try? await Task.sleep(nanoseconds: 100_000_000)

        XCTAssertTrue(refresher.isRunningForTesting,
                      "the launch must be claimed before waiting on anything, "
                      + "or a second overlapping call would see nothing claimed yet")

        await gate.open()
        _ = await task.value
        XCTAssertEqual(spy.launches, 1)
    }

    /// The behavioural guarantee this all adds up to: two ticks racing over the
    /// *same* token never start two processes. This is the outside view — it
    /// does not care which of the three mechanisms above is the one holding,
    /// only that the observable result is right.
    func testTwoOverlappingTicksForTheSameTokenLaunchAtMostOnce() async {
        let spy = Spy()
        let gate = Gate()
        let refresher = refresher(
            spy: spy, before: inSeconds(60), after: inSeconds(8 * 3600),
            wrapExpiry: { original in { await gate.wait(); return await original() } }
        )

        async let first: Void = refresher.considerRenewing(now: now)
        async let second: Void = refresher.considerRenewing(now: now)
        try? await Task.sleep(nanoseconds: 100_000_000)
        await gate.open()
        _ = await (first, second)

        XCTAssertEqual(spy.launches, 1, "two overlapping ticks must start at most one process")
    }

    /// A held-open gate so a test can force two calls to overlap deterministically
    /// rather than hoping a race happens to line up.
    private actor Gate {
        private var opened = false
        private var waiters: [CheckedContinuation<Void, Never>] = []

        func wait() async {
            if opened { return }
            await withCheckedContinuation { waiters.append($0) }
        }

        func open() {
            opened = true
            waiters.forEach { $0.resume() }
            waiters = []
        }
    }

    // MARK: - The real subprocess

    /// The initializer's real launcher sends a named profile's exact config
    /// directory to the child. This executable only records its environment;
    /// it never invokes Claude Code or reads a credential.
    func testNamedProfileDirectoryReachesRealProcess() async throws {
        let (root, executable, output) = try makeEnvironmentProbe()
        defer { try? FileManager.default.removeItem(at: root) }
        let profile = ClaudeProfile(slug: "work",
                                    configDirectory: root.appendingPathComponent(".claude-work"))
        let refresher = ClaudeTokenRefresher(
            expiry: { self.inSeconds(60) },
            reload: { self.inSeconds(8 * 3600) },
            cli: executable,
            profile: profile
        )

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(refresher.outcome, .refreshed(until: inSeconds(8 * 3600)))
        XCTAssertEqual(try String(contentsOf: output, encoding: .utf8),
                       profile.configDirectory.path)
    }

    func testNamedProfileKeepsInjectedLauncherSignature() async {
        let spy = Spy()
        let profile = ClaudeProfile(slug: "work",
                                    configDirectory: URL(fileURLWithPath: "/tmp/.claude-work"))
        let refresher = ClaudeTokenRefresher(
            expiry: { self.inSeconds(60) },
            reload: { self.inSeconds(8 * 3600) },
            cli: URL(fileURLWithPath: "/fake/claude"),
            profile: profile,
            launcher: { cli, timeout in
                XCTAssertEqual(cli.path, "/fake/claude")
                XCTAssertEqual(timeout, 30)
                spy.launches += 1
                return (4242, { 1 })
            }
        )

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 1)
        XCTAssertEqual(refresher.outcome, .refreshed(until: inSeconds(8 * 3600)))
    }

    /// A GUI process could inherit another profile's override. The default
    /// profile must remove it before launching its own process.
    func testDefaultProfileDoesNotInheritAnotherConfigDirectory() async throws {
        let (root, executable, output) = try makeEnvironmentProbe()
        defer { try? FileManager.default.removeItem(at: root) }
        let otherDirectory = root.appendingPathComponent(".claude-other").path

        let (_, exit) = try ClaudeTokenRefresher.run(
            executable, timeout: 2, profile: .default(home: root),
            inheritedEnvironment: ["CLAUDE_CONFIG_DIR": otherDirectory]
        )

        let status = await exit()
        XCTAssertEqual(status, 0)
        XCTAssertEqual(try String(contentsOf: output, encoding: .utf8), "<unset>")
    }

    private func makeEnvironmentProbe() throws -> (root: URL, executable: URL, output: URL) {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("claude-renewal-probe-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let executable = root.appendingPathComponent("probe.sh")
        let output = root.appendingPathComponent("config-dir")
        let script = """
        #!/bin/sh
        printf '%s' "${CLAUDE_CONFIG_DIR-<unset>}" > "${0%/*}/config-dir"
        """
        try Data(script.utf8).write(to: executable)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
        return (root, executable, output)
    }

    /// `run(_:timeout:)` is what actually spawns `claude`, and it is the one
    /// piece of this feature every other test above deliberately swaps out —
    /// `refresher(...)` injects a fake `launcher` precisely so nothing else
    /// has to spawn anything real. That leaves the timeout-and-kill path,
    /// which is safety-critical (a wedge here would hang a *launch*, the exact
    /// shape of bug this whole feature exists downstream of), unverified by
    /// everything else. This is a real process, so the whole thing costs
    /// under a second rather than a millisecond, but that is the honest price
    /// of actually proving the kill path leaves nothing behind.
    func testATimedOutProcessIsKilledAndReaped() async throws {
        // Ignores SIGTERM, so the only way out is the SIGKILL fallback —
        // exercising the exact path this test is for, not the gentler one.
        let stubborn = try makeStubbornScript()
        defer { try? FileManager.default.removeItem(at: stubborn) }

        let start = Date()
        let (pid, exit) = try ClaudeTokenRefresher.run(stubborn, timeout: 0.2)
        let status = await exit()

        XCTAssertNil(status, "a killed process has no exit status to report")
        // Under a couple of seconds: 0.2s timeout + 0.5s SIGTERM grace + a
        // moment to confirm SIGKILL landed. A hang here would mean the reap
        // loop never noticed the process was gone.
        XCTAssertLessThan(Date().timeIntervalSince(start), 3)
        XCTAssertEqual(kill(pid, 0), -1, "the pid must not still exist once wait() has returned")
        XCTAssertEqual(errno, ESRCH, "specifically gone, not merely unreachable for some other reason")
    }

    /// A script that ignores SIGTERM so a test can force the SIGKILL branch,
    /// written fresh each time rather than checked in: it needs the executable
    /// bit, which a git checkout does not reliably preserve.
    private func makeStubbornScript() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("stubborn-\(UUID().uuidString).sh")
        try Data("#!/bin/sh\ntrap '' TERM\nsleep 30\n".utf8).write(to: url)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    /// #227: the renewal run starts no MCP servers, keeps no transcript, and
    /// never runs from the app's own working directory.
    func testTheRenewalRunIsContainedLikeTheUsageRun() {
        XCTAssertTrue(ClaudeTokenRefresher.arguments.contains("--strict-mcp-config"))
        XCTAssertTrue(ClaudeTokenRefresher.arguments.contains("--no-session-persistence"))
        XCTAssertEqual(ClaudeTokenRefresher.arguments.first, "-p")
        XCTAssertFalse(ClaudeTokenRefresher.arguments.contains("--mcp-config"),
                       "the flag is variadic and would swallow what follows it")
    }
}
