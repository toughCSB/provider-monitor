import XCTest
@testable import ProviderMonitor

/// Which of a provider's windows the ring draws, once the user has asked for a
/// cadence rather than left it to the provider.
///
/// The risk this covers is not cosmetic: a window matched by the wrong rule is a
/// ring confidently reporting a different limit than the one it was asked for,
/// in the same position in the stack, with nothing on screen to say so.
final class HeadlineWindowTests: XCTestCase {
    /// Claude's shape: a five-hour session and three seven-day windows, of which
    /// only one is the one the user means by "weekly".
    private let claude = [
        LimitWindow(id: "session", label: "Current session", usedFraction: 0.2,
                    duration: 5 * 3600),
        LimitWindow(id: "weekly_all", label: "All models", usedFraction: 0.6,
                    duration: 7 * 86400),
        LimitWindow(id: "weekly_opus", label: "Opus", usedFraction: 0.3,
                    duration: 7 * 86400)
    ]

    private func resolve(_ windows: [LimitWindow], _ cadence: RingCadence,
                         declared: String? = "session", weeklyID: String? = "weekly_all") -> String? {
        HeadlineWindow.resolve(windows: windows, declared: declared,
                               weeklyID: weeklyID, cadence: cadence)
    }

    // MARK: - Automatic

    func testAutomaticLeavesTheProviderItsOwnChoice() {
        XCTAssertEqual(resolve(claude, .automatic), "session")
    }

    /// A provider with no declaration at all — a local runtime, a snapshot from
    /// an older archive — is left exactly as it was rather than being given one.
    func testAutomaticInventsNothingWhenThereIsNoDeclaration() {
        XCTAssertNil(resolve(claude, .automatic, declared: nil))
    }

    // MARK: - A cadence the windows answer

    func testAFiveHourWindowsIsFoundByItsOwnLength() {
        XCTAssertEqual(resolve(claude, .fiveHour), "session")
    }

    /// The three weeklies are indistinguishable by duration, so the choice falls
    /// to the window the provider named as the one its second ring draws. Picking
    /// "Opus" because it sorts first would be this rule deciding something
    /// already decided.
    func testAWeeklyWindowIsFoundAmongThreeWeekliesByTheProvidersOwnName() {
        XCTAssertEqual(resolve(claude, .weekly), "weekly_all")
    }

    // MARK: - A cadence the windows do not answer

    /// Claude meters nothing monthly. The ring keeps its subject rather than
    /// being handed a weekly window wearing the monthly label.
    func testAMonthlyChoiceLeavesAProviderWithNoMonthAlone() {
        XCTAssertEqual(resolve(claude, .monthly), "session")
    }

    /// Two windows that both answer the cadence and no name to break the tie:
    /// the honest answer is the provider's own, not a coin toss.
    func testAnAmbiguousCadenceIsNotGuessedAt() {
        let windows = [
            LimitWindow(id: "a-weekly", label: "A", usedFraction: 0.1, duration: 7 * 86400),
            LimitWindow(id: "b-weekly", label: "B", usedFraction: 0.2, duration: 7 * 86400)
        ]
        XCTAssertEqual(resolve(windows, .weekly, declared: "a-weekly", weeklyID: nil), "a-weekly")
    }

    /// A window that dropped out of the response cannot be drawn. Nothing else
    /// stands in for it.
    func testAWindowThatIsNotThereIsNotReplacedByAnother() {
        let onlySession = [claude[0]]
        XCTAssertEqual(resolve(onlySession, .weekly), "session")
    }

    // MARK: - The word rule

    /// Providers that publish no duration at all are matched on the words they
    /// use themselves.
    func testAWindowWithNoDurationIsMatchedOnItsOwnWords() {
        let windows = [
            LimitWindow(id: "rolling", label: "Rolling", usedFraction: 0.1),
            LimitWindow(id: "weekly", label: "Weekly", usedFraction: 0.2)
        ]
        XCTAssertEqual(resolve(windows, .fiveHour, declared: "rolling", weeklyID: nil), "rolling")
        XCTAssertEqual(resolve(windows, .weekly, declared: "rolling", weeklyID: nil), "weekly")
    }

