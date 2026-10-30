import AppKit
import Combine
import os

/// Quota readings remain useful during transient failures. Clear local inventory
/// when the server cannot confirm which models are still loaded.
@MainActor
final class UsageStore: ObservableObject {
    @Published private(set) var snapshots: [ProviderSnapshot] = [] {
        didSet { updateNotchSnapshots() }
    }
    /// Settings and every display share the same ordered, visible model cells.
    @Published private(set) var notchSnapshots: [ProviderSnapshot] = []
    /// Providers with a fetch in flight, so the cell can show it happening.
    @Published private(set) var refreshing: Set<String> = []
    /// Providers whose last fetch was refused by macOS, cleared as soon as one
    /// succeeds. The settings row's only honest basis for offering to ask again.
    @Published private(set) var refusedAccess: Set<String> = []
    /// Providers whose saved login has aged out and could not be renewed.
    ///
    /// Kept beside `refusedAccess`, and for the same reason: it is a fact about
    /// the *credential*, not about the numbers on screen. An expired token
    /// leaves the last reading standing and still roughly true, so nothing in
    /// any snapshot says anything is wrong — which is exactly how a frozen
    /// reading went unnoticed for twelve hours. Reading it off the snapshot
    /// would reproduce the bug.
    @Published private(set) var needsRenewal: Set<String> = []
    /// Bumped when a provider's authentication state changes. Settings listens
    /// to this separately from usage snapshots so it can re-read account
    /// summaries without re-reading every credential on every polling pass.
    @Published private(set) var providerAccountRevision = 0

    private let providers: [UsageProvider]

    /// Provider ids plus any model cells currently on screen.
    var knownIDs: [String] {
        Array(Set(providers.map(\.id) + snapshots.map(\.id) + localModelSummaries.map(\.id)))
    }
    /// Provider IDs block fetching before credential access. Model IDs only hide
    /// their cells so disabling one model does not stop the shared runtime.
    @Published var disconnected: Set<String> = [] {
        didSet {
            guard disconnected != oldValue else { return }
            for id in disconnected.subtracting(oldValue) { cancelRefresh(providerID: id) }
            snapshots.removeAll { disconnected.contains($0.id) }
            refusedAccess.subtract(disconnected)
            // The remembered reading has to go as well. Dropping it from
            // `snapshots` alone left it in `lastGood`, which is written to the
            // archive wholesale on every fetch — so a switched-off provider was
            // remembered forever and rebuilt from the archive at the next
            // launch, ring and all.
            for id in disconnected { lastGood.removeValue(forKey: id) }
            archive.save(lastGood)
            for provider in providers where oldValue.contains(provider.id) && !disconnected.contains(provider.id) {
                publish(Self.placeholder(provider))
            }
            let changed = disconnected.symmetricDifference(oldValue)
            if providers.contains(where: { changed.contains($0.id) && $0.kind == .usage }) {
                refreshNow()
            } else if providers.contains(where: { changed.contains($0.id) && $0.kind == .localRuntime }) {
                refreshLocalRuntimes()
            }
        }
    }

    /// The order the user has put the rings in, as provider ids.
    ///
    /// Held here rather than at each consumer because there are two consumers —
    /// the notch reads `notchSnapshots`, settings reads `providerSummaries` — and
    /// they have to agree. Sorting each of them separately makes that agreement
    /// something two call sites have to keep remembering.
    @Published var order: [String] = [] {
        didSet {
            guard order != oldValue else { return }
            // Reordered in place, not refetched. The user has just dragged a
            // row and the rings have to follow now; re-reading every credential
            // to answer a question about layout would spend Claude's
            // rate-limit budget on nothing.
            snapshots = ProviderOrder.arrange(snapshots, by: order, id: \.id)
        }
    }

    /// `providers` in the user's order. Every read of `providers` that ends up
    /// on screen goes through this.
    private var orderedProviders: [UsageProvider] {
        ProviderOrder.arrange(providers, by: order, id: \.id)
    }

    /// Whether any provider is actively being used right now. Your usage cannot
    /// move while nothing is running, so polling hard through a quiet afternoon
    /// spends rate-limit budget to re-read a number that has not changed.
    var isBusy: () -> Bool = { false }

    private let refreshInterval: TimeInterval
    private let localRefreshInterval: TimeInterval
    /// How long a snapshot stays believable after its last successful fetch.
    ///
    /// Comfortably above `idleRefreshInterval`, on purpose. With the two equal,
    /// a ring dimmed the instant the *first* idle refresh attempt failed —
    /// which reads as "nothing is being read any more" when what actually
    /// happened is one attempt, five minutes ago, out of what will keep being
    /// tried every five minutes after. The margin buys room for a couple of
    /// those attempts to have genuinely failed before the ring says so; it
    /// must never fire merely because the idle schedule hasn't come round yet.
    private let staleAfter: TimeInterval
    /// How often to look when nothing is running.
    private let idleRefreshInterval: TimeInterval
    private var lastAttempt: Date?
    private let pollingNow: () -> Date

    private let archive: UsageArchive
    private var lastGood: [String: (snapshot: ProviderSnapshot, fetchedAt: Date)] = [:]
    private var timer: Timer?
    private var localTimer: Timer?
    private var fetchTasks: [String: Task<Void, Never>] = [:]
    private var generations: [String: Int] = [:]
    private var refreshTask: Task<Void, Never>?
    /// Set synchronously before the task exists, so "is one already running"
    /// never depends on when the task body happens to start.
    private var isRefreshing = false
    private var wakeObserver: NSObjectProtocol?
    private var languageObserver: NSObjectProtocol?

