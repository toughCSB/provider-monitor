import Foundation

/// How much to trust a provider's numbers. The UI never presents a derived or
/// manual figure as if a vendor had published it.
extension String {
    /// Nil when this would be an empty plan line on the card.
    var nonEmptyPlan: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

enum Fidelity: String, Codable, Equatable {
    case official
    case derived
    case manual

    /// Prefix shown in front of a percentage that we worked out ourselves.
    var qualifier: String { self == .official ? "" : "~" }
}

/// A provider-reported money balance. The percentage is derived from these
/// exact account amounts; the amounts themselves come from the provider.
struct UsageMoneyBreakdown: Codable, Equatable, Sendable {
    let currency: String
    let spent: Double
    let remaining: Double

    var funded: Double { spent + remaining }

    var spentFraction: Double {
        guard funded > 0 else { return 0 }
        return min(max(spent / funded, 0), 1)
    }
}

enum ProviderStatus: Equatable {
    case ok
    case stale(since: Date)
    case needsAuth
    /// The owning app emptied its own credential. Distinct from `needsAuth`
    /// because the last reading is kept — see `UsageProviderError`.
    case signedOutByOwner
    /// macOS was asked for a credential that exists, and refused.
    case accessDenied
    case unsupported(String)
    case error(String)

    var isStale: Bool { if case .stale = self { return true }; return false }

    /// When the reading behind this status was actually taken.
    var staleSince: Date? { if case .stale(let since) = self { return since }; return nil }
}

/// How percentages read.
///
/// Whole percents above one — "12%", "104%" — because decimals there are
/// noise. Below one, whole percents collapse a real reading into "0%", the one
/// number that looks most like "nothing used", so both halves gain a tenth of
/// a percent and still add up: 0.3% used is 99.7% left. A tenth of nothing
/// says so rather than pretending to be zero.
enum Percent {
    /// Which half of a used-fraction the ring's own number reports.
    ///
    /// The ring draws *usage* whatever this says — the arc fills as the limit
    /// is spent, and an arc that meant the opposite would need the space above
    /// it read as the limit. Only the figure under it changes hands, because
    /// that figure is the thing people quote to each other, and "how much is
    /// left" is the question a quota is usually asked.
    enum Basis: String, CaseIterable, Identifiable {
        case remaining
        case used

        var id: String { rawValue }

        var title: String {
            switch self {
            case .remaining: return L10n.t("Percent left")
            case .used:      return L10n.t("Percent used")
            }
        }

        var explanation: String {
            switch self {
            case .remaining: return L10n.t("The number under each ring counts down as the limit is spent.")
            case .used:      return L10n.t("The number under each ring counts up as the limit is spent.")
            }
        }
    }

    /// The two halves of a used-fraction, as display text.
    static func halves(for fraction: Double) -> (used: String, left: String) {
        let value = fraction * 100
        let fractional = (value > 0 && value < 1) || (value > 99 && value < 100)
        guard fractional else {
            // The left half derives from the *rounded* used half, not from the
            // raw value — 9.5% used is "10% Used · 90% left", because that is
            // how the dashboard the user is comparing against does the maths.
            let used = Int(value.rounded())
            return ("\(used)", "\(max(0, 100 - used))")
        }
        let left = max(0, 100 - value)
        // Keep the compact upper bound free of a comparison sign. The number is a display value,
        // not an inequality, and a leading `>` looked like corrupted usage in the notch.
        return (small(value), left > 99.9 ? "99.9" : small(left))
    }

    /// One percentage, as display text — the ring's label.
    static func text(for fraction: Double) -> String {
        let value = fraction * 100
        guard value > 0, value < 1 else { return "\(Int(value.rounded()))" }
        return small(value)
    }

    static func displayedFraction(for usedFraction: Double, basis: Basis) -> Double {
        let used = min(max(usedFraction, 0), 1)
        return basis == .used ? used : 1 - used
    }

