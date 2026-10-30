import Foundation

/// Retry-After is a minimum deadline. Parsing and display round upward so a
/// fractional second never wakes a request just before the provider allows it.
enum UsageRetryPolicy {
    static func retryAfter(header: String?, at now: Date = Date()) -> TimeInterval? {
        guard let header = header?.trimmingCharacters(in: .whitespacesAndNewlines),
              !header.isEmpty else { return nil }
        if let seconds = TimeInterval(header), seconds.isFinite, seconds >= 0,
           seconds < Double(UInt64.max) {
            return ceil(seconds)
        }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        guard let date = formatter.date(from: header) else { return nil }
        return remaining(until: date, at: now)
    }

    static func retryAfter(from response: URLResponse?, at now: Date = Date()) -> TimeInterval? {
        retryAfter(header: (response as? HTTPURLResponse)?.value(forHTTPHeaderField: "Retry-After"), at: now)
    }

    static func remaining(until deadline: Date, at now: Date = Date()) -> TimeInterval {
        ceil(max(0, deadline.timeIntervalSince(now)))
    }

    static func backoff(attempt: Int, retryAfter: TimeInterval?) -> TimeInterval {
        let local = min(15 * 60, 60 * pow(2, Double(min(max(0, attempt), 4))))
        return max(local, retryAfter ?? 0)
    }
}