    /// A window that states its length wins over the words, so a provider that
    /// calls its monthly plan "combined weekly allowance" is not read as weekly.
    func testADurationBeatsTheWording() {
        let windows = [
            LimitWindow(id: "primary", label: "Primary", usedFraction: 0.1, duration: 5 * 3600),
            LimitWindow(id: "monthly", label: "Monthly weekly rollup", usedFraction: 0.2,
                        duration: 30 * 86400)
        ]
        XCTAssertEqual(resolve(windows, .weekly, declared: "primary", weeklyID: nil), "primary")
        XCTAssertEqual(resolve(windows, .monthly, declared: "primary", weeklyID: nil), "monthly")
    }

    // MARK: - What the settings row may offer

    func testOnlyTheCadencesTheWindowsCanAnswerAreOffered() {
        XCTAssertEqual(HeadlineWindow.cadences(in: claude, weeklyID: "weekly_all"),
                       [.fiveHour, .weekly])
    }

    /// The same matcher answers the menu and the ring, so a cadence the menu
    /// offers is one the ring can honour. Asked with no declaration to fall back
    /// on, the matcher answers nil exactly when it can find nothing.
    func testEverythingOfferedCanBeAnswered() {
        for cadence in HeadlineWindow.cadences(in: claude, weeklyID: "weekly_all") {
            XCTAssertNotNil(
                HeadlineWindow.resolve(windows: claude, declared: nil,
                                       weeklyID: "weekly_all", cadence: cadence),
                "\(cadence.rawValue) is offered by the menu but cannot be answered"
            )
        }
    }

    /// `automatic` is not one of the windows — it is the absence of a request,
    /// and the settings row adds it to whatever this returns.
    func testAutomaticIsNeverInTheOfferedList() {
        XCTAssertFalse(HeadlineWindow.cadences(in: claude, weeklyID: "weekly_all").contains(.automatic))
    }

    /// One window is not a choice, which is what the settings row keys off — it
    /// asks for this list and shows no menu when there is nothing in it.
    func testAProviderWithOneWindowOffersNothingToChoose() {
        XCTAssertEqual(HeadlineWindow.cadences(in: [claude[0]], weeklyID: nil), [.fiveHour])
    }

    /// Antigravity reports one lane per model family, so both of its cadences
    /// arrive ambiguous and neither can be named without the provider saying
    /// which weekly lane it means. Asked without that name the menu comes up
    /// empty — on the one provider whose windows need the most explaining.
    func testTheProvidersOwnWeeklyNameIsWhatMakesWeeklyAvailable() {
        let lanes = [
            LimitWindow(id: "gemini-5h", label: "5-hour Limit", usedFraction: 0.1,
                        duration: 5 * 3600),
            LimitWindow(id: "gemini-weekly", label: "Weekly Limit", usedFraction: 0.2,
                        duration: 7 * 86400),
            LimitWindow(id: "3p-5h", label: "5-hour Limit", usedFraction: 0.1,
                        duration: 5 * 3600),
            LimitWindow(id: "3p-weekly", label: "Weekly Limit", usedFraction: 0.3,
                        duration: 7 * 86400)
        ]

        XCTAssertEqual(HeadlineWindow.cadences(in: lanes, weeklyID: "gemini-weekly"), [.weekly])
        XCTAssertEqual(HeadlineWindow.cadences(in: lanes, weeklyID: nil), [])
    }

