//! Kimi Code usage adapter, ported from `Sources/Providers/Kimi*.swift`.
//!
//! Kimi Code owns and refreshes the OAuth credential under
//! `~/.kimi-code/credentials/kimi-code.json` (or `%KIMI_CODE_HOME%/credentials/kimi-code.json`).
//! Provider Monitor reads that file only and never logs or persists either token.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://api.kimi.com/coding/v1/usages";
const POLL_SECS: u64 = 300;
const BACKOFF_MIN_SECS: u64 = 60;

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_refresh() {
    REFRESH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn auth_path_from(home: Option<&Path>, override_root: Option<&str>) -> Option<PathBuf> {
    let root = nonempty(override_root)
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".kimi-code")))?;
    Some(root.join("credentials").join("kimi-code.json"))
}

fn auth_path() -> Option<PathBuf> {
    let home = dirs::home_dir();
    auth_path_from(
        home.as_deref(),
        std::env::var("KIMI_CODE_HOME").ok().as_deref(),
    )
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("kimi.json")
}

pub fn load_persisted() -> UsageSnapshot {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|text| serde_json::from_str::<UsageSnapshot>(&text).ok())
        .map(|mut snapshot| {
            if !snapshot.windows.is_empty() {
                snapshot.status = "stale".into();
            }
            snapshot
        })
        .unwrap_or_default()
}

fn persist(snapshot: &UsageSnapshot) {
    if let Ok(text) = serde_json::to_string_pretty(snapshot) {
        let _ = std::fs::write(store_path(), text);
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Debug, PartialEq)]
struct Credential {
    access_token: String,
    /// Epoch milliseconds.
    expires_at: u64,
}

fn load_credential_from(path: &Path) -> Option<Credential> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let access_token =
        nonempty(root.get("access_token").and_then(|value| value.as_str()))?.to_string();
    let expires_seconds = root.get("expires_at")?.as_f64()?;
    if expires_seconds <= 0.0 {
        return None;
    }
    Some(Credential {
        access_token,
        expires_at: (expires_seconds * 1000.0) as u64,
    })
}

fn load_credential() -> Option<Credential> {
    load_credential_from(&auth_path()?)
}

pub fn present() -> bool {
    auth_path().map(|path| path.is_file()).unwrap_or(false)
}

/// For doctor output; deliberately contains no token or token fragment.
pub fn probe() -> String {
    let Some(path) = auth_path() else {
        return "Kimi: cannot locate KIMI_CODE_HOME or the home directory".into();
    };
    if !path.is_file() {
        return format!(
            "Kimi: {} not found (Kimi Code not installed, or not signed in)",
            path.display()
        );
    }
    match load_credential_from(&path) {
        Some(credential) if credential.expires_at <= now_ms() => {
            "Kimi: credential found but expired; run kimi and /login".into()
        }
        Some(_) => "Kimi: credential usable".into(),
        None => format!("Kimi: {} has no usable OAuth session", path.display()),
    }
}

enum FetchErr {
    NeedsAuth,
    NothingMetered,
    RateLimited(u64),
    Other(String),
}

fn retry_after_secs(response: &ureq::Response) -> u64 {
    crate::polling::retry_after_secs(response.header("retry-after"), now_ms(), BACKOFF_MIN_SECS)
}

fn fetch_usage(token: &str) -> Result<serde_json::Value, FetchErr> {
    let response = ureq::get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call();
    match response {
        Ok(response) => response
            .into_json()
            .map_err(|error| FetchErr::Other(format!("parse: {error}"))),
        Err(ureq::Error::Status(401 | 403, _)) => Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(404, _)) => Err(FetchErr::NothingMetered),
        Err(ureq::Error::Status(429, response)) => {
            Err(FetchErr::RateLimited(retry_after_secs(&response)))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(error) => Err(FetchErr::Other(error.to_string())),
    }
}

fn count(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse::<i64>().ok())
}

fn parse_reset(value: Option<&serde_json::Value>) -> Option<u64> {
    value
        .and_then(serde_json::Value::as_str)
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .map(|date| date.timestamp_millis().max(0) as u64)
}

