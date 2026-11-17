//! Small, shared timing rules for provider polling. All deadlines are epoch milliseconds.

/// npm installs expose .cmd launchers; CreateProcess cannot execute those directly.
pub(crate) fn cli_command(cli: &std::path::Path) -> std::process::Command {
    if cli.extension().is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("cmd")) {
        let mut command=std::process::Command::new("cmd.exe");
        command.args(["/d","/c"]).arg(cli);
        command
    } else { std::process::Command::new(cli) }
}

pub(crate) fn wait_for_renewal(child: &mut std::process::Child, timeout: std::time::Duration) -> std::io::Result<()> {
    let deadline=std::time::Instant::now()+timeout;
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() >= deadline {
            #[cfg(windows)] {
                use std::os::windows::process::CommandExt;
                let _=std::process::Command::new("taskkill.exe")
                    .args(["/PID",&child.id().to_string(),"/T","/F"])
                    .creation_flags(0x0800_0000)
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            }
            let _=child.kill();
            let _=child.wait();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

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
        // A network outage or unavailable CLI must not disable renewal for the
        // rest of the app's lifetime. After a failed burst, retry at most hourly.
        if self.attempts >= 3 && !self.finished
            && self.last_attempt.is_some_and(|last| now.saturating_sub(last) >= cooldown_ms.max(3_600_000)) {
            self.attempts = 0;
            self.last_attempt = None;
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
    fn npm_cli_launcher_uses_command_processor_without_changing_native_launchers() {
        let command=cli_command(std::path::Path::new("claude.cmd"));
        assert_eq!(command.get_program(),"cmd.exe");
        assert_eq!(command.get_args().map(|x|x.to_str().unwrap()).collect::<Vec<_>>(),vec!["/d","/c","claude.cmd"]);
        assert_eq!(cli_command(std::path::Path::new("grok.exe")).get_program(),"grok.exe");
    }

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

    #[test]
    fn exhausted_renewal_recovers_after_outage_without_restart_or_new_expiry() {
        let mut budget=RenewalBudget::default();
        for now in [100,700,1300] { assert!(budget.claim(Some(100),now,true,600)); }
        assert!(!budget.claim(Some(100),3_601_299,true,600));
        assert!(budget.claim(Some(100),3_601_300,true,600));
        assert!(!budget.claim(Some(100),3_601_301,true,600));
        budget.finish_for_current_expiry();
        assert!(!budget.claim(Some(100),9_000_000,true,600));
    }
}