    /// And the menu's promise is kept: what it offers, `resolve` answers — with
    /// the same name passed, which is what the ring is given.
    func testWhatTheMenuOffersTheRingHonours() {
        let lanes = [
            LimitWindow(id: "gemini-5h", label: "5-hour Limit", usedFraction: 0.1,
                        duration: 5 * 3600),
            LimitWindow(id: "gemini-weekly", label: "Weekly Limit", usedFraction: 0.2,
                        duration: 7 * 86400),
            LimitWindow(id: "3p-5h", label: "5-hour Limit", usedFraction: 0.1,
                        duration: 5 * 3600),
            LimitWindow(id: "3p-weekly", label: "Weekly Limit", usedFraction: 0.3,
                        duration: 7 * 86400)
        ]
        for cadence in HeadlineWindow.cadences(in: lanes, weeklyID: "gemini-weekly") {
            XCTAssertEqual(HeadlineWindow.resolve(windows: lanes, declared: nil,
                                                 weeklyID: "gemini-weekly", cadence: cadence),
                           "gemini-weekly")
        }
    }
}

/// The choice as it is stored, and as it reaches the ring.
@MainActor
final class RingCadencePreferenceTests: XCTestCase {
    private func defaults() -> (UserDefaults, String) {
        let name = "RingCadenceTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return (defaults, name)
    }

    func testAProviderThatWasNeverAskedIsAutomatic() {
        let (fresh, _) = defaults()
        XCTAssertEqual(Preferences(defaults: fresh).ringCadence(for: "claude"), .automatic)
    }

    func testAChoiceSurvivesARelaunch() {
        let (fresh, name) = defaults()
        Preferences(defaults: fresh).setRingCadence(.weekly, for: "claude")

        let reloaded = Preferences(defaults: UserDefaults(suiteName: name)!)
        XCTAssertEqual(reloaded.ringCadence(for: "claude"), .weekly)
        // And only for the provider it was made for.
        XCTAssertEqual(reloaded.ringCadence(for: "codex"), .automatic)
    }

    func testGoingBackToAutomaticForgetsTheChoice() {
        let (fresh, name) = defaults()
        let preferences = Preferences(defaults: fresh)
        preferences.setRingCadence(.monthly, for: "claude")
        preferences.setRingCadence(.automatic, for: "claude")

        XCTAssertEqual(Preferences(defaults: UserDefaults(suiteName: name)!)
            .ringCadence(for: "claude"), .automatic)
    }

    /// Antigravity had this setting before any other provider did. An install
    /// that chose "weekly" there keeps it rather than finding the ring back on
    /// automatic after an update.
    func testAntigravitysOldSettingIsCarriedOver() {
        let (fresh, name) = defaults()
        fresh.set("weekly", forKey: "antigravityHeadlineLimit")

        let reloaded = Preferences(defaults: UserDefaults(suiteName: name)!)
        XCTAssertEqual(reloaded.ringCadence(for: "gemini"), .weekly)
    }

    func testAnOldSettingOfAutomaticCarriesNothingOver() {
        let (fresh, name) = defaults()
        fresh.set("automatic", forKey: "antigravityHeadlineLimit")

        let reloaded = Preferences(defaults: UserDefaults(suiteName: name)!)
        XCTAssertTrue(reloaded.providerRingCadence.isEmpty)
    }
}

/// And the same choice, arriving at the store.
@MainActor
final class RingCadenceStoreTests: XCTestCase {
    /// Claude's shape, declared as the provider declares it.
    private final class Stub: UsageProvider, @unchecked Sendable {
        let id = "claude"
        let displayName = "Claude"
        let glyph = ProviderGlyph.claude

        func fetchSnapshot() async throws -> ProviderSnapshot {
            ProviderSnapshot(
                id: id, displayName: displayName, glyph: glyph,
                fidelity: .official, status: .ok,
                windows: [
                    LimitWindow(id: "session", label: "Current session", usedFraction: 0.2,
                                resetsAt: Date().addingTimeInterval(2 * 3600), duration: 5 * 3600),
                    LimitWindow(id: "weekly_all", label: "All models", usedFraction: 0.6,
                                resetsAt: Date().addingTimeInterval(5 * 86400), duration: 7 * 86400)
                ],
                headlineID: "session", weeklyID: "weekly_all")
        }
    }

