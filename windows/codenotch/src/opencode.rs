//! OpenCode Go usage adapter, implemented from the upstream Codenotch's documented behaviour
//! (`Sources/Providers/OpenCode*.swift`).
//!
//! Credential: `~/.local/share/opencode/auth.json`, one entry per connected account. The
//! `opencode-go` entry (`{"type":"api","key":...}`) is the Go plan's own API key and authenticates
//! the usage endpoint directly — no workspace id, no cookie. Any other entry (`openai`, `google`, …)
//! is a different vendor's key, and reading one would show the wrong account under OpenCode's name.
//! Re-read on every fetch: this is an ordinary file, not a keychain item, so reading it prompts no
//! one.
//!
//! Endpoint: `GET https://opencode.ai/zen/go/v1/usage`, `Authorization: Bearer <key>`
//! Reply:
//! ```text
//! {"usage":{
//!   "rolling":{"status":"ok","percent":0,"resetsAt":"..."},
//!   "weekly": {"status":"ok","percent":0,"resetsAt":"..."},
//!   "monthly":{"status":"ok","percent":0,"resetsAt":"..."}}}
//! ```
//! `percent` is already *used*, matching the OpenCode dashboard's "X% used" — no inversion needed.
//! A valid key with no Go plan answers 401, the same as a bad key; a key not entitled to Go answers
//! 403, which is readable but meters nothing (not an error). Zen pay-as-you-go credit has no API at
//! all, so this covers the Go windows only.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://opencode.ai/zen/go/v1/usage";
const POLL_SECS: u64 = 300;
const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_CAP_SECS: u64 = 900;

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_refresh() {
    REFRESH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn auth_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".local").join("share").join("opencode").join("auth.json"))
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("opencode.json")
}

pub fn load_persisted() -> UsageSnapshot {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|t| serde_json::from_str::<UsageSnapshot>(&t).ok())
        .map(|mut s| {
            if !s.windows.is_empty() {
                s.status = "stale".into();
            }
            s
        })
        .unwrap_or_default()
}

fn persist(s: &UsageSnapshot) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(store_path(), t);
    }
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn load_token() -> Option<String> {
    let text = std::fs::read_to_string(auth_path()?).ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = root.get("opencode-go")?;
    if let Some(s) = nonempty(entry.as_str()) {
        return Some(s);
    }
    let obj = entry.as_object()?;
    ["key", "apiKey", "api_key", "token", "accessToken"]
        .iter()
        .find_map(|k| nonempty(obj.get(*k).and_then(|x| x.as_str())))
}

pub fn present() -> bool {
    load_token().is_some()
}

/// For doctor: contains no secret values
pub fn probe() -> String {
    let Some(p) = auth_path() else { return "OpenCode: cannot locate home directory".into() };
    if !p.is_file() {
        return format!("OpenCode: {} not found (not installed, or Go not connected)", p.display());
    }
    match load_token() {
        Some(_) => "OpenCode: opencode-go key found".into(),
        None => format!("OpenCode: {} exists but has no opencode-go entry (connect Go with `opencode auth login`)", p.display()),
    }
}

enum FetchErr {
    NeedsAuth,
    NothingMetered(String),
    RateLimited(u64),
    Other(String),
}

fn retry_after_secs(resp: &ureq::Response) -> Option<u64> {
    crate::polling::retry_after_hint_secs(resp.header("retry-after"), now_ms())
}

/// A minute, doubling per consecutive limit, is the floor under any server hint.
fn backoff_secs(consecutive: u32, retry_after: Option<u64>) -> u64 {
    let doubled = BACKOFF_BASE_SECS.saturating_mul(1u64 << consecutive.min(4));
    crate::polling::backoff_secs(doubled.clamp(BACKOFF_BASE_SECS, BACKOFF_CAP_SECS), retry_after)
}

fn fetch_usage(token: &str, consecutive_429: u32) -> Result<serde_json::Value, FetchErr> {
    let resp = ureq::get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) => r.into_json().map_err(|e| FetchErr::Other(format!("parse: {e}"))),
        Err(ureq::Error::Status(401, _)) => Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(403, _)) => Err(FetchErr::NothingMetered("No OpenCode Go subscription on this key".into())),
        Err(ureq::Error::Status(429, r)) => {
            Err(FetchErr::RateLimited(backoff_secs(consecutive_429, retry_after_secs(&r))))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(e) => Err(FetchErr::Other(format!("{e}"))),
    }
}

fn parse_reset(v: Option<&serde_json::Value>) -> Option<u64> {
    v.and_then(|x| x.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis().max(0) as u64)
}

fn windows_from_usage(v: &serde_json::Value) -> Vec<LimitWindow> {
    let Some(usage) = v.get("usage") else { return Vec::new() };
    [("rolling", "5h limit"), ("weekly", "Weekly limit"), ("monthly", "Monthly limit")]
        .iter()
        .filter_map(|(id, label)| {
            let entry = usage.get(*id)?;
            let percent = entry.get("percent")?.as_f64()?;
            Some(LimitWindow {
                id: (*id).into(),
                label: (*label).into(),
                used: (percent / 100.0).clamp(0.0, 1.0),
                resets_at: parse_reset(entry.get("resetsAt")),
                ..Default::default()
            })
        })
        .collect()
}