    /// How long one pass gets before the store stops waiting for it.
    ///
    /// **Not a cancellation, and it cannot be one.** At the bottom of a Claude
    /// fetch is `SecItemCopyMatching`, which is synchronous and blocks its
    /// thread until macOS resolves the authorization prompt sitting in front of
    /// it. `Task.cancel()` sets a flag; it does not reach into a blocked C
    /// call. What the deadline buys is that *the store* stops waiting — which
    /// is the part that was broken.
    ///
    /// It happened for real: a keychain prompt went unanswered, the pass never
    /// returned, `isRefreshing` stayed true, and every tick after it logged
    /// "refresh skipped: one already in flight" for eighty minutes. The app
    /// looked alive and had silently stopped reading anything.
    ///
    /// The blocked read is not on the main actor — providers are actors, so it
    /// blocks one cooperative thread and the UI keeps running. Left to finish
    /// whenever it finishes; the provider's own entry in `generations` stops
    /// its late answer overwriting a newer one.
    private let refreshDeadline: TimeInterval
    private var deadlineTask: Task<Void, Never>?
    /// A late completion may only finish the pass that created it.
    private var refreshGeneration: UInt64 = 0
    @Published private(set) var diagnostics: [String: UsageDiagnostic] = [:]
    private var retryTasks: [String: Task<Void, Never>] = [:]
    private var fetchDeadlineTasks: [String: Task<Void, Never>] = [:]

    init(
        providers: [UsageProvider],
        refreshInterval: TimeInterval = 60,
        localRefreshInterval: TimeInterval = 1,
        idleRefreshInterval: TimeInterval = 5 * 60,
        staleAfter: TimeInterval = 15 * 60,
        // Thirty times a normal pass, which is a second or two. High enough
        // never to fire on a slow network, low enough that a wedged read costs
        // one tick rather than the rest of the day.
        refreshDeadline: TimeInterval = 60,
        archive: UsageArchive = UsageArchive(),
        disconnected: Set<String> = [],
        order: [String] = [],
        pollingNow: @escaping () -> Date = Date.init
    ) {
        self.pollingNow = pollingNow
        self.providers = providers
        self.refreshInterval = refreshInterval
        self.localRefreshInterval = localRefreshInterval
        self.idleRefreshInterval = idleRefreshInterval
        self.staleAfter = staleAfter
        self.refreshDeadline = refreshDeadline
        self.archive = archive

        // Open on what we knew last time rather than on an empty ring; the
        // first fetch will either confirm it or replace it.
        // Set through the wrapper's storage, not `self.disconnected = …`.
        // `disconnected` is `@Published`, so a plain assignment here runs its
        // `didSet` — which saves `lastGood` to the archive. At this point
        // `lastGood` is still empty, so it wrote an empty archive and destroyed
        // every remembered reading on any launch with a provider switched off.
        _disconnected = Published(initialValue: disconnected)
        _order = Published(initialValue: order)
        lastGood = archive.load()
        for provider in providers where provider.kind == .localRuntime {
            lastGood.removeValue(forKey: provider.id)
        }
        // Pruned here as well as in `didSet`, because `didSet` cannot be relied
        // on to run: it guards against a no-op change, and the value the
        // preference binding delivers a moment later is usually identical to
        // the one passed in here. A provider switched off in a previous session
        // would then keep its archived reading indefinitely.
        if lastGood.keys.contains(where: disconnected.contains) {
            for id in disconnected { lastGood.removeValue(forKey: id) }
            archive.save(lastGood)
        }
        // Filtered here, not only in `didSet`. The store is built before the
        // preference reaches it, so an unfiltered first pass draws every
        // switched-off provider for as long as it takes the binding to arrive.
        snapshots = orderedProviders.filter { !disconnected.contains($0.id) }.compactMap { provider in
            if !provider.isVisibleWhenAbsent && provider.account() == nil {
                return nil
            }
            guard let remembered = lastGood[provider.id] else { return Self.placeholder(provider) }
            var snapshot = remembered.snapshot
            snapshot.status = .stale(since: remembered.fetchedAt)
            // The choice has to reach a reading that was never fetched this
            // launch, or the ring opens on the provider's window and changes its
            // mind a minute later when the first fetch lands.
            return applyingRingCadence(snapshot)
        }
        updateNotchSnapshots()
    }

    private func updateNotchSnapshots() {
        let cells = ProviderOrder.cells(from: snapshots, keeping: notchSnapshots)
        notchSnapshots = ProviderOrder.arrange(cells, by: order, id: \.id)
            .filter { !disconnected.contains($0.id) }
    }

    /// Model discovery does not need to re-read any cloud account's credential.
    var localModelSummaries: [ProviderSummary] {
        ProviderOrder.cells(from: snapshots, keeping: notchSnapshots).compactMap { cell in
            guard let model = cell.localModel else { return nil }
            let runtime = providers.first { $0.id == cell.providerID }?.displayName ?? cell.displayName
            return ProviderSummary(kind: .localRuntime, localModel: model,
                                   sourceProviderID: cell.providerID, runtimeName: runtime,
                                   id: cell.id, name: model.name, glyph: cell.glyph,
                                   account: nil, signIn: .guidance(L10n.t("Loaded in \(runtime).")))
        }
    }