    private func defaults() -> UserDefaults {
        let name = "RingCadenceStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    private func read() async -> (UsageStore, ProviderSnapshot) {
        let store = UsageStore(providers: [Stub()], archive: UsageArchive(defaults: defaults()))
        await store.refresh()
        return (store, store.snapshots[0])
    }

    func testAReadingOpensOnTheProvidersOwnWindow() async {
        let (_, snapshot) = await read()
        XCTAssertEqual(snapshot.headlineID, "session")
        XCTAssertEqual(snapshot.headline?.usedFraction ?? 0, 0.2, accuracy: 0.0001)
    }

    func testTheChoiceMovesTheRingWithoutRefetchingAnything() async {
        let (store, _) = await read()
        store.ringCadence = ["claude": .weekly]

        XCTAssertEqual(store.snapshots[0].headlineID, "weekly_all")
        XCTAssertEqual(store.snapshots[0].headline?.usedFraction ?? 0, 0.6, accuracy: 0.0001)
        // The numbers the ring is drawing are the ones already in hand; only the
        // window it means has moved.
        XCTAssertEqual(store.snapshots[0].windows.count, 2)
    }

    /// The whole reason the provider's declaration is kept apart from the drawn
    /// window: without it, "Automatic" would freeze whichever window the
    /// override landed on and never go back to the session.
    func testGoingBackToAutomaticReturnsToTheSession() async {
        let (store, _) = await read()
        store.ringCadence = ["claude": .weekly]
        store.ringCadence = [:]

        XCTAssertEqual(store.snapshots[0].headlineID, "session")
        XCTAssertEqual(store.snapshots[0].declaredHeadlineID, "session")
    }

    func testACadenceTheReadingCannotAnswerChangesNothing() async {
        let (store, _) = await read()
        store.ringCadence = ["claude": .monthly]

        XCTAssertEqual(store.snapshots[0].headlineID, "session")
    }

    /// The notch draws the cells, not the snapshots, so the change has to reach
    /// the projection the panel is actually given.
    func testTheChoiceReachesWhatTheNotchDraws() async {
        let (store, _) = await read()
        store.ringCadence = ["claude": .weekly]

        XCTAssertEqual(store.notchSnapshots.first?.headlineID, "weekly_all")
        // The window is the one that changed hands, so the figure under the
        // ring is the weekly one — reported as what is *left* of it, which is
        // the basis the store answers with by default.
        XCTAssertEqual(store.notchSnapshots.first?.headlineText, "40%")
        store.percentBasis = .used
        XCTAssertEqual(store.notchSnapshots.first?.headlineText, "60%")
    }

    /// The card asks the reading the same two questions the ring answers from:
    /// what it counts down to, and what its switch may offer. Stamped on the
    /// snapshot rather than read from Preferences per view, so the card, the
    /// badge and the ring cannot name different windows.
    func testTheCardLeadsWithTheWindowTheRingReads() async {
        let (store, snapshot) = await read()
        XCTAssertEqual(snapshot.ringCadence, .automatic)
        // The session is the ring's window *and* the short one, so the summary
        // has one column rather than the same countdown twice.
        XCTAssertEqual(snapshot.resetSummaryIDs, ["session"])
        XCTAssertEqual(snapshot.switchableCadences, [.automatic, .fiveHour, .weekly])

        store.ringCadence = ["claude": .weekly]
        let weekly = store.snapshots[0]
        XCTAssertEqual(weekly.ringCadence, .weekly)
        // And the summary moves with the ring: the week leads, and the session
        // is now the short window *beside* it.
        XCTAssertEqual(weekly.resetSummaryIDs, ["weekly_all", "session"])
    }
}

