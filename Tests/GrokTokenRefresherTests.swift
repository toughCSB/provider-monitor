import XCTest
@testable import ProviderMonitor

/// Which of the `grok` binaries on a Mac is the right one.
final class GrokCLITests: XCTestCase {
    /// An empty home rather than the developer's own: left pointing at the real
    /// home this test passed or failed depending on whether the machine running
    /// it happened to have installed the Grok CLI.
    func testNothingInstalledIsNotAnError() throws {
        let home = try makeHome(executablesAt: [])

        XCTAssertNil(GrokCLI.standalone(candidates: ["~/.local/bin/grok"], home: home.path))
    }

    func testItFindsTheInstallersShim() throws {
        let home = try makeHome(executablesAt: [".local/bin/grok"])

        let found = GrokCLI.standalone(candidates: ["~/.local/bin/grok"], home: home.path)

        XCTAssertEqual(found?.path, home.appendingPathComponent(".local/bin/grok").path)
    }

    /// The shim is what a shell runs, and the managed binary underneath it is
    /// reached through it — so an install that has both resolves to the shim.
    func testTheShimOutranksTheManagedBinary() throws {
        let home = try makeHome(executablesAt: [".local/bin/grok", ".grok/bin/grok"])

        let found = GrokCLI.standalone(candidates: GrokCLI.candidates, home: home.path)

        XCTAssertEqual(found?.path, home.appendingPathComponent(".local/bin/grok").path)
    }

    /// An absolute candidate is not run through `home`, or a Homebrew install
    /// would be looked for inside the temporary directory.
    func testAnAbsoluteCandidateIgnoresTheHome() throws {
        let home = try makeHome(executablesAt: [])

        XCTAssertNil(GrokCLI.standalone(candidates: ["/opt/homebrew/bin/grok"], home: home.path))
    }

    private func makeHome(executablesAt paths: [String]) throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("GrokCLITests.\(UUID().uuidString)")
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

/// The gate, the cooldown and the failure path around renewing Grok's session.
@MainActor
final class GrokTokenRefresherTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private func inSeconds(_ s: TimeInterval) -> Date { now.addingTimeInterval(s) }

    // MARK: - The gate

    /// Only once the session has expired, and never a moment before. The command
    /// leaves a live session alone — measured on a real machine: an expiry sixty
    /// seconds out is untouched, one sixty seconds past is renewed on the spot —
    /// so asking earlier is a launch this would then have to report as a failure.
    func testItOnlyRunsAfterTheSessionHasExpired() {
        func shouldRenew(_ remaining: TimeInterval) -> Bool {
            GrokTokenRefresher.shouldRenew(
                expiry: inSeconds(remaining), now: now, margin: 0,
                attemptedFor: nil, lastAttempt: nil, cooldown: 600
            )
        }
        XCTAssertFalse(shouldRenew(6 * 3600), "hours of session left")
        XCTAssertFalse(shouldRenew(60), "a minute left is still a live session")
        XCTAssertFalse(shouldRenew(0), "the command's own gate is shut until it is past")
        XCTAssertTrue(shouldRenew(-60))
    }

    /// Never on a guess: no reading yet means no idea whether the session is live.
    func testItNeverRunsWithoutAnExpiry() {
        XCTAssertFalse(GrokTokenRefresher.shouldRenew(
            expiry: nil, now: now, margin: 0,
            attemptedFor: nil, lastAttempt: nil, cooldown: 600
        ))
    }

    /// The no-retry-loop guarantee, expressed as "one attempt per session"
    /// rather than as a timer: a launch that failed to move the expiry leaves the
    /// same value here on the next tick, so it is refused for as long as the
    /// session stays expired — not merely for a cooldown.
    func testASessionGetsOneAttemptEver() {
        let expiry = inSeconds(-60)
        XCTAssertFalse(GrokTokenRefresher.shouldRenew(
            expiry: expiry, now: now, margin: 0,
            attemptedFor: expiry, lastAttempt: nil, cooldown: 600
        ))
        // A renewed session is a different question.
        XCTAssertTrue(GrokTokenRefresher.shouldRenew(
            expiry: inSeconds(-30), now: now, margin: 0,
            attemptedFor: expiry, lastAttempt: nil, cooldown: 600
        ))
    }

    func testTheCooldownHoldsOffASecondLaunch() {
        XCTAssertFalse(GrokTokenRefresher.shouldRenew(
            expiry: inSeconds(-60), now: now, margin: 0,
            attemptedFor: nil, lastAttempt: now.addingTimeInterval(-60), cooldown: 600
        ))
        XCTAssertTrue(GrokTokenRefresher.shouldRenew(
            expiry: inSeconds(-60), now: now, margin: 0,
            attemptedFor: nil, lastAttempt: now.addingTimeInterval(-900), cooldown: 600
        ))
    }

    // MARK: - Running it

    private final class Stub: @unchecked Sendable {
        var value: Date?
        init(_ value: Date?) { self.value = value }
    }

    private final class Spy: @unchecked Sendable {
        var launches = 0
        var pidWhileRunning: Int32??
        var exitStatus: Int32? = 0
        var throwsOnLaunch = false
    }

    private struct Boom: Error {}

    /// The launcher is the one piece every other test swaps out, so nothing here
    /// spawns a real command; `onLaunch` is how a test simulates the CLI
    /// rewriting auth.json while it runs.
    private func refresher(
        stub: Stub,
        spy: Spy,
        cli: URL? = URL(fileURLWithPath: "/opt/homebrew/bin/grok"),
        onLaunch: @escaping (Stub) -> Void = { _ in }
    ) -> GrokTokenRefresher {
        var made: GrokTokenRefresher?
        let refresher = GrokTokenRefresher(
            expiry: { stub.value },
            cli: cli,
            launcher: { _, _ in
                spy.launches += 1
                if spy.throwsOnLaunch { throw Boom() }
                onLaunch(stub)
                return (4242, {
                    // Read while the command is alive, which is the only moment
                    // `launchedPID` is set.
                    await MainActor.run { spy.pidWhileRunning = made?.launchedPID }
                    return spy.exitStatus
                })
            }
        )
        made = refresher
        return refresher
    }

