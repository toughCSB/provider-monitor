//! Grok usage adapter, implemented from the upstream Codenotch's documented behaviour
//! (`Sources/Providers/Grok*.swift`).
//!
//! Credential: `~/.grok/auth.json`, written by the Grok CLI's own sign-in (`grok login`) through
//! `auth.x.ai`. The file is keyed by `issuer::client_id`; only an entry whose key starts with
//! `https://auth.x.ai` (or whose `oidc_issuer` field says so) is trusted — Grok also supports a
//! customer IdP whose token is meant for a private proxy, and sending that to the public
//! `cli-chat-proxy.grok.com` would hand someone else's credential to the wrong endpoint. Read only,
//! never refreshed — that is the CLI's job.
//!
//! Endpoint: `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`
//! Headers: `Authorization: Bearer <token>`, `X-XAI-Token-Auth: xai-grok-cli`, `Accept: application/json`
//! Reply:
//! ```text
//! { "config": {
//!     "currentPeriod": {"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"...","end":"..."},
//!     "creditUsagePercent": 8.0,
//!     "productUsage": [{"product":"GrokBuild","usagePercent":8.0}],
//!     "billingPeriodStart": "...", "billingPeriodEnd": "..." } }
//! ```
//! `creditUsagePercent` is the weekly Grok Build allowance and the ring's number; `productUsage`
//! is the fallback when it is absent. A fresh weekly plan with neither field yet still gets a
//! "Weekly limit" row at 0%, mirroring what Grok's own `/usage` shows rather than reporting the
//! account as unmetered.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const POLL_SECS: u64 = 300; // Grok has no session state to key off, same fixed cadence as Codex
const BACKOFF_MIN_SECS: u64 = 60;
const TRUSTED_ISSUER: &str = "https://auth.x.ai";

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
    dirs::home_dir().map(|h| h.join(".grok").join("auth.json"))
}

fn present() -> bool {
    auth_path().map(|p| p.is_file()).unwrap_or(false)
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("grok.json")
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

struct Credential {
    access_token: String,
    expires_at: Option<u64>,
    email: Option<String>,
}

fn parse_date(v: Option<&serde_json::Value>) -> Option<u64> {
    v.and_then(|x| x.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis().max(0) as u64)
}

fn is_trusted(key: &str, entry: &serde_json::Value) -> bool {
    if key.split("::").next() == Some(TRUSTED_ISSUER) {
        return true;
    }
    entry.get("oidc_issuer").and_then(|x| x.as_str()) == Some(TRUSTED_ISSUER)
}

/// The file is keyed by `issuer::client_id`. One signed-in CLI is the ordinary case; if several
/// sit there, the one that is still live wins, otherwise the first trusted entry.
fn pick(root: &serde_json::Value) -> Option<serde_json::Value> {
    let obj = root.as_object()?;
    let trusted: Vec<&serde_json::Value> =
        obj.iter().filter(|(k, v)| is_trusted(k, v)).map(|(_, v)| v).collect();
    let now = now_ms();
    trusted
        .iter()
        .find(|e| parse_date(e.get("expires_at")).map(|exp| exp > now).unwrap_or(true))
        .or(trusted.first())
        .map(|v| (*v).clone())
}

fn load_credential() -> Option<Credential> {
    let text = std::fs::read_to_string(auth_path()?).ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = pick(&root)?;
    let token = entry.get("key").and_then(|x| x.as_str())?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some(Credential {
        access_token: token,
        expires_at: parse_date(entry.get("expires_at")),
        email: entry.get("email").and_then(|x| x.as_str()).map(String::from),
    })
}

/// For doctor: contains no secret values
pub fn probe() -> String {
    let Some(p) = auth_path() else { return "Grok: cannot locate home directory".into() };
    if !p.is_file() {
        return format!("Grok: {} not found (not installed, or not signed in)", p.display());
    }
    match load_credential() {
        Some(c) => {
            let expired = c.expires_at.map(|e| e <= now_ms()).unwrap_or(false);
            format!(
                "Grok: auth.json usable{}{}",
                if expired { " (token expired)" } else { "" },
                c.email.map(|e| format!(", account={e}")).unwrap_or_default()
            )
        }
        None => format!("Grok: {} present but has no trusted xAI session", p.display()),
    }
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn fetch_credits(token: &str) -> Result<serde_json::Value, FetchErr> {
    let resp = ureq::get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("X-XAI-Token-Auth", "xai-grok-cli")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) => r.into_json().map_err(|e| FetchErr::Other(format!("parse: {e}"))),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(429, r)) => {
            let ra = r.header("retry-after").and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
            Err(FetchErr::RateLimited(ra.max(BACKOFF_MIN_SECS)))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(e) => Err(FetchErr::Other(format!("{e}"))),
    }
}