/// Antigravity answers the request itself, because its lanes are split by model
/// family as well as by cadence and the shared rule cannot see the family.
final class AntigravityCadenceTests: XCTestCase {
    /// Fix the selected family explicitly; an unknown family must no longer
    /// borrow another model's limits just to test cadence selection.
    private let lanes = [
        LimitWindow(id: "gemini-hourly", label: "5 hour", usedFraction: 0.3, duration: 5 * 3600),
        LimitWindow(id: "gemini-weekly", label: "Weekly", usedFraction: 0.5, duration: 7 * 86400)
    ]

    private let provider = AntigravityProvider(headlineModel: { .gemini })

    func testAutomaticKeepsTheMostConstrainedLane() {
        XCTAssertEqual(provider.resolveHeadlineID(for: lanes), "gemini-weekly")
    }

    func testAChosenCadenceMovesTheRingToThatLane() {
        XCTAssertEqual(provider.resolveHeadlineID(for: lanes, cadence: .fiveHour), "gemini-hourly")
        XCTAssertEqual(provider.resolveHeadlineID(for: lanes, cadence: .weekly), "gemini-weekly")
    }

    /// Nothing Antigravity reports is metered by the month, so the ring stays on
    /// the provider's own choice rather than being pointed at a lane that is not
    /// one.
    func testAMonthlyChoiceLeavesTheRingWhereAutomaticLeftIt() {
        XCTAssertEqual(provider.resolveHeadlineID(for: lanes, cadence: .monthly),
                       provider.resolveHeadlineID(for: lanes))
    }

    /// What the store asks, and what the settings row offers: the two have to
    /// agree, so a lane the menu names is one the ring can be pointed at.
    func testTheProviderAnswersTheStoresQuestion() {
        XCTAssertEqual(provider.resolveRingWindow(in: lanes, cadence: .weekly), "gemini-weekly")
        XCTAssertNil(provider.resolveRingWindow(in: [], cadence: .weekly))
    }

    /// What the row offers, asked of the provider rather than worked out from
    /// the windows: each cadence arrives as two lanes, one per model family, and
    /// the shared rule refuses to name either. The menu would then offer the
    /// weekly limit alone — less than this provider can actually do.
    func testTheProviderNamesBothCadencesItsLanesCover() {
        XCTAssertEqual(provider.ringCadences(in: lanes), [.fiveHour, .weekly])
    }

    /// And nothing it cannot: Antigravity meters nothing by the month, so a
    /// monthly entry would be a menu item whose ring never moved.
    func testAMonthlyLaneIsNotOffered() {
        XCTAssertFalse((provider.ringCadences(in: lanes) ?? []).contains(.monthly))
    }
}

/// What the card leads with, and what the ring wears, for readings whose
/// windows are not all equally talkative.
///
/// Claude is the live case: it reports its session window whenever it has one —
/// at 0% used and with no reset time until the five hours actually start — and
/// its week beside it. The summary used to drop the session entirely for having
/// nothing to count down to, which left a ring reading a week with one figure
/// where the design has two columns.
@MainActor
final class ResetSummaryPairTests: XCTestCase {
    private final class Stub: UsageProvider, @unchecked Sendable {
        let id: String
        let displayName: String
        let glyph: ProviderGlyph
        private let windows: [LimitWindow]
        private let headline: String
        private let weekly: String?

        init(id: String, glyph: ProviderGlyph, windows: [LimitWindow],
             headline: String, weekly: String? = nil) {
            self.id = id
            self.displayName = id.capitalized
            self.glyph = glyph
            self.windows = windows
            self.headline = headline
            self.weekly = weekly
        }

        func fetchSnapshot() async throws -> ProviderSnapshot {
            ProviderSnapshot(id: id, displayName: displayName, glyph: glyph,
                             fidelity: .official, status: .ok, windows: windows,
                             headlineID: headline, weeklyID: weekly)
        }
    }

    private func defaults() -> UserDefaults {
        let name = "ResetSummaryPairTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    private func read(_ provider: UsageProvider) async -> (UsageStore, ProviderSnapshot) {
        let store = UsageStore(providers: [provider], archive: UsageArchive(defaults: defaults()))
        await store.refresh()
        return (store, store.snapshots[0])
    }