    private static func small(_ value: Double) -> String {
        if value <= 0 { return "0" }
        let tenths = (value * 10).rounded() / 10
        if tenths < 0.1 { return "<0.1" }
        if tenths > 99.9 { return "99.9" }
        // Fixed locale: the decimal point is not up to the system settings,
        // any more than "%" is.
        return String(format: "%.1f", locale: Locale(identifier: "en_US_POSIX"), tenths)
    }
}

/// One metered window a provider exposes — Claude has two (the rolling session
/// and the longer all-models window), others have one.
struct LimitWindow: Identifiable, Codable, Equatable {
    let id: String
    let group: String?
    let label: String
    /// 0...1+, where 1 means the limit is spent. Nil when the provider reports
    /// what is left but never says what the limit was — Perplexity does exactly
    /// this, and a percentage would have to invent the denominator.
    let usedFraction: Double?
    /// How many are left, when that is what the provider reports.
    let remaining: Int?
    /// How many have been spent, when the provider counts up rather than down
    /// and never states the ceiling. Cursor does this.
    let used: Int?
    /// Optional provider-specific value for count-only rows.
    let detail: String?
    /// Structured money data for providers whose account is metered in money.
    let money: UsageMoneyBreakdown?
    /// Optional display override for `used` — used when the raw count would
    /// be the wrong unit (e.g. a dollar balance formatted as "$14.28").
    let usedText: String?
    /// Nil when the provider does not say when the window rolls over.
    let resetsAt: Date?

    /// Exact cycle length when known; optional to keep older archives readable.
    let duration: TimeInterval?

    init(id: String, group: String? = nil, label: String, usedFraction: Double? = nil,
         remaining: Int? = nil, used: Int? = nil, usedText: String? = nil, detail: String? = nil,
         money: UsageMoneyBreakdown? = nil, resetsAt: Date? = nil,
         duration: TimeInterval? = nil) {
        self.id = id
        self.group = group
        self.label = label
        self.usedFraction = usedFraction
        self.remaining = remaining
        self.used = used
        self.usedText = usedText
        self.detail = detail
        self.money = money
        self.resetsAt = resetsAt
        self.duration = duration
    }

    /// A count short enough to sit inside a 44 pt ring.
    ///
    /// Requests and credits are three or four digits and print verbatim; token
    /// counts run to seven, and "651061" under the ring is unreadable at that
    /// width. The threshold is 10 000 so no existing provider's number changes.
    static func compact(_ count: Int) -> String {
        if count < 10_000 { return "\(count)" }
        if count < 1_000_000 { return "\(count / 1_000)k" }
        return String(format: "%.1fM", Double(count) / 1_000_000)
    }

    /// What the tooltip says on the line under the bar.
    var summary: String { summary(locale: L10n.locale) }

    func summary(locale: Locale = L10n.locale) -> String {
        if let usedFraction {
            // Both ends of the same figure. Vendors do not agree on which to
            // show — Codex writes "87% remaining", Claude writes "% used" — so
            // a notch that picks one side leaves the user converting in their
            // head, and "12% Used" beside Codex's "87% remaining" reads as two
            // different numbers rather than one seen from either end. That is
            // what made a correct reading look wrong.
            let halves = Percent.halves(for: usedFraction)
            return L10n.t("\(halves.used)% Used · \(halves.left)% left", locale: locale)
        }
        if let remaining {
            return remaining < 10_000
                ? L10n.t("\(remaining) left", locale: locale)
                : L10n.t("\(Self.compact(remaining)) left", locale: locale)
        }
        if let used {
            if let usedText { return usedText }
            return used < 10_000
                ? L10n.t("\(used) used", locale: locale)
                : L10n.t("\(Self.compact(used)) used", locale: locale)
        }
        return L10n.t("No reading", locale: locale)
    }
}

/// A limit that has been *reached*, even where the headline still shows room.
///
/// Vendors meter some capabilities separately from the plan's main allowance,
/// so "84% left" and "paused until 4:13 PM" are both true at once. A ring that
/// only knows the headline reports the first and hides the second, which is
/// the reading that actually stops you working.
struct UsageBlock: Equatable {
    /// What is paused, in the vendor's own terms.
    let reason: String
    /// When it lifts, where the vendor says.
    let resetsAt: Date?

