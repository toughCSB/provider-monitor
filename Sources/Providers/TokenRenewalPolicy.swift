import Foundation

/// Bounds unattended CLI renewal attempts for one observed credential expiry.
/// A changed expiry starts a fresh budget; a successful after-check also blocks
/// repeat launches while a provider is still serving the old cached expiry.
struct TokenRenewalPolicy {
    static let maxAttempts = 3
    /// A failed burst is not a permanent verdict on an unchanged expiry.
    static let exhaustedRetryDelay: TimeInterval = 60 * 60

    private var observedExpiry: Date?
    private var attempts = 0
    private var lastAttempt: Date?
    private var finished = false

    mutating func shouldAttempt(expiry: Date?, now: Date,
                                margin: TimeInterval, cooldown: TimeInterval) -> Bool {
        guard let expiry else { return false }
        if observedExpiry != expiry {
            observedExpiry = expiry
            attempts = 0
            lastAttempt = nil
            finished = false
        }
        guard expiry.timeIntervalSince(now) < margin, !finished else { return false }
        if attempts >= Self.maxAttempts {
            guard let lastAttempt,
                  now.timeIntervalSince(lastAttempt) >= Self.exhaustedRetryDelay else { return false }
            attempts = 0
        }
        if let lastAttempt, now.timeIntervalSince(lastAttempt) < cooldown { return false }
        return true
    }

    mutating func recordAttempt(at now: Date) {
        attempts += 1
        lastAttempt = now
    }

    /// No CLI was available: check again after an hour, without three pointless
    /// ten-minute checks against the same missing executable.
    mutating func deferUnavailable(at now: Date) {
        attempts = Self.maxAttempts
        lastAttempt = now
    }

    mutating func finishForCurrentExpiry() {
        finished = true
    }
}