    /// Claude's live shape: a session that has reported a percentage but not
    /// yet a reset, and a week that has both.
    private var claude: Stub {
        Stub(id: "claude", glyph: .claude, windows: [
            LimitWindow(id: "session", label: "Current session", usedFraction: 0,
                        duration: 5 * 3600),
            LimitWindow(id: "weekly_all", label: "All models", usedFraction: 0.3,
                        resetsAt: Date().addingTimeInterval(5 * 86400), duration: 7 * 86400)
        ], headline: "session", weekly: "weekly_all")
    }

    /// The session keeps its column rather than being dropped for having no
    /// countdown of its own — and it is the column the week pushes aside.
    func testASessionWithNoResetStillHoldsItsColumnBesideTheWeek() async {
        let (store, snapshot) = await read(claude)
        // On the provider's own pick the ring reads the session, and the
        // session is also the short window: one column, with nothing to count
        // down to yet, so the card grows no block rather than a lone dash.
        XCTAssertEqual(snapshot.resetSummaryIDs, [])

        // Point the ring at the week and the session becomes the short window
        // beside it. It keeps its column — which is where a five-hour reset
        // shows up the moment Claude publishes one, instead of the summary
        // quietly becoming a single figure.
        store.ringCadence = ["claude": .weekly]
        XCTAssertEqual(store.snapshots[0].resetSummaryIDs, ["weekly_all", "session"])
    }

    /// And a provider with no reset anywhere still grows no block at all: two
    /// dashes are not a summary.
    func testNoResetAnywhereIsNoSummaryAtAll() async {
        let (_, snapshot) = await read(Stub(id: "claude", glyph: .claude, windows: [
            LimitWindow(id: "session", label: "Current session", usedFraction: 0,
                        duration: 5 * 3600)
        ], headline: "session"))
        XCTAssertTrue(snapshot.resetSummaryIDs.isEmpty)
    }

    /// The letters follow the window, not the choice: a ring left on the
    /// provider's own pick still has to say which limit its number is.
    func testTheBadgeNamesTheWindowTheRingIsReading() async {
        let (store, snapshot) = await read(claude)
        XCTAssertEqual(snapshot.ringBadge, .fiveHour)

        store.ringCadence = ["claude": .weekly]
        XCTAssertEqual(store.snapshots[0].ringBadge, .weekly)
    }

    /// A provider whose only window is a week wears W without anyone having
    /// chosen the week — the case Codex and Grok are both in.
    func testAProviderWithOnlyAWeeklyWindowWearsWItself() async {
        let (_, snapshot) = await read(Stub(id: "codex", glyph: .openai, windows: [
            LimitWindow(id: "primary", label: "Weekly limit", usedFraction: 0.54,
                        resetsAt: Date().addingTimeInterval(2 * 86400), duration: 7 * 86400)
        ], headline: "primary"))
        XCTAssertEqual(snapshot.ringBadge, .weekly)
        XCTAssertEqual(snapshot.resetSummaryIDs, ["primary"])
    }

    /// A month is not a week, and a window that says how long it lasts is read
    /// by its length rather than by whatever it is called.
    func testALengthsOwnWindowIsReadByItsLength() async {
        let (_, snapshot) = await read(Stub(id: "opencode", glyph: .opencode, windows: [
            LimitWindow(id: "rolling", label: "5-hour limit", usedFraction: 0.06,
                        resetsAt: Date().addingTimeInterval(3600), duration: 5 * 3600),
            LimitWindow(id: "monthly", label: "Monthly limit", usedFraction: 0.81,
                        resetsAt: Date().addingTimeInterval(86400), duration: 31 * 86400)
        ], headline: "rolling"))
        XCTAssertEqual(snapshot.ringBadge, .fiveHour)
    }
}