    func testItRenewsWhenTheExpiryMoves() async {
        let before = inSeconds(-60)
        let after = inSeconds(6 * 3600)
        let stub = Stub(before)
        let spy = Spy()
        let refresher = refresher(stub: stub, spy: spy) { $0.value = after }

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 1)
        XCTAssertEqual(spy.pidWhileRunning, 4242)
        XCTAssertEqual(refresher.outcome, .refreshed(until: after))
    }

    /// Judged on the outcome, never on the exit status: the command can exit zero
    /// and still have changed nothing, which must not read as a renewal.
    func testItReportsFailureWhenTheExpiryDoesNotMove() async {
        let stub = Stub(inSeconds(-60))
        let spy = Spy()
        let refresher = refresher(stub: stub, spy: spy)   // the CLI wrote nothing

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 1)
        guard case .failed = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
    }

    /// A live session is not launched at all — the whole point of the zero
    /// margin, and the reason a renewal never cuts a working reading short.
    func testALiveSessionIsNeverLaunched() async {
        let stub = Stub(inSeconds(60))
        let spy = Spy()
        let refresher = refresher(stub: stub, spy: spy)

        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 0)
        XCTAssertEqual(refresher.outcome, .idle)
    }

    func testACommandThatWillNotStartIsReported() async {
        let stub = Stub(inSeconds(-60))
        let spy = Spy()
        spy.throwsOnLaunch = true
        let refresher = refresher(stub: stub, spy: spy)

        await refresher.considerRenewing(now: now)

        guard case .failed = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
    }

    /// Nothing installed to renew with: the miss is reported rather than guessed
    /// away, and reported once — the session's expiry has not moved, so the next
    /// tick is refused by `attemptedFor` rather than nagging every minute.
    func testAMissingCommandFailsOnce() async {
        let stub = Stub(inSeconds(-60))
        let spy = Spy()
        let refresher = refresher(stub: stub, spy: spy, cli: nil)

        await refresher.considerRenewing(now: now)
        await refresher.considerRenewing(now: now.addingTimeInterval(3600))

        XCTAssertEqual(spy.launches, 0)
        guard case .failed = refresher.outcome else {
            return XCTFail("expected a failure, got \(refresher.outcome)")
        }
    }

    /// A second tick must not overlap the first: `isRunning` is claimed before
    /// the first `await`, so an in-flight renewal is never launched twice.
    func testASecondTickWhileRunningIsRefused() async {
        let stub = Stub(inSeconds(-60))
        let spy = Spy()
        let gate = AsyncGate()
        let refresher = GrokTokenRefresher(
            expiry: { stub.value },
            cli: URL(fileURLWithPath: "/opt/homebrew/bin/grok"),
            launcher: { _, _ in
                spy.launches += 1
                stub.value = self.inSeconds(6 * 3600)
                return (4242, { await gate.wait(); return 0 })
            }
        )

        let first = Task { await refresher.considerRenewing(now: now) }
        // Let the first call reach its await before the second one asks.
        while !refresher.isRunningForTesting { await Task.yield() }
        await refresher.considerRenewing(now: now)

        XCTAssertEqual(spy.launches, 1)
        gate.open()
        await first.value
        XCTAssertEqual(spy.launches, 1)
    }

    func testTheRenewalRunAsksTheCliToReauthenticateHeadlessly() {
        XCTAssertEqual(GrokTokenRefresher.arguments, ["agent", "--reauth", "stdio"])
    }

    // MARK: - The real subprocess

    /// `run(_:timeout:)` is the one piece every test above deliberately swaps
    /// out, which leaves the timeout-and-kill path — a wedge here would hang a
    /// launch, the exact shape of bug this feature exists downstream of —
    /// unverified by everything else. This is a real process, so it costs about
    /// a second rather than a millisecond.
    func testATimedOutProcessIsKilledAndReaped() async throws {
        let stubborn = try makeStubbornScript()
        defer { try? FileManager.default.removeItem(at: stubborn) }

        let start = Date()
        let (pid, exit) = try GrokTokenRefresher.run(stubborn, timeout: 0.2)
        let status = await exit()

        XCTAssertNil(status, "a killed process has no exit status to report")
        XCTAssertLessThan(Date().timeIntervalSince(start), 3)
        XCTAssertEqual(kill(pid, 0), -1, "the pid must not still exist once wait() has returned")
        XCTAssertEqual(errno, ESRCH, "specifically gone, not merely unreachable for some other reason")
    }

    /// A script that ignores SIGTERM, so the SIGKILL fallback is the only way
    /// out — the path this test is for, not the gentler one. Written fresh each
    /// time rather than checked in: it needs the executable bit, which a git
    /// checkout does not reliably preserve.
    private func makeStubbornScript() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("grok-stubborn-\(UUID().uuidString).sh")
        try Data("#!/bin/sh\ntrap '' TERM\nsleep 30\n".utf8).write(to: url)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }
}

/// A one-shot latch the launcher's `exit` closure waits on, so a test can hold a
/// renewal "in flight" without sleeping.
private actor AsyncGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private var isOpen = false

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { continuation = $0 }
    }

    nonisolated func open() {
        Task { await self.release() }
    }

    private func release() {
        isOpen = true
        continuation?.resume()
        continuation = nil
    }
}