    /// Enough to list the providers in settings without exposing them.
    var providerSummaries: [ProviderSummary] {
        let models = localModelSummaries
        // What each provider last reported, so a settings row can offer only
        // the cadences its provider actually has. Read from the snapshots rather
        // than kept on the provider: the windows belong to the reading.
        let reportedWindows = Dictionary(snapshots.map { ($0.providerID, $0.windows) },
                                         uniquingKeysWith: { first, _ in first })
        let reportedWeekly = Dictionary(snapshots.compactMap { snapshot in
            snapshot.weeklyID.map { (snapshot.providerID, $0) }
        }, uniquingKeysWith: { first, _ in first })
        let summaries = orderedProviders.flatMap { provider in
            let summary = ProviderSummary(kind: provider.kind, id: provider.id, name: provider.displayName,
                            glyph: provider.glyph,
                            account: disconnected.contains(provider.id) ? nil : provider.account(),
                            signIn: provider.signInRoute,
                            wasRefusedAccess: refusedAccess.contains(provider.id),
                            needsSignInRenewal: needsRenewal.contains(provider.id),
                            windows: reportedWindows[provider.id] ?? [],
                            ringCadences: Self.ringCadences(
                                in: reportedWindows[provider.id] ?? [],
                                weeklyID: reportedWeekly[provider.id],
                                provider: provider),
                            connectionNote: diagnostics[provider.id]?.failure?.message(at: Date()))
            return [summary] + models.filter { $0.sourceProviderID == provider.id }
        }
        return ProviderOrder.arrange(summaries, by: order, id: \.id)
    }

    /// The cadences a settings row may offer: what the provider says it can
    /// answer, and otherwise what the shared rule can name from the windows.
    private static func ringCadences(in windows: [LimitWindow], weeklyID: String?,
                                     provider: UsageProvider?) -> [RingCadence] {
        if let own = provider?.ringCadences(in: windows), !own.isEmpty { return own }
        return HeadlineWindow.cadences(in: windows, weeklyID: weeklyID)
    }

    func start() {
        refreshNow()

        let timer = Timer(timeInterval: refreshInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer

        let localTimer = Timer(timeInterval: localRefreshInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshLocalRuntimes() }
        }
        RunLoop.main.add(localTimer, forMode: .common)
        self.localTimer = localTimer

        // Waking up is the one moment the numbers are guaranteed to be wrong.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshNow() }
        }