    /// The line the tooltip leads with.
    func summary(now: Date = Date(), calendar: Calendar = .current,
                 locale: Locale = L10n.locale) -> String {
        guard let resetsAt, resetsAt > now else { return reason }
        let formatter = ResetCopy.formatter(for: calendar)
        formatter.locale = locale
        // The same clock the vendor's own banner uses — "4:13 PM" — rather
        // than a countdown, because that is what you are waiting for. `j`
        // rather than `h` so the hour cycle is the region's, as in
        // `ResetCopy`; a 12-hour region still reads "4:13 PM".
        let template = ResetCopy.daysApart(from: now, to: resetsAt,
                                           calendar: calendar) >= 1
            ? "E j:mm" : "j:mm"
        formatter.setLocalizedDateFormatFromTemplate(template)
        return L10n.t("\(reason) until \(formatter.string(from: resetsAt))", locale: locale)
    }
}

struct ProviderSnapshot: Identifiable, Equatable {
    let id: String
    let displayName: String
    let glyph: ProviderGlyph
    let fidelity: Fidelity
    var status: ProviderStatus
    var windows: [LimitWindow]
    /// Which window the ring means, declared by the provider rather than left to
    /// position. Without it the headline is "whichever window happens to be
    /// first", and a window dropping out of the response silently promotes
    /// another one — the ring keeps its shape and quietly changes its subject.
    var headlineID: String?
    /// What the provider itself declared, before the user's cadence choice had
    /// its say. Written once, when the provider hands the snapshot over, and
    /// never by the store — which is what lets going back to Automatic restore
    /// the provider's own choice instead of freezing whichever window the
    /// override happened to land on.
    var declaredHeadlineID: String?
    /// Which window the weekly ring draws, when it is switched on. Declared
    /// rather than derived — see `weeklyWindow`.
    var weeklyID: String?
    /// Set when something is blocked right now. Deliberately separate from the
    /// windows: it is not a measurement, it is a door being shut.
    var block: UsageBlock?
    var kind: ProviderKind = .usage
    var localRuntime: LocalRuntimeReading?
    var localModel: LocalRuntimeReading.Model?
    var localPerformance: LocalModelPerformance?
    var showsLocalPerformance = false
    /// Which half of a used-fraction the number under the ring reports.
    ///
    /// Stamped onto the reading by the store from the user's own choice rather
    /// than read out of `Preferences` at each use: the snapshot is what the
    /// ring, the status item and the hover card all pass around, and three of
    /// them consulting three answers is how a display choice turns into a
    /// contradiction on screen.
    var percentBasis: Percent.Basis = .used
    /// Which of this provider's windows the ring means, as the user last chose
    /// it — `automatic` when the provider's own declaration stands.
    ///
    /// Stamped by the store from `Preferences` for the same reason
    /// `percentBasis` is: the ring, the menu and the hover card all read the
    /// snapshot, and a choice that lives in three places is three answers.
    var ringCadence: RingCadence = .automatic
    /// Which cadences this reading can be switched between, in the order the
    /// switch draws them and never the default — that is always on offer and
    /// the switch puts it first.
    ///
    /// Stamped by the store rather than worked out here, because only the store
    /// still holds the provider, and for Antigravity the windows alone do not
    /// answer: each of its cadences arrives as two lanes — one per model family
    /// — and a rule that insists on a single answer finds none. That is what
    /// left Antigravity's card offering Weekly with no 5-hour switch at all,
    /// while the same provider answered both in Settings, which asks the
    /// provider directly.
    var offeredCadences: [RingCadence] = []
    /// The window each offered cadence means, resolved once as the reading goes
    /// in. The reset summary reads this: for Antigravity the 5-hour limit is a
    /// choice between two lanes that the shared rule refuses to make, and the
    /// provider's own answer is the one the ring is already drawing.
    var cadenceWindows: [RingCadence: String] = [:]
    /// How full the loaded context was on the last request, from the runtime's
    /// own log. The local ring's arc: a window filling up is the one fraction
    /// a local model has, where a cloud ring has a quota.
    var localContextFraction: Double?
    /// Today's tokens and requests and the shape of the last response, when
    /// the runtime logs them. Read by the tooltip; absent for runtimes that
    /// do not.
    var localLedger: LocalTokenLedger.Summary?
    /// The runtime measures responses itself, so speed is shown without the
    /// Ollama relay switch.
    var localRuntimeMeasuresSpeed = false
    /// A model cell has its own display preference, but polling belongs to the
    /// runtime that supplied it.
    var sourceProviderID: String?

    var providerID: String { sourceProviderID ?? id }