fn usage_row(id: &str, label: &str, detail: &serde_json::Value) -> Option<LimitWindow> {
    let used_count = count(detail.get("used"))?;
    let resets_at = parse_reset(detail.get("resetTime"));
    match count(detail.get("limit")) {
        Some(limit) if limit > 0 => Some(LimitWindow {
            id: id.into(),
            label: label.into(),
            used: used_count as f64 / limit as f64,
            resets_at,
            ..Default::default()
        }),
        _ => Some(LimitWindow {
            id: id.into(),
            label: label.into(),
            count: Some(used_count),
            resets_at,
            ..Default::default()
        }),
    }
}

fn window_kind(window: &serde_json::Value) -> Option<(&'static str, &'static str)> {
    let duration = count(window.get("duration"))?;
    let unit = window.get("timeUnit")?.as_str()?;
    match (unit, duration) {
        ("TIME_UNIT_MINUTE", minutes) if minutes % 60 == 0 && minutes / 60 == 5 => {
            Some(("rolling", "5h limit"))
        }
        ("TIME_UNIT_HOUR", 5) => Some(("rolling", "5h limit")),
        ("TIME_UNIT_WEEK", 1) => Some(("weekly", "Weekly limit")),
        _ => None,
    }
}

fn membership_plan(root: &serde_json::Value) -> Option<String> {
    let level = root
        .get("user")?
        .get("membership")?
        .get("level")?
        .as_str()?;
    let name = level.strip_prefix("LEVEL_").unwrap_or(level).trim();
    if name.is_empty() {
        return None;
    }
    let lowercase = name.to_ascii_lowercase();
    let mut characters = lowercase.chars();
    let first = characters.next()?.to_ascii_uppercase();
    Some(format!("{first}{}", characters.as_str()))
}

fn windows_from_usage(root: &serde_json::Value) -> (Vec<LimitWindow>, Option<String>) {
    let mut windows = Vec::new();
    if let Some(summary) = root.get("usage") {
        if let Some(window) = usage_row("weekly", "Weekly limit", summary) {
            windows.push(window);
        }
    }
    if let Some(limits) = root.get("limits").and_then(serde_json::Value::as_array) {
        for entry in limits {
            let Some((id, label)) = entry.get("window").and_then(window_kind) else {
                continue;
            };
            if let Some(window) = entry
                .get("detail")
                .and_then(|detail| usage_row(id, label, detail))
            {
                windows.push(window);
            }
        }
    }
    (windows, membership_plan(root))
}

fn read_once(previous: &UsageSnapshot) -> UsageSnapshot {
    let mut snapshot = previous.clone();
    let now = now_ms();
    if snapshot.backoff_until > now {
        snapshot.note = format!(
            "Rate limited — retrying in {}s",
            (snapshot.backoff_until - now) / 1000
        );
        return snapshot;
    }
    let Some(credential) = load_credential() else {
        snapshot.status = "needsAuth".into();
        snapshot.note =
            "Run kimi and sign in with /login — Provider Monitor only reads its token.".into();
        return snapshot;
    };
    if credential.expires_at <= now {
        snapshot.status = "needsAuth".into();
        snapshot.note = "Kimi sign-in expired — run kimi and /login again".into();
        return snapshot;
    }

    match fetch_usage(&credential.access_token) {
        Ok(value) => {
            let (windows, plan) = windows_from_usage(&value);
            snapshot.fetched_at = now_ms();
            snapshot.backoff_until = 0;
            if windows.is_empty() {
                snapshot.status = "none".into();
                snapshot.windows.clear();
                snapshot.note = "No Kimi Code usage limits on this account".into();
            } else {
                snapshot.status = "ok".into();
                snapshot.windows = windows;
                snapshot.note = plan.unwrap_or_else(|| "Kimi Code".into());
            }
        }
        Err(FetchErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note = "Kimi rejected its sign-in — run kimi and /login again".into();
        }
        Err(FetchErr::NothingMetered) => {
            snapshot.status = "none".into();
            snapshot.windows.clear();
            snapshot.note = "No Kimi Code plan on this account".into();
        }
        Err(FetchErr::RateLimited(seconds)) => {
            snapshot.backoff_until = crate::polling::deadline_ms(now_ms(), seconds);
            snapshot.status = if snapshot.windows.is_empty() {
                "backoff"
            } else {
                "stale"
            }
            .into();
            snapshot.note = format!("Rate limited — retrying in {seconds}s");
        }
        Err(FetchErr::Other(message)) => {
            snapshot.status = if snapshot.windows.is_empty() {
                "error"
            } else {
                "stale"
            }
            .into();
            snapshot.note = message;
        }
    }
    snapshot
}

