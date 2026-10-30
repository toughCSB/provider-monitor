import Foundation

/// Bounds unattended CLI renewal attempts for one observed credential expiry.
/// A changed expiry starts a fresh budget; a successful after-check also blocks
/// repeat launches while a provider is still serving the old cached expiry.
struct TokenRenewalPolicy {
    static let maxAttempts = 3

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
        guard expiry.timeIntervalSince(now) < margin,
              !finished, attempts < Self.maxAttempts else { return false }
        if let lastAttempt, now.timeIntervalSince(lastAttempt) < cooldown { return false }
        return true
    }

    mutating func recordAttempt(at now: Date) {
        attempts += 1
        lastAttempt = now
    }

    mutating func finishForCurrentExpiry() {
        finished = true
    }
}