/// "GrokBuild" → "Grok Build"
fn humanize(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_uppercase() && !out.is_empty() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn percent(v: Option<&serde_json::Value>) -> Option<f64> {
    v.and_then(|x| x.as_f64()).map(|p| (p / 100.0).clamp(0.0, 1.0))
}

fn windows_from_credits(v: &serde_json::Value) -> Vec<LimitWindow> {
    let mut out = Vec::new();
    let Some(config) = v.get("config") else { return out };

    let current_period = config.get("currentPeriod");
    let current_end = current_period.and_then(|p| parse_date(p.get("end")));
    let credits_reset = current_end.or_else(|| parse_date(config.get("billingPeriodEnd")));

    if let Some(fraction) = percent(config.get("creditUsagePercent")) {
        let label = config
            .get("productUsage")
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("product"))
            .and_then(|x| x.as_str())
            .map(humanize)
            .unwrap_or_else(|| "Grok Build".into());
        out.push(LimitWindow { id: "credits".into(), label, used: fraction, resets_at: credits_reset, ..Default::default() });
    } else if let Some(products) = config.get("productUsage").and_then(|x| x.as_array()) {
        for product in products {
            let Some(fraction) = percent(product.get("usagePercent")) else { continue };
            let name = product.get("product").and_then(|x| x.as_str()).map(humanize).unwrap_or_else(|| "Usage".into());
            let id = if out.is_empty() {
                "credits".to_string()
            } else {
                product.get("product").and_then(|x| x.as_str()).unwrap_or(&name).to_string()
            };
            out.push(LimitWindow { id, label: name, used: fraction, resets_at: credits_reset, ..Default::default() });
        }
    }

    // A weekly plan pool (X Premium+, SuperGrok) states its window in currentPeriod and omits the
    // percent fields until usage lands; mirror Grok's own "Weekly limit" bar at 0% rather than
    // reporting the account as unmetered.
    if out.is_empty() {
        if let Some(period) = current_period {
            let is_weekly = period.get("type").and_then(|x| x.as_str()).map(|s| s.contains("WEEKLY")).unwrap_or(false);
            if is_weekly {
                out.push(LimitWindow {
                    id: "credits".into(),
                    label: "Weekly limit".into(),
                    used: 0.0,
                    resets_at: parse_date(period.get("end")).or(credits_reset),
                    ..Default::default()
                });
            }
        }
    }
    out
}

fn auth_failure(mut snap: UsageSnapshot, note: &str) -> UsageSnapshot {
    // A failed credential does not erase a previously measured percentage. Keep
    // its timestamp so the notch can label the number as old, never as live.
    snap.status = if snap.windows.is_empty() { "needsAuth" } else { "stale" }.into();
    snap.note = note.into();
    snap
}

fn read_once(prev: &UsageSnapshot) -> UsageSnapshot {
    read_once_with(prev, load_credential, fetch_credits)
}

fn read_once_with<L, F>(prev: &UsageSnapshot, load: L, fetch: F) -> UsageSnapshot
where
    L: Fn() -> Option<Credential>,
    F: Fn(&str) -> Result<serde_json::Value, FetchErr>,
{
    let mut snap = prev.clone();
    let held_until = snap.backoff_until;
    let now = now_ms();
    if held_until > now {
        snap.note = format!("Rate limited — retrying in {}s", (held_until - now) / 1000);
        return snap;
    }
    let Some(cred) = load() else {
        return auth_failure(snap, "Run grok login — it signs in and refreshes the token this reads.");
    };
    if cred.expires_at.map(|e| e <= now).unwrap_or(false) {
        return auth_failure(snap, "Grok sign-in expired — run grok login again");
    }
    // Grok may rotate auth.json between our file read and the HTTP response.
    // Re-read once before declaring the session rejected, as Claude does.
    let result = match fetch(&cred.access_token) {
        Err(FetchErr::NeedsAuth) => match load() {
            Some(newer) if newer.access_token != cred.access_token
                && newer.expires_at.map(|e| e > now_ms()).unwrap_or(true) => fetch(&newer.access_token),
            _ => Err(FetchErr::NeedsAuth),
        },
        other => other,
    };
    match result {
        Ok(v) => {
            let windows = windows_from_credits(&v);
            snap.fetched_at = now_ms();
            snap.backoff_until = 0;
            if windows.is_empty() {
                snap.status = "none".into();
                snap.windows.clear();
                snap.note = "Grok has nothing metered on this account yet".into();
            } else {
                snap.status = "ok".into();
                snap.windows = windows;
                snap.note = String::new();
            }
        }
        Err(FetchErr::NeedsAuth) => {
            snap = auth_failure(snap, "Grok rejected its sign-in — run grok login again");
        }
        Err(FetchErr::RateLimited(secs)) => {
            snap.backoff_until = now_ms() + secs * 1000;
            if !snap.windows.is_empty() {
                snap.status = "stale".into();
            }
            snap.note = format!("Rate limited — retrying in {secs}s");
        }
        Err(FetchErr::Other(msg)) => {
            snap.status = if snap.windows.is_empty() { "error" } else { "stale" }.into();
            snap.note = msg;
        }
    }
    snap
}