fn broadcast(app: &AppHandle, snapshot: UsageSnapshot) {
    let state = app.state::<AppState>();
    *state.kimi.lock().unwrap() = snapshot.clone();
    persist(&snapshot);
    let _ = app.emit("kimi", &snapshot);
}

fn sleep_interruptible(seconds: u64) {
    for _ in 0..seconds {
        if REFRESH.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        {
            let state = app.state::<AppState>();
            let snapshot = state.kimi.lock().unwrap().clone();
            let _ = app.emit("kimi", &snapshot);
        }
        if !present() {
            broadcast(
                &app,
                UsageSnapshot {
                    status: "absent".into(),
                    ..Default::default()
                },
            );
            loop {
                sleep_interruptible(600);
                if present() {
                    break;
                }
            }
        }
        loop {
            let previous = {
                let state = app.state::<AppState>();
                let snapshot = state.kimi.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            let deadline = snapshot.backoff_until;
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("kimi: {}", snapshot.note));
            }
            broadcast(&app, snapshot);
            sleep_interruptible(crate::polling::poll_wait_secs(POLL_SECS, deadline, now_ms()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PAYLOAD: &str = r#"{
      "user":{"membership":{"level":"LEVEL_ADVANCED"}},
      "usage":{"limit":"100","used":"2","resetTime":"2026-09-15T19:39:34.389610Z"},
      "limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                 "detail":{"limit":"100","used":"8","resetTime":"2026-09-11T16:39:34.389610Z"}}]
    }"#;

    fn temporary_file(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "provider-monitor-kimi-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn reads_weekly_summary_five_hour_window_and_plan() {
        let root: serde_json::Value = serde_json::from_str(PAYLOAD).unwrap();
        let (windows, plan) = windows_from_usage(&root);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["weekly", "rolling"]
        );
        assert!((windows[0].used - 0.02).abs() < 1e-9);
        assert!((windows[1].used - 0.08).abs() < 1e-9);
        assert!(windows[0].resets_at.is_some());
        assert_eq!(plan.as_deref(), Some("Advanced"));
    }

    #[test]
    fn unknown_window_is_dropped_and_count_only_summary_is_kept() {
        let root = serde_json::json!({
            "usage": {"used": "2"},
            "limits": [{"window": {"duration": 1, "timeUnit": "TIME_UNIT_DAY"}, "detail": {"used": "1", "limit": "10"}}]
        });
        let (windows, _) = windows_from_usage(&root);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "weekly");
        assert_eq!(windows[0].count, Some(2));
    }

    #[test]
    fn credential_requires_token_and_epoch_seconds_expiry() {
        let path = temporary_file(r#"{"access_token":"k-live","expires_at":1789139960}"#);
        let credential = load_credential_from(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(credential.access_token, "k-live");
        assert_eq!(credential.expires_at, 1_789_139_960_000);

        let missing_expiry = temporary_file(r#"{"access_token":"k-live"}"#);
        assert!(load_credential_from(&missing_expiry).is_none());
        fs::remove_file(missing_expiry).unwrap();
    }

    #[test]
    fn kimi_code_home_override_is_windows_friendly() {
        let home = Path::new(r"C:\Users\tester");
        assert_eq!(
            auth_path_from(Some(home), Some(r"D:\KimiData")),
            Some(
                PathBuf::from(r"D:\KimiData")
                    .join("credentials")
                    .join("kimi-code.json")
            )
        );
        assert_eq!(
            auth_path_from(Some(home), None),
            Some(
                home.join(".kimi-code")
                    .join("credentials")
                    .join("kimi-code.json")
            )
        );
    }

    #[test]
    fn empty_payload_has_no_metered_windows() {
        let (windows, plan) = windows_from_usage(&serde_json::json!({"usage": {}, "limits": []}));
        assert!(windows.is_empty());
        assert!(plan.is_none());
    }
}