    var notchSnapshots: [ProviderSnapshot] {
        guard kind == .localRuntime, localModel == nil else { return [self] }
        return (localRuntime?.models ?? []).map { model in
            ProviderSnapshot(id: "\(id):model:\(model.id)", displayName: displayName,
                             glyph: model.brand?.glyph ?? glyph,
                             fidelity: fidelity, status: status, windows: [],
                             kind: kind, localModel: model,
                             localRuntimeMeasuresSpeed: localRuntime?.measuresSpeed ?? false,
                             sourceProviderID: id)
        }
    }
    /// Codex's account-wide token activity, when its profile endpoint returned
    /// it. The optional top model is an enrichment from the desktop breakdown
    /// endpoint; it never changes the profile token buckets. Other providers
    /// leave this nil because they do not expose the same account-level data.
    var tokenUsage: CodexTokenUsage? = nil
    /// The account's named tier, when the provider publishes one. Shown under
    /// the tooltip title. Nil when there is nothing to name.
    var plan: String? = nil

    /// Unused rate-limit resets on this Codex account, listed by the same
    /// backend as usage.
    var resetCredits: CodexResetCredits? = nil

    /// Whether the Codex tooltip has a reset-credit section to draw.
    ///
    /// The endpoint can successfully return an empty result. That is data,
    /// but it is not useful card content and must not reserve layout space.
    var hasAvailableResetCredits: Bool {
        (resetCredits?.availableCount ?? 0) > 0
    }
    /// Provider-owned online usage detail, such as DeepSeek's API key/model
    /// breakdown and daily token/cost series.
    var usageDetail: ProviderUsageDetail? = nil

    /// Original measurement time and source, not the time a cache was polled.
    var observation: UsageObservation? = nil
    /// The latest connection result, independent of the age of retained numbers.
    var connectionFailure: UsageFailure? = nil

    func readingNote(at now: Date) -> String? {
        guard kind == .usage, hasReading else { return nil }
        if let observation { return observation.note(at: now) }
        guard let since = status.staleSince, since != .distantPast else { return nil }
        return ElapsedCopy.ago(since: since, now: now)
    }

    /// The number on the cell: the provider's declared primary window — for
    /// Claude, the current session.
    ///
    /// Not the most-constrained window, which is what the design spec asks for.
    /// Picking whichever limit is highest means the headline silently changes
    /// meaning — session one minute, weekly the next — and disagrees with
    /// Claude's own panel, which always leads with the session.
    ///
    /// If the declared window is missing from the response the cell shows no
    /// reading rather than promoting a different one. A blank is honest; a
    /// weekly percentage wearing the session's place is not.
    var headline: LimitWindow? {
        guard let headlineID else { return windows.first }
        return windows.first { $0.id == headlineID }
    }

    var usedFraction: Double? { headline?.usedFraction }

    /// The window the second ring draws, when one is switched on.
    ///
    /// Declared by the provider, exactly like `headlineID`, and for the same
    /// reason: the weekly window is called something different by everyone who
    /// has one — `weekly_all`, `secondary`, `weekly`, `gemini-weekly` — and a
    /// rule that guessed from durations would silently skip whichever provider
    /// had not filled that field in, with nothing on screen to say why.
    ///
    /// Nil means this provider has no second window worth a ring, which is a
    /// real answer rather than a missing one.
    var weeklyWindow: LimitWindow? {
        guard let weeklyID else { return nil }
        // Never the window the headline is already drawing. Providers that pick
        // their headline by which limit is tightest — Antigravity does — will
        // sometimes land on the weekly one, and two rings reporting the same
        // number is worse than one: it reads as a second fact that happens to
        // agree, rather than as the same fact twice.
        guard weeklyID != headlineID else { return nil }
        return windows.first { $0.id == weeklyID }
    }

    /// Nil when there is no weekly window, or when the provider reports one
    /// without a denominator — the same rule the headline ring follows.
    var weeklyFraction: Double? { weeklyWindow?.usedFraction }

    /// What the cell prints under the ring.
    var headlineText: String {
        if kind == .localRuntime {
            return showsLocalPerformance ? (localPerformance?.headlineText ?? "— tok/s")
                : (localModel?.memoryText ?? "—")
        }
        // `halves` rather than `text`: the two halves of one reading have to
        // agree, and they only do when both are derived from the same rounding.
        // "<0.1% left" beside "99.9% used" is the failure this avoids.
        if let usedFraction {
            let halves = Percent.halves(for: usedFraction)
            return (percentBasis == .remaining ? halves.left : halves.used) + "%"
        }
        if let remaining = headline?.remaining { return LimitWindow.compact(remaining) }
        if let usedText = headline?.usedText { return usedText }
        if let used = headline?.used { return LimitWindow.compact(used) }
        return "—"
    }