fn broadcast(app: &AppHandle, snap: UsageSnapshot) {
    let st = app.state::<AppState>();
    *st.grok.lock().unwrap() = snap.clone();
    persist(&snap);
    let _ = app.emit("grok", &snap);
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
            let snap = st.grok.lock().unwrap().clone();
            let _ = app.emit("grok", &snap);
        }
        loop {
            let prev = {
                let st = app.state::<AppState>();
                let s = st.grok.lock().unwrap().clone();
                s
            };
            if !present() && prev.windows.is_empty() {
                broadcast(&app, UsageSnapshot { status: "absent".into(), ..Default::default() });
                sleep_interruptible(30);
                continue;
            }
            let snap = read_once(&prev);
            let hold = snap.backoff_until.saturating_sub(now_ms()) / 1000;
            if snap.status == "error" || snap.status == "stale" {
                crate::applog(&format!("grok: {}", snap.note));
            }
            let retry = if snap.status == "needsAuth" ||
                (snap.status == "stale" && snap.note.contains("Grok sign-in")) ||
                (snap.status == "stale" && snap.note.starts_with("Run grok login")) {
                30
            } else { POLL_SECS };
            broadcast(&app, snap);
            sleep_interruptible(retry.max(hold));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(json: &str) -> Vec<LimitWindow> {
        windows_from_credits(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn credit_usage_percent_is_the_headline() {
        let ws = windows(
            r#"{"config":{
              "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-09-05T08:21:18.802818+00:00","end":"2026-09-12T08:21:18.802818+00:00"},
              "creditUsagePercent":8.0,
              "productUsage":[{"product":"GrokBuild","usagePercent":8.0}],
              "billingPeriodStart":"2026-09-05T08:21:18.802818+00:00","billingPeriodEnd":"2026-09-12T08:21:18.802818+00:00"}}"#,
        );
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].id, "credits");
        assert_eq!(ws[0].label, "Grok Build");
        assert!((ws[0].used - 0.08).abs() < 1e-9);
        let expected = chrono::DateTime::parse_from_rfc3339("2026-09-12T08:21:18.802818+00:00")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(ws[0].resets_at, Some(expected));
    }

    #[test]
    fn product_usage_is_the_fallback_when_credit_usage_percent_is_absent() {
        let ws = windows(
            r#"{"config":{
              "productUsage":[{"product":"GrokBuild","usagePercent":40.0},{"product":"GrokChat","usagePercent":12.0}]}}"#,
        );
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].id, "credits");
        assert_eq!(ws[0].label, "Grok Build");
        assert_eq!(ws[1].id, "GrokChat");
        assert_eq!(ws[1].label, "Grok Chat");
    }

    #[test]
    fn a_fresh_weekly_plan_with_nothing_metered_yet_shows_zero_not_absent() {
        let ws = windows(
            r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-09-05T00:00:00Z","end":"2026-09-12T00:00:00Z"}}}"#,
        );
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].id, "credits");
        assert_eq!(ws[0].label, "Weekly limit");
        assert_eq!(ws[0].used, 0.0);
    }

    #[test]
    fn no_current_period_and_no_percent_is_unmetered() {
        assert!(windows(r#"{"config":{}}"#).is_empty());
    }

    #[test]
    fn only_a_trusted_xai_issuer_is_picked() {
        let root: serde_json::Value = serde_json::from_str(
            r#"{"https://auth.customer-idp.example::client1":{"key":"untrusted-token"},
                "https://auth.x.ai::client2":{"key":"trusted-token","email":"a@b.com"}}"#,
        )
        .unwrap();
        let entry = pick(&root).unwrap();
        assert_eq!(entry.get("key").and_then(|x| x.as_str()), Some("trusted-token"));
        assert!(!is_trusted("https://auth.x.ai.example.com::client", &serde_json::json!({})));
    }

    #[test]
    fn missing_credentials_keep_the_last_measurement_marked_stale() {
        let prev = UsageSnapshot {
            status: "ok".into(), fetched_at: 123,
            windows: vec![LimitWindow { id: "credits".into(), label: "Grok Build".into(),
                                        used: 0.21, resets_at: Some(456), ..Default::default() }],
            ..Default::default()
        };
        let next = read_once_with(&prev, || None, |_| unreachable!());
        assert_eq!(next.status, "stale");
        assert_eq!(next.windows[0].used, 0.21);
        assert_eq!(next.windows[0].resets_at, Some(456));
        assert_eq!(next.fetched_at, 123);
    }

    #[test]
    fn a_rotated_cli_token_is_retried_before_reconnect_warning() {
        let loads = std::cell::Cell::new(0);
        let next = read_once_with(
            &UsageSnapshot::default(),
            || {
                let n = loads.get(); loads.set(n + 1);
                Some(Credential { access_token: if n == 0 { "old" } else { "new" }.into(),
                                  expires_at: None, email: None })
            },
            |token| {
                if token == "old" { Err(FetchErr::NeedsAuth) }
                else { Ok(serde_json::json!({"config":{"creditUsagePercent":21.0}})) }
            },
        );
        assert_eq!(loads.get(), 2);
        assert_eq!(next.status, "ok");
        assert_eq!(next.windows[0].used, 0.21);
    }
}