static CONSECUTIVE_429: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn read_once(prev: &UsageSnapshot) -> UsageSnapshot {
    let mut snap = prev.clone();
    let now = now_ms();
    if snap.backoff_until > now {
        snap.note = format!("Rate limited — retrying in {}s", (snap.backoff_until - now) / 1000);
        return snap;
    }
    let Some(token) = load_token() else {
        snap.status = "needsAuth".into();
        snap.note = "Connect Go inside OpenCode (opencode auth login) and the notch reads it.".into();
        return snap;
    };
    let attempt = CONSECUTIVE_429.load(std::sync::atomic::Ordering::Relaxed);
    match fetch_usage(&token, attempt) {
        Ok(v) => {
            let windows = windows_from_usage(&v);
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.fetched_at = now_ms();
            snap.backoff_until = 0;
            if windows.is_empty() {
                snap.status = "none".into();
                snap.windows.clear();
                snap.note = "OpenCode reported no Go usage windows".into();
            } else {
                snap.status = "ok".into();
                snap.windows = windows;
                snap.note = "Go · via OpenCode".into();
            }
        }
        Err(FetchErr::NeedsAuth) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = "needsAuth".into();
            snap.note = "OpenCode key rejected — connect Go again (opencode auth login)".into();
        }
        Err(FetchErr::NothingMetered(msg)) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = "none".into();
            snap.windows.clear();
            snap.note = msg;
        }
        Err(FetchErr::RateLimited(secs)) => {
            CONSECUTIVE_429.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            snap.backoff_until = crate::polling::deadline_ms(now_ms(), secs);
            if !snap.windows.is_empty() {
                snap.status = "stale".into();
            }
            snap.note = format!("Rate limited, retrying in {secs}s");
        }
        Err(FetchErr::Other(msg)) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = if snap.windows.is_empty() { "error" } else { "stale" }.into();
            snap.note = msg;
        }
    }
    snap
}

fn broadcast(app: &AppHandle, snap: UsageSnapshot) {
    let st = app.state::<AppState>();
    *st.opencode.lock().unwrap() = snap.clone();
    persist(&snap);
    let _ = app.emit("opencode", &snap);
}

fn sleep_interruptible(secs: u64) {
    for _ in 0..secs {
        if REFRESH.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        {
            let st = app.state::<AppState>();
            let snap = st.opencode.lock().unwrap().clone();
            let _ = app.emit("opencode", &snap);
        }
        if !present() {
            broadcast(&app, UsageSnapshot { status: "absent".into(), ..Default::default() });
            loop {
                sleep_interruptible(600); // OpenCode Go is not connected: look again every 10 minutes
                if present() {
                    break;
                }
            }
        }
        loop {
            let prev = {
                let st = app.state::<AppState>();
                let s = st.opencode.lock().unwrap().clone();
                s
            };
            let snap = read_once(&prev);
            let deadline = snap.backoff_until;
            if snap.status == "error" || snap.status == "stale" {
                crate::applog(&format!("opencode: {}", snap.note));
            }
            broadcast(&app, snap);
            sleep_interruptible(crate::polling::poll_wait_secs(POLL_SECS, deadline, now_ms()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_retry_hint_never_shortens_local_exponential_delay() {
        assert_eq!(backoff_secs(3, None), 480);
        assert_eq!(backoff_secs(3, Some(2)), 480);
    }

    fn windows(json: &str) -> Vec<LimitWindow> {
        windows_from_usage(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn rolling_weekly_monthly_are_read_in_order() {
        let ws = windows(
            r#"{"usage":{
              "rolling":{"status":"ok","percent":10,"resetsAt":"2026-09-06T12:31:06.611Z"},
              "weekly": {"status":"ok","percent":20,"resetsAt":"2026-09-07T00:00:00.611Z"},
              "monthly":{"status":"ok","percent":30,"resetsAt":"2026-10-03T13:09:45.611Z"}}}"#,
        );
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].id, "rolling");
        assert_eq!(ws[1].id, "weekly");
        assert_eq!(ws[2].id, "monthly");
        assert!((ws[0].used - 0.10).abs() < 1e-9);
        assert!((ws[1].used - 0.20).abs() < 1e-9);
        assert!((ws[2].used - 0.30).abs() < 1e-9);
        assert!(ws[0].resets_at.is_some());
    }

    #[test]
    fn a_zero_percent_reading_is_kept_not_dropped() {
        let ws = windows(r#"{"usage":{"rolling":{"status":"ok","percent":0,"resetsAt":"2026-09-06T12:31:06.611Z"}}}"#);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].used, 0.0);
    }

    #[test]
    fn missing_usage_object_is_unmetered() {
        assert!(windows(r#"{}"#).is_empty());
    }

    #[test]
    fn the_opencode_go_entry_accepts_either_shape() {
        assert_eq!(nonempty(Some("  raw-key  ")), Some("raw-key".to_string()));
        assert_eq!(nonempty(Some("")), None);
    }
}