    /// An empty local inventory still confirms server connectivity; an absent
    /// reading must not be shown as measured zero usage.
    var hasReading: Bool { localRuntime != nil || localModel != nil || !windows.isEmpty }

    /// Group headings occupy space in both the card and its hover region.
    var windowGroupCount: Int { Set(windows.compactMap(\.group)).count }

    /// How many windows are count-only (no fraction, no bar) — they render as
    /// single-line rows and take less vertical space than full bar rows.
    var compactRowCount: Int {
        windows.filter { $0.usedFraction == nil && ($0.used != nil || $0.detail != nil) }.count
    }

    /// A ring can only be drawn when the provider said what the limit was. A
    /// local model has no limit; its arc is how full the context was.
    var ringFraction: Double? { kind == .localRuntime ? localContextFraction : usedFraction }

    /// The fraction the arc draws. Cloud quotas follow the same basis as the number below them;
    /// local context rings have no remaining/used preference and keep their measured fill.
    var displayedRingFraction: Double? {
        guard let fraction = ringFraction else { return nil }
        guard kind != .localRuntime else { return fraction }
        return Percent.displayedFraction(for: fraction, basis: percentBasis)
    }

    /// Rows the tooltip adds for a logged runtime: context used, tokens and
    /// requests today, reasoning share, draft acceptance. Counted here so the
    /// card's budget and its contents cannot disagree.
    var localLedgerRowCount: Int { localLedger == nil ? 0 : 5 }

    /// The windows the hover card's reset summary leads with, in the order it
    /// draws them: the window the ring itself reads, and then the short one
    /// beside it when the two are not the same window.
    ///
    /// The same pair the Windows port's card leads with. Answered from the
    /// reading rather than from the provider, so nothing is fetched twice and
    /// the ring and the summary cannot name different windows.
    ///
    /// A window that published no reset time still gets its column. Requiring
    /// one is what left a ring reading a week with a one-figure summary while
    /// the session beside it — the window Claude reports whether or not it has
    /// started — had nowhere to appear at all. The block has always drawn a dash
    /// for the gap, so the column was missing from this list and not from the
    /// design.
    ///
    /// Empty when neither window has anything to count down to, which is what
    /// keeps a provider that never publishes a reset from growing a block of
    /// dashes.
    var resetSummaryIDs: [String] {
        guard kind == .usage, !windows.isEmpty else { return [] }
        var ids: [String] = []
        if let lead = windows.first(where: { $0.id == headlineID }) ?? windows.first(where: { $0.resetsAt != nil }) {
            ids.append(lead.id)
        }
        let shortID = cadenceWindows[.fiveHour]
            ?? HeadlineWindow.window(answering: .fiveHour, in: windows, weeklyID: weeklyID)?.id
        if let shortID, !ids.contains(shortID) {
            ids.append(shortID)
        }
        let countsDown = ids.contains { id in
            guard let resets = windows.first(where: { $0.id == id })?.resetsAt else { return false }
            return resets > .distantPast
        }
        return countsDown ? ids : []
    }

    /// Which limit the ring is reading, in shorthand: M, W or 5h.
    ///
    /// Read off the window itself rather than off the choice, so a ring left on
    /// Automatic still says which limit its number is. A weekly reading and a
    /// session reading look identical otherwise, and telling them apart without
    /// a hover is the whole reason these letters are on the ring.
    var ringBadge: RingCadence? {
        guard kind == .usage, localModel == nil, hasReading else { return nil }
        if ringCadence != .automatic { return ringCadence }
        guard let window = windows.first(where: { $0.id == headlineID }) else { return nil }
        let durations = RingCadence.allCases.filter { $0 != .automatic }
        if let length = window.duration {
            return durations.first { $0.covers(length) }
        }
        return durations.first { $0.names(window.id, window.label) }
    }

