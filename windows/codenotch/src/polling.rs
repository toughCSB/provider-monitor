//! Small, shared timing rules for provider polling. All deadlines are epoch milliseconds.

pub fn retry_after_secs(header: Option<&str>, now_ms: u64, fallback_secs: u64) -> u64 {
    retry_after_hint_secs(header, now_ms).unwrap_or(fallback_secs)
}

pub fn retry_after_hint_secs(header: Option<&str>, now_ms: u64) -> Option<u64> {
    let value = header.map(str::trim).filter(|s| !s.is_empty())?;
    let max_secs = u64::MAX.saturating_sub(now_ms) / 1_000;
    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() && seconds > 0.0 && seconds.ceil() < u64::MAX as f64 {
            let rounded = seconds.ceil() as u64;
            return (rounded <= max_secs).then_some(rounded);
        }
        return None;
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| remaining_secs(date.timestamp_millis().max(0) as u64, now_ms))
        .filter(|seconds| *seconds > 0 && *seconds <= max_secs)
}

pub fn deadline_ms(now_ms: u64, seconds: u64) -> u64 {
    now_ms.saturating_add(seconds.saturating_mul(1_000))
}

pub fn remaining_secs(deadline_ms: u64, now_ms: u64) -> u64 {
    deadline_ms.saturating_sub(now_ms).saturating_add(999) / 1000
}

pub fn poll_wait_secs(period_secs: u64, deadline_ms: u64, now_ms: u64) -> u64 {
    if deadline_ms > now_ms { remaining_secs(deadline_ms, now_ms) } else { period_secs }
}

/// Keep the local cooldown as a floor; a longer server hint is never capped by it.
pub fn backoff_secs(local_secs: u64, retry_after: Option<u64>) -> u64 {
    local_secs.max(retry_after.unwrap_or(0))
}

#[derive(Default)]
pub struct RenewalBudget {
    expiry: Option<u64>,
    attempts: u8,
    last_attempt: Option<u64>,
    finished: bool,
}

impl RenewalBudget {
    pub fn claim(&mut self, expiry: Option<u64>, now: u64, eligible: bool, cooldown_ms: u64) -> bool {
        let Some(expiry) = expiry else { return false };
        if self.expiry != Some(expiry) {
            self.expiry = Some(expiry);
            self.attempts = 0;
            self.last_attempt = None;
            self.finished = false;
        }
        if !eligible || self.finished || self.attempts >= 3 || self.last_attempt.is_some_and(|last| now.saturating_sub(last) < cooldown_ms) {
            return false;
        }
        self.attempts += 1;
        self.last_attempt = Some(now);
        true
    }

    pub fn finish_for_current_expiry(&mut self) {
        self.finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_hint_is_exact_and_deadline_rounds_up() {
        assert_eq!(retry_after_secs(Some("0.25"), 1_000, 60), 1);
        assert_eq!(retry_after_secs(Some("2"), 1_000, 60), 2);
        assert_eq!(retry_after_secs(None, 1_000, 60), 60);
        assert_eq!(retry_after_secs(Some("0"), 1_000, 60), 60);
        assert_eq!(retry_after_hint_secs(Some("0"), 1_000), None);
        assert_eq!(retry_after_hint_secs(Some("invalid"), 1_000), None);
        assert_eq!(retry_after_hint_secs(Some("18446744073709551616"), 1_000), None);
        assert_eq!(deadline_ms(u64::MAX - 10, 60), u64::MAX);
        assert_eq!(backoff_secs(900, Some(2)), 900);
        assert_eq!(backoff_secs(900, Some(0)), 900);
        assert_eq!(backoff_secs(900, Some(3_600)), 3_600);
        assert_eq!(poll_wait_secs(300, 2_001, 1_002), 1);
        assert_eq!(poll_wait_secs(300, 2_001, 1_001), 1);
        assert_eq!(poll_wait_secs(300, 2_001, 1_000), 2);
    }

    #[test]
    fn shared_policy_fixture() {
        let cases: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/polling_policy.json")).unwrap();
        for case in cases["retry_after"].as_array().unwrap() {
            assert_eq!(
                retry_after_secs(case["header"].as_str(), case["now_ms"].as_u64().unwrap(), case["fallback_secs"].as_u64().unwrap()),
                case["expected_secs"].as_u64().unwrap(),
                "{case}"
            );
        }
        for case in cases["deadline"].as_array().unwrap() {
            assert_eq!(
                remaining_secs(case["deadline_ms"].as_u64().unwrap(), case["now_ms"].as_u64().unwrap()),
                case["expected_secs"].as_u64().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn renewal_is_bounded_per_expiry_and_resets_on_change() {
        let mut budget = RenewalBudget::default();
        assert!(budget.claim(Some(100), 100, true, 600));
        assert!(!budget.claim(Some(100), 699, true, 600));
        assert!(budget.claim(Some(100), 700, true, 600));
        assert!(budget.claim(Some(100), 1300, true, 600));
        assert!(!budget.claim(Some(100), 1900, true, 600));
        assert!(budget.claim(Some(200), 1900, true, 600));
    }

    #[test]
    fn successful_renewal_finishes_old_expiry_until_it_changes() {
        let mut budget = RenewalBudget::default();
        assert!(budget.claim(Some(100), 100, true, 600));
        budget.finish_for_current_expiry();
        assert!(!budget.claim(Some(100), 700, true, 600));
        assert!(!budget.claim(Some(100), 1_300, true, 600));
        assert!(budget.claim(Some(200), 1_300, true, 600));
    }
}