        // A window's `label` is display text a provider resolved while it was
        // parsing, and it is stored — archived to disk with the rest of the
        // reading. Everything else on a tooltip is computed as it is drawn and
        // so follows a language change immediately; the labels do not, and
        // stayed in the old language across a relaunch. Re-reading is what
        // rebuilds them, because it is the parse that names them.
        languageObserver = NotificationCenter.default.addObserver(
            forName: L10n.didChange, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshNow() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        localTimer?.invalidate()
        localTimer = nil
        refreshTask?.cancel()
        for task in retryTasks.values { task.cancel() }
        retryTasks.removeAll()
        for id in Array(fetchTasks.keys) { cancelRefresh(providerID: id) }
        deadlineTask?.cancel()
        deadlineTask = nil
        refreshGeneration &+= 1
        isRefreshing = false
        // Block-based observers are not removed by `removeObserver(self)`.
        if let languageObserver {
            NotificationCenter.default.removeObserver(languageObserver)
            self.languageObserver = nil
        }
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
            self.wakeObserver = nil
        }
    }

    /// Decides whether this tick is worth a request at all.
    private func tick() {
        let now = pollingNow()
        let waited = lastAttempt.map { now.timeIntervalSince($0) } ?? .greatestFiniteMagnitude
        guard Self.shouldRefresh(
            isBusy: isBusy(),
            sinceLastAttempt: waited,
            idleInterval: idleRefreshInterval,
            resetDue: Self.hasWindowRolledOver(in: snapshots, since: lastAttempt, at: now)
        ) else { return }
        refreshNow()
    }

    /// True when a window's `resetsAt` fell between the last attempt and now.
    ///
    /// That boundary is the one moment the numbers are certain to have moved,
    /// and it is the moment a reset alert is owed — waiting out the idle
    /// interval there is what made the alert arrive minutes late. It fires once
    /// per rollover: the refresh it asks for puts `lastAttempt` past the
    /// boundary, so the next tick no longer sees it.
    ///
    /// Pure, for the reason `shouldRefresh` is: the boundary it looks for is a
    /// date, and a test of it should not need a store or a clock.
    static func hasWindowRolledOver(in snapshots: [ProviderSnapshot], since last: Date?, at now: Date) -> Bool {
        guard let last else { return false }
        return snapshots.contains { snapshot in
            snapshot.windows.contains { window in
                guard let resetsAt = window.resetsAt else { return false }
                return resetsAt > last && resetsAt <= now
            }
        }
    }

    /// Poll at full rate while something is running or a window has just rolled
    /// over; otherwise wait out the idle interval. Pure, so the schedule can be
    /// tested without a clock.
    static func shouldRefresh(
        isBusy: Bool,
        sinceLastAttempt: TimeInterval,
        idleInterval: TimeInterval,
        resetDue: Bool = false
    ) -> Bool {
        isBusy || resetDue || sinceLastAttempt >= idleInterval
    }

    func refreshNow() {
        guard !isRefreshing else {
            Log.usage.notice("refresh skipped: one already in flight")
            return
        }
        isRefreshing = true
        refreshGeneration &+= 1
        let generation = refreshGeneration
        lastAttempt = pollingNow()
        refreshTask = Task { [weak self] in
            await self?.refresh()
            self?.finish(generation: generation)
        }
        armDeadline(generation: generation)
    }

    /// Stops waiting for a pass that has not come back, so the next tick can
    /// run. Deliberately does not touch the pass itself — there is nothing here
    /// that could stop it.
    private func armDeadline(generation: UInt64) {
        let deadline = refreshDeadline
        deadlineTask?.cancel()
        deadlineTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(deadline * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.abandon(generation: generation)
        }
    }

    /// A pass came back. Clears the flag whatever the outcome — success, thrown
    /// error, or cancellation — because the one thing that must never happen is
    /// the flag outliving the work.
    ///
    /// `refreshing` is not this function's to clear: each provider's own task
    /// removes itself from it as it lands, so a second write here would only
    /// ever repeat what that one just did — or clear a cell that a
    /// single-provider refresh still genuinely owns.
    private func finish(generation: UInt64) {
        guard refreshGeneration == generation else { return }
        deadlineTask?.cancel()
        deadlineTask = nil
        isRefreshing = false
    }

    /// A pass outlived its deadline.
    ///
    /// Says so on the providers that never answered, using the same degrading
    /// path as any other failure: a remembered reading is re-shown and ages,
    /// and a provider with nothing to show says it got no response. Then frees
    /// the flag so the schedule resumes.
    private func abandon(generation: UInt64) {
        guard isRefreshing, refreshGeneration == generation else { return }
        // Whatever is still in the task table never answered. The tasks are
        // left exactly where they are: a provider is an actor, so cancelling
        // one that is blocked inside a synchronous keychain call changes
        // nothing, and dropping it from the table would let the next pass
        // queue a second call behind the first — which is how one stuck
        // provider used to take all of them down.
        let stuck = fetchTasks.keys.sorted()
        Log.usage.error("refresh abandoned after \(self.refreshDeadline, format: .fixed(precision: 0))s; no answer from: \(stuck.joined(separator: ", "), privacy: .public)")
        for id in stuck {
            guard let provider = providers.first(where: { $0.id == id }) else { continue }
            abandonFetch(provider, generation: generations[id, default: 0])
        }
        isRefreshing = false
    }

    func refresh() async {
        // The provider tasks below do not inherit this task's cancellation.
        guard !Task.isCancelled else { return }
        let tasks = orderedProviders.filter { !disconnected.contains($0.id) }.map {
            beginRefresh($0)
        }
        for task in tasks { await task.value }
    }

    /// Refetch one provider, leaving the others alone.
    ///
    /// Deliberately not routed through `refreshNow`: asking one cell for a fresh
    /// reading should not spend every other provider's rate-limit budget, and
    /// Claude's in particular is easy to exhaust.
    @discardableResult
    func refresh(providerID: String) -> Task<Void, Never>? {
        guard let provider = providers.first(where: { $0.id == providerID }),
              !disconnected.contains(providerID) else { return nil }
        if let task = fetchTasks[providerID] { return task }
        if provider.kind == .usage { lastAttempt = pollingNow() }
        return beginRefresh(provider, holdIndicator: true)
    }

    func refreshLocalRuntimes() {
        for provider in providers where provider.kind == .localRuntime && !disconnected.contains(provider.id) {
            _ = beginRefresh(provider)
        }
    }

    func updateOllamaEndpoint(_ endpoint: URL) {
        guard let provider = providers.first(where: { $0.id == "ollama-local" }) as? OllamaLocalProvider,
              provider.endpoint != endpoint else { return }
        restart(provider) { provider.endpoint = endpoint }
    }

    func updateLMStudioEndpoint(_ endpoint: URL) {
        guard let provider = providers.first(where: { $0.id == LMStudioMetrics.providerID }) as? LMStudioLocalProvider,
              provider.endpoint != endpoint else { return }
        restart(provider) { provider.endpoint = endpoint }
    }

    /// A changed address makes whatever the old one was about to answer
    /// untrue; the reading is cleared and the new address asked at once.
    private func restart(_ provider: UsageProvider, applying change: () -> Void) {
        cancelRefresh(providerID: provider.id)
        change()
        guard !disconnected.contains(provider.id) else { return }
        publish(Self.placeholder(provider))
        _ = beginRefresh(provider)
    }

    private func beginRefresh(_ provider: UsageProvider, holdIndicator: Bool = false) -> Task<Void, Never> {
        if let task = fetchTasks[provider.id] { return task }
        if !provider.canRefreshDuringBackoff,
           let retryAt = diagnostics[provider.id]?.failure?.retryAt, retryAt > Date() {
            return Task {}
        }
        let generation = generations[provider.id, default: 0]
        var diagnostic = diagnostics[provider.id] ?? UsageDiagnostic(providerID: provider.id)
        if diagnostic.measuredAt == nil {
            diagnostic.measuredAt = lastGood[provider.id]?.fetchedAt
            diagnostic.source = lastGood[provider.id]?.snapshot.observation?.source
        }
        diagnostic.lastAttemptAt = Date()
        diagnostic.inFlight = true
        diagnostic.attempts += 1
        diagnostics[provider.id] = diagnostic
        refreshing.insert(provider.id)
        let task = Task { [weak self] in
            guard let self else { return }
            if let fresh = await snapshot(from: provider, generation: generation) {
                publish(fresh)
            } else if acceptsResult(from: provider, generation: generation) {
                snapshots.removeAll { $0.id == provider.id }
            }
            if holdIndicator { try? await Task.sleep(nanoseconds: 380_000_000) }
            guard generations[provider.id, default: 0] == generation else { return }
            refreshing.remove(provider.id)
            fetchTasks.removeValue(forKey: provider.id)
            fetchDeadlineTasks.removeValue(forKey: provider.id)?.cancel()
            diagnostics[provider.id]?.inFlight = false
            UsageDiagnostics.persist(diagnostics)
        }
        fetchTasks[provider.id] = task
        let deadline = refreshDeadline
        fetchDeadlineTasks[provider.id] = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, deadline) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.abandonFetch(provider, generation: generation)
        }
        return task
    }

    /// Also protects an individual/manual refresh when no global pass is running.
    /// The underlying blocked call remains registered so retries cannot pile up.
    private func abandonFetch(_ provider: UsageProvider, generation: Int) {
        guard acceptsResult(from: provider, generation: generation),
              fetchTasks[provider.id] != nil, refreshing.contains(provider.id) else { return }
        if let stale = degraded(provider: provider, error: UsageProviderError.timedOut) {
            publish(stale)
        } else {
            snapshots.removeAll { $0.id == provider.id }
        }
        refreshing.remove(provider.id)
    }

    private func cancelRefresh(providerID: String) {
        fetchDeadlineTasks.removeValue(forKey: providerID)?.cancel()
        retryTasks.removeValue(forKey: providerID)?.cancel()
        diagnostics[providerID]?.inFlight = false
        generations[providerID, default: 0] += 1
        fetchTasks.removeValue(forKey: providerID)?.cancel()
        refreshing.remove(providerID)
    }

    private func publish(_ snapshot: ProviderSnapshot) {
        guard !disconnected.contains(snapshot.id) else { return }
        // The provider's declaration is recorded here, on the way in, so that
        // the store can always undo its own override.
        let snapshot = applyingRingCadence(snapshot)
        var current = Dictionary(uniqueKeysWithValues: snapshots.map { ($0.id, $0) })
        current[snapshot.id] = snapshot
        snapshots = orderedProviders.compactMap { disconnected.contains($0.id) ? nil : current[$0.id] }
    }

    /// Sign out of one provider: discard anything of its account that this app
    /// is holding, and stop reading it.
    ///
    /// Deliberately more than `disconnected` alone. Switching a provider off
    /// stops the *next* read; it leaves the last one archived, so the numbers
    /// come back on the next launch. Someone who presses Sign out means those
    /// numbers to be gone.
    ///
    /// What it cannot do is end the session at the vendor: for every provider
    /// shipping today the credential belongs to Claude Code, Cursor or Codex,
    /// and deleting their keychain item would sign the user out of an app they
    /// did not ask us to touch. `SignInRoute.signOutCaveat` says so on the row.
    func signOut(providerID: String) {
        guard let provider = providers.first(where: { $0.id == providerID }) else { return }
        cancelRefresh(providerID: providerID)
        refusedAccess.remove(providerID)
        snapshots.removeAll { $0.id == providerID }
        lastGood.removeValue(forKey: providerID)
        diagnostics.removeValue(forKey: providerID)
        UsageDiagnostics.persist(diagnostics)
        archive.forget(providerID)

        Task { await provider.signOut() }
    }

    /// Take the user to wherever this provider's account is signed into.
    ///
    /// Three different places, because the providers differ in what they own: a
    /// `WebSessionProvider` presents its own modal, Cursor and Codex hold the
    /// credential in their app so the app is launched, and Claude Code is a
    /// command with nothing to open — the row's guidance is all there is.
    /// Returns whether anything was actually opened, so the sheet can say so
    /// when nothing was.
    @discardableResult
    func signIn(providerID: String) -> Bool {
        guard let provider = providers.first(where: { $0.id == providerID }) else { return false }

        // Already holding a usable credential: connecting is the whole job, and
        // throwing up a sign-in window over a signed-in account is just noise.
        if provider.account() != nil {
            refresh(providerID: providerID)
            return true
        }

        return openAccountSource(providerID: providerID)
    }

    /// Tell consumers that a provider has just confirmed authentication. The
    /// refresh updates the notch; the revision updates Settings' account row.
    func providerAuthenticationChanged(providerID: String) {
        providerAccountRevision &+= 1
        refresh(providerID: providerID)
    }

    /// Say that a provider's saved login needs renewing by hand.
    ///
    /// Called by `ClaudeTokenRefresher` when it tried and the expiry did not
    /// move. The store only carries the fact so Settings can show it; it starts
    /// nothing and retries nothing.
    func reportRenewalFailed(providerID: String) {
        guard !needsRenewal.contains(providerID) else { return }
        needsRenewal.insert(providerID)
        Log.usage.notice("\(providerID, privacy: .public): saved login needs renewing by hand")
    }

    /// Ask macOS for this provider's credential again.
    ///
    /// The remedy for a declined keychain prompt. Dropping the in-memory copy
    /// first is the part that matters: a plain refresh is served from the cache
    /// whenever the token is still valid, so the keychain is never touched and
    /// the prompt never returns — the button would appear to do nothing.
    func reauthorize(providerID: String) {
        providers.first { $0.id == providerID }?.forgetCachedCredential()
        refresh(providerID: providerID)
    }

    /// Take the user to where this provider's account is *changed*.
    ///
    /// Same destination as signing in, but unconditional: switching accounts is
    /// something you do while already signed in, so the shortcut `signIn` takes
    /// when a credential exists is exactly wrong here.
    ///
    /// Provider Monitor cannot switch the account itself. The credential belongs to
    /// Claude Code, Cursor or Codex, and the most this can honestly do is open
    /// the thing that owns it.
    @discardableResult
    func openAccountSource(providerID: String, switching: Bool = false) -> Bool {
        guard let provider = providers.first(where: { $0.id == providerID }) else { return false }

        switch provider.signInRoute {
        case .modal:
            if switching {
                provider.presentAccountSwitch()
            } else {
                provider.presentSignIn()
            }
            return true
        case .openApp(let bundleID, _):
            guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
            else { return false }
            NSWorkspace.shared.openApplication(at: url, configuration: .init())
            return true
        case .guidance:
            // Claude Code: nothing to open. The row's guidance is the whole
            // answer, so the sheet has to show it rather than pretend.
            return false
        }
    }

    func reevaluate(providerID: String) {
        guard let provider = providers.first(where: { $0.id == providerID }),
              let idx = snapshots.firstIndex(where: { $0.id == providerID })
        else { return }
        let snapshot = snapshots[idx]
        var updated = applyingRingCadence(snapshot)
        // Antigravity's second ring follows the model family the same way its
        // first does, and that family is a setting of its own.
        if let ag = provider as? AntigravityProvider {
            updated.weeklyID = ag.resolveWeeklyID(for: updated.windows)
        }
        guard updated != snapshot else { return }
        snapshots[idx] = updated
        updateNotchSnapshots()
    }

    /// Which window each provider's ring means, by provider id. Absent means the
    /// provider's own choice. Held here rather than read from Preferences at
    /// each use, because the store reads it off the main actor.
    ///
    /// Deliberately not `@Published`: its `didSet` runs at init, where
    /// `snapshots` is not built yet — the same trap `disconnected` documents.
    var ringCadence: [String: RingCadence] = [:] {
        didSet {
            guard ringCadence != oldValue else { return }
            reapplyRingCadence()
        }
    }

    /// Which half of a used-fraction every ring's number reports. Held for the
    /// same reason `ringCadence` is, and not `@Published` for the same reason:
    /// its `didSet` runs at init, where `snapshots` is not built yet.
    var percentBasis: Percent.Basis = .remaining {
        didSet {
            guard percentBasis != oldValue else { return }
            reapplyPercentBasis()
        }
    }

    /// Re-letters every ring from the numbers already in hand. No refetch: a
    /// choice about which half of a percentage to print cannot change what the
    /// provider reported, and asking again to answer it spends that provider's
    /// rate-limit budget for nothing.
    func reapplyPercentBasis() {
        var changed = false
        for index in snapshots.indices where snapshots[index].percentBasis != percentBasis {
            snapshots[index].percentBasis = percentBasis
            changed = true
        }
        if changed { updateNotchSnapshots() }
    }

    /// Re-picks every ring's window from the numbers already in hand.
    ///
    /// No refetch: the windows a provider reports do not change because the
    /// user changed which of them to look at, and asking Claude for a fresh
    /// reading to answer a question about the reading we already have spends
    /// that provider's rate-limit budget for nothing.
    func reapplyRingCadence() {
        var changed = false
        for index in snapshots.indices {
            let updated = applyingRingCadence(snapshots[index])
            guard updated != snapshots[index] else { continue }
            snapshots[index] = updated
            changed = true
        }
        if changed { updateNotchSnapshots() }
    }

    /// The provider's own answer where it has one, and the user's cadence on
    /// top of the provider's declaration otherwise.
    ///
    /// Also stamps the percent basis, which is the other choice the store makes
    /// on the user's behalf: both are decided once, here, as the reading goes
    /// in, so nothing downstream has to consult `Preferences` to draw a
    /// snapshot and nothing can disagree with anything else about it.
    private func applyingRingCadence(_ snapshot: ProviderSnapshot) -> ProviderSnapshot {
        var snapshot = snapshot
        snapshot.percentBasis = percentBasis
        // Captured the first time the reading is seen, and never overwritten:
        // the provider's declaration is what Automatic has to go back to, and
        // after the first override the current value is no longer it.
        if snapshot.declaredHeadlineID == nil { snapshot.declaredHeadlineID = snapshot.headlineID }
        let cadence = ringCadence[snapshot.providerID] ?? .automatic
        // The card's switch and its badge both read this off the snapshot, so
        // it travels with the reading rather than being asked of Preferences
        // again per view.
        snapshot.ringCadence = cadence
        let provider = providers.first { $0.id == snapshot.providerID }
        // The switch's own list and the window each choice means, both resolved
        // here while the provider is still in hand — see
        // `ProviderSnapshot.offeredCadences` for why the snapshot cannot work
        // this out from its windows alone.
        snapshot.offeredCadences = Self.ringCadences(in: snapshot.windows,
                                                    weeklyID: snapshot.weeklyID,
                                                    provider: provider)
        snapshot.cadenceWindows = snapshot.offeredCadences.reduce(into: [:]) { result, cadence in
            let id = provider?.resolveRingWindow(in: snapshot.windows, cadence: cadence)
                ?? HeadlineWindow.window(answering: cadence, in: snapshot.windows,
                                         weeklyID: snapshot.weeklyID)?.id
            if let id { result[cadence] = id }
        }
        if let own = provider?.resolveRingWindow(in: snapshot.windows, cadence: cadence) {
            snapshot.headlineID = own
            return snapshot
        }
        if let resolved = HeadlineWindow.resolve(windows: snapshot.windows,
                                                declared: snapshot.declaredHeadlineID,
                                                weeklyID: snapshot.weeklyID,
                                                cadence: cadence) {
            snapshot.headlineID = resolved
        }
        return snapshot
    }

    private func snapshot(from provider: UsageProvider, generation: Int) async -> ProviderSnapshot? {
        // A scheduled task can be disconnected before it begins; avoid reading
        // its credential at all, as well as rejecting an obsolete response.
        guard acceptsResult(from: provider, generation: generation) else { return nil }
        let accountBefore = provider.usageAccountID
        forgetChangedAccount(providerID: provider.id, current: accountBefore)
        do {
            var fresh = try await provider.fetchSnapshot()
            guard acceptsResult(from: provider, generation: generation) else { return nil }
            let accountAfter = provider.usageAccountID
            guard accountBefore == accountAfter else { throw UsageProviderError.accountChanged }
            // Model residency becomes untrue as soon as a server stops. It must
            // never use quota's last-good cache or survive an app relaunch.
            if provider.kind == .usage {
                let now = Date()
                var observation = fresh.observation ?? UsageObservation(
                    measuredAt: fresh.status.staleSince ?? now, source: .provider)
                observation.accountID = accountAfter
                fresh.observation = observation
                if fresh.status == .ok, now.timeIntervalSince(observation.measuredAt) > staleAfter {
                    fresh.status = .stale(since: observation.measuredAt)
                }
                lastGood[provider.id] = (fresh, observation.measuredAt)
                archive.save(lastGood)
            }
            fresh.connectionFailure = nil
            var diagnostic = diagnostics[provider.id] ?? UsageDiagnostic(providerID: provider.id)
            diagnostic.lastSuccessfulReadAt = Date()
            diagnostic.measuredAt = fresh.observation?.measuredAt
            diagnostic.source = fresh.observation?.source
            diagnostic.failure = nil
            diagnostics[provider.id] = diagnostic
            retryTasks.removeValue(forKey: provider.id)?.cancel()
            refusedAccess.remove(provider.id)
            // A reading that actually came back is proof the credential works,
            // whatever was thought a moment ago. The only way this clears —
            // there is no timer and nothing retries.
            needsRenewal.remove(provider.id)
            if let observation = fresh.observation {
                Log.usage.notice("\(provider.id, privacy: .public): source=\(observation.source.rawValue, privacy: .public), measurement age=\(Int(max(0, Date().timeIntervalSince(observation.measuredAt))), privacy: .public)s")
            }
            return fresh
        } catch {
            guard acceptsResult(from: provider, generation: generation) else { return nil }
            forgetChangedAccount(providerID: provider.id, current: provider.usageAccountID)
            if provider.kind == .localRuntime {
                var empty = Self.placeholder(provider)
                empty.status = .error(error.localizedDescription)
                return empty
            }
            Log.usage.error("\(provider.id, privacy: .public) failed: \(String(describing: error), privacy: .public)")
            return degraded(provider: provider, error: error)
        }
    }

    private func forgetChangedAccount(providerID: String, current: String?) {
        guard let previous = lastGood[providerID]?.snapshot.observation?.accountID,
              previous != current else { return }
        lastGood.removeValue(forKey: providerID)
        archive.forget(providerID)
        snapshots.removeAll { $0.id == providerID }
    }

    private func acceptsResult(from provider: UsageProvider, generation: Int) -> Bool {
        !Task.isCancelled && !disconnected.contains(provider.id)
            && generations[provider.id, default: 0] == generation
    }

    /// A failed fetch never invents a number: it either re-shows the last good
    /// one marked stale, or shows the cell with no reading at all.
    private func degraded(provider: UsageProvider, error: Error) -> ProviderSnapshot? {
        let failure = UsageFailure(error)
        var diagnostic = diagnostics[provider.id] ?? UsageDiagnostic(providerID: provider.id)
        diagnostic.failure = failure
        diagnostics[provider.id] = diagnostic
        scheduleRetry(providerID: provider.id, failure: failure)
        UsageDiagnostics.persist(diagnostics)
        if !provider.isVisibleWhenAbsent {
            lastGood[provider.id] = nil
            archive.save(lastGood)
            return nil
        }

        let status = Self.status(for: error)
        let sameAccount = lastGood[provider.id]?.snapshot.observation?.accountID.map {
            $0 == provider.usageAccountID
        } ?? false

        // Remembered apart from the snapshot on purpose. The snapshot answers
        // "how good are the numbers I am showing", and for a refusal the honest
        // answer is "still fine, just ageing" — which is why `supersedesHistory`
        // keeps the old reading and its status. That deliberately loses the one
        // fact the settings row needs: whether macOS let us in last time. Two
        // different questions, so two different places to keep the answer.
        if case .accessDenied = status {
            refusedAccess.insert(provider.id)
        } else {
            refusedAccess.remove(provider.id)
        }

        // Some failures are statements about the account rather than a hiccup:
        // signed out, or a plan that meters nothing. Re-showing an old reading
        // through one of those would present a number that is no longer true —
        // and, after an endpoint change, one that came from somewhere we no
        // longer read. So the remembered reading is dropped, not dimmed.
        if Self.supersedesHistory(status) && !(status == .needsAuth && sameAccount) {
            lastGood[provider.id] = nil
            archive.save(lastGood)
            var empty = Self.placeholder(provider)
            empty.status = status
            empty.connectionFailure = failure
            return empty
        }

        guard let previous = lastGood[provider.id] else {
            var empty = Self.placeholder(provider)
            empty.status = status
            empty.connectionFailure = failure
            return empty
        }

        // A stale-but-recent reading is still worth showing undimmed; past the
        // window it gets marked, and the ring dims.
        let age = Date().timeIntervalSince(previous.fetchedAt)
        var snapshot = previous.snapshot
        snapshot.connectionFailure = failure
        snapshot.status = age > staleAfter || status == .needsAuth
            ? .stale(since: previous.fetchedAt) : previous.snapshot.status
        return snapshot
    }

    /// Wake at the provider's retry deadline, rather than losing another idle
    /// polling interval. Manual refresh uses the same gate and cannot bypass it.
    private func scheduleRetry(providerID: String, failure: UsageFailure) {
        retryTasks.removeValue(forKey: providerID)?.cancel()
        guard let retryAt = failure.retryAt else { return }
        let generation = generations[providerID, default: 0]
        retryTasks[providerID] = Task { [weak self] in
            let interval = min(max(0, retryAt.timeIntervalSinceNow), 24 * 3600)
            try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            if let pending = fetchTasks[providerID] { await pending.value }
            guard !Task.isCancelled, !disconnected.contains(providerID),
                  generations[providerID, default: 0] == generation,
                  diagnostics[providerID]?.failure?.retryAt == retryAt else { return }
            if retryAt > Date() {
                scheduleRetry(providerID: providerID, failure: failure)
            } else {
                retryTasks.removeValue(forKey: providerID)
                _ = refresh(providerID: providerID)
            }
        }
    }

    /// True when the new status makes any remembered reading untrue rather than
    /// merely old.
    static func supersedesHistory(_ status: ProviderStatus) -> Bool {
        switch status {
        case .needsAuth, .unsupported: return true
        // A refusal says nothing about the reading — the credential is there
        // and still valid, we were simply not let in to re-read it. Discarding
        // the last number would punish someone for pressing the wrong button.
        case .accessDenied:            return false
        // The account was not closed and the numbers were not wrong — the
        // owning app dropped its own token. Blanking the ring here is what
        // turned a recurring overnight glitch into apparent data loss.
        case .signedOutByOwner:        return false
        case .ok, .stale, .error:      return false
        }
    }

    /// Exposed for the tests: the store never invents a reading, so what a
    /// failure looks like is worth pinning down.
    static func statusForTesting(_ error: Error) -> ProviderStatus { status(for: error) }

    /// Exposed so a test can hold the shipped defaults to the margin they are
    /// supposed to keep, without re-typing the numbers on both sides.
    var staleAfterForTesting: TimeInterval { staleAfter }
    /// Exposed so a test can prove the flag was released rather than infer it
    /// from a second refresh happening to work.
    var isRefreshingForTesting: Bool { isRefreshing }
    var inFlightForTesting: Set<String> { Set(fetchTasks.keys) }
    var idleRefreshIntervalForTesting: TimeInterval { idleRefreshInterval }

    private static func status(for error: Error) -> ProviderStatus {
        switch error {
        case UsageProviderError.accountChanged:
            return .error(L10n.t("Account changed during the request. Waiting for a new reading."))
        case UsageProviderError.needsAuth:
            return .needsAuth
        case UsageProviderError.credentialExpired:
            // Ages the reading rather than discarding it: the number was true
            // when it was taken, and the token will refresh itself in the
            // ordinary course of using the app that owns it.
            return .stale(since: Date())
        case UsageProviderError.rateLimited:
            // Not an error the user can do anything about, and the last good
            // reading is still roughly true, so it reads as staleness.
            return .stale(since: Date())
        case UsageProviderError.signedOutByOwner:
            return .signedOutByOwner
        case UsageProviderError.accessDenied:
            return .accessDenied
        case UsageProviderError.timedOut:
            // Nothing is known about the account, so a remembered reading stays
            // and simply ages. `degraded` handles that; this is only what a
            // provider with nothing to show says.
            return .error("no response")
        case UsageProviderError.nothingMetered(let why):
            return .unsupported(why)
        case UsageProviderError.badResponse(let code):
            return .error("HTTP \(code)")
        default:
            return .error((error as NSError).localizedDescription)
        }
    }

    private static func placeholder(_ provider: UsageProvider) -> ProviderSnapshot {
        ProviderSnapshot(
            id: provider.id,
            displayName: provider.displayName,
            glyph: provider.glyph,
            fidelity: .official,
            status: .stale(since: .distantPast),
            windows: [],
            kind: provider.kind
        )
    }
}