    /// The cadences the hover card may switch this provider's ring between,
    /// `automatic` first. Empty when there is nothing to choose — one window,
    /// or a reading whose windows answer no cadence at all — which is what
    /// keeps the switch off a card where it would do nothing.
    ///
    /// The same rule the Settings row follows, including the last clause: a
    /// choice that has since stopped being answerable still has to appear, or
    /// the row would highlight nothing at all.
    var switchableCadences: [RingCadence] {
        guard kind == .usage, windows.count > 1 else { return [] }
        // The provider's own answer where the store kept one, and the shared
        // rule otherwise — the same order Settings uses, so the switch on the
        // card and the row in Settings can never offer different sets.
        let available = offeredCadences.isEmpty
            ? HeadlineWindow.cadences(in: windows, weeklyID: weeklyID)
            : offeredCadences
        guard !available.isEmpty else { return [] }
        var options: [RingCadence] = [.automatic] + available
        if ringCadence != .automatic, !available.contains(ringCadence) {
            options.append(ringCadence)
        }
        return options
    }

    /// Signing in means something different per provider, so the prompt has to
    /// say which door to knock on.
    private var authPrompt: String {
        let locale = L10n.locale
        switch id {
        case "claude":     return L10n.t("Sign in to Claude Code to read your usage", locale: locale)
        // A profile is signed in by running Claude Code against its directory,
        // which is worth saying: plain `claude` signs the default one in.
        case _ where ClaudeProfile.isClaude(providerID: id):
            let slug = ClaudeProfile.slug(fromProviderID: id) ?? ""
            return L10n.t("Sign in to Claude Code in ~/.claude-\(slug) to read your usage", locale: locale)
        case "cursor":     return L10n.t("Sign in to Cursor in the editor", locale: locale)
        case "codex":      return L10n.t("Sign in to Codex to read your usage", locale: locale)
        case "deepseek":   return L10n.t("Sign in to DeepSeek Platform to read your usage", locale: locale)
        case _ where CodexProfile.slug(fromProviderID: id) != nil:
            let slug = CodexProfile.slug(fromProviderID: id)!
            return L10n.t("Sign in to Codex in ~/.codex-\(slug) to read your usage", locale: locale)
        case "gemini":     return L10n.t("Sign in to Antigravity to read your usage", locale: locale)
        case "glm":        return L10n.t("Set up a GLM Coding Plan key for a coding tool to read your usage", locale: locale)
        case "copilot":    return L10n.t("Sign in with GitHub CLI to read your Copilot usage", locale: locale)
        case "opencode":   return L10n.t("Connect the Go plan in OpenCode to read your usage", locale: locale)
        case "commandcode": return L10n.t("Sign in with the Command Code app to read your usage", locale: locale)
        case "kiro":       return L10n.t("Sign in with kiro-cli to read your usage", locale: locale)
        // Two Ollamas, and they are stuck for different reasons: the hosted
        // one wants a key, the local one wants the daemon running.
        case "ollama":       return L10n.t("Enter an Ollama API key in Settings, or export OLLAMA_API_KEY", locale: locale)
        case "ollama-local": return L10n.t("Start Ollama to monitor your local models", locale: locale)
        case "lmstudio":     return L10n.t("Start LM Studio's server to monitor your local models", locale: locale)
        default:           return L10n.t("Sign in to \(displayName) to read your usage", locale: locale)
        }
    }

    /// What the tooltip says instead of limit rows when there is nothing to show.
    var statusMessage: String? {
        if kind == .localRuntime {
            if localModel != nil { return nil }
            if let localRuntime {
                return localRuntime.models.isEmpty ? localRuntime.summary : nil
            }
            if case .error(let why) = status { return why }
            return "Connecting to \(displayName)…"
        }
        if hasReading { return nil }
        let locale = L10n.locale
        switch status {
        case .needsAuth:      return authPrompt
        case .signedOutByOwner:
            // Names the cause, because "sign in again" on its own invites the
            // reasonable conclusion that this app lost the login.
            return L10n.t("Claude Code emptied this profile's saved login — it does that to every profile at once after it updates itself. Sign in again to \(displayName) to read your usage.", locale: locale)
        case .accessDenied:
            // Says what happened and what fixes it. "Sign in to Claude Code"
            // would send someone who *is* signed in to fix the wrong thing.
            // Points at the one control that asks again. Clicking the ring
            // only refreshes, and a refresh never shows the dialogue — polls
            // are not allowed to.
            return L10n.t("macOS refused Provider Monitor access to \(displayName)'s saved login. Use Allow access… in Settings to ask again.", locale: locale)
        case .unsupported(let why): return why
        case .error(let why): return L10n.t("Couldn't read usage — \(why)", locale: locale)
        case .stale, .ok:     return L10n.t("Waiting for the first reading…", locale: locale)
        }
    }
}
