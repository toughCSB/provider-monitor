//! Command Code usage adapter, ported from `Sources/Providers/CommandCode*.swift`.
//!
//! The desktop app owns `~/.commandcode/auth.json`; Provider Monitor only reads it. The
//! `COMMAND_CODE_API_KEY` environment variable has the same precedence as it does in the macOS
//! implementation. No credential value is ever written or logged.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const WHOAMI_ENDPOINT: &str = "https://api.commandcode.ai/alpha/whoami";
const CREDITS_ENDPOINT: &str = "https://api.commandcode.ai/alpha/billing/credits";
const SUBSCRIPTIONS_ENDPOINT: &str = "https://api.commandcode.ai/alpha/billing/subscriptions";
const SUMMARY_ENDPOINT: &str = "https://api.commandcode.ai/alpha/usage/summary";
const POLL_SECS: u64 = 300;
const BACKOFF_MIN_SECS: u64 = 60;

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
    dirs::home_dir().map(|home| home.join(".commandcode").join("auth.json"))
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("commandcode.json")
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

#[derive(Debug, PartialEq)]
struct Credential {
    api_key: String,
    user_name: Option<String>,
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn load_credential_from(path: Option<&Path>, env_key: Option<&str>) -> Option<Credential> {
    if let Some(api_key) = nonempty(env_key) {
        return Some(Credential {
            api_key,
            user_name: None,
        });
    }
    let text = std::fs::read_to_string(path?).ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Credential {
        api_key: nonempty(root.get("apiKey").and_then(|v| v.as_str()))?,
        user_name: nonempty(root.get("userName").and_then(|v| v.as_str())),
    })
}

fn load_credential() -> Option<Credential> {
    let path = auth_path();
    load_credential_from(
        path.as_deref(),
        std::env::var("COMMAND_CODE_API_KEY").ok().as_deref(),
    )
}

pub fn present() -> bool {
    load_credential().is_some()
}

/// For doctor output; deliberately contains no key or key fragment.
pub fn probe() -> String {
    if std::env::var("COMMAND_CODE_API_KEY")
        .ok()
        .and_then(|v| nonempty(Some(&v)))
        .is_some()
    {
        return "Command Code: COMMAND_CODE_API_KEY is set".into();
    }
    let Some(path) = auth_path() else {
        return "Command Code: cannot locate home directory".into();
    };
    if !path.is_file() {
        return format!(
            "Command Code: {} not found (app not installed, or not signed in)",
            path.display()
        );
    }
    match load_credential() {
        Some(_) => "Command Code: auth.json usable".into(),
        None => format!("Command Code: {} has no usable apiKey", path.display()),
    }
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn retry_after_secs(response: &ureq::Response) -> u64 {
    crate::polling::retry_after_secs(response.header("retry-after"), now_ms(), BACKOFF_MIN_SECS)
}

fn fetch_json(
    endpoint: &str,
    token: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, FetchErr> {
    let mut request = ureq::get(endpoint)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "command-code-desktop")
        .set("x-command-code-version", "desktop")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15));
    for (key, value) in query {
        if !value.is_empty() {
            request = request.query(key, value);
        }
    }
    match request.call() {
        Ok(response) => response
            .into_json()
            .map_err(|error| FetchErr::Other(format!("parse: {error}"))),
        Err(ureq::Error::Status(401 | 403, _)) => Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(429, response)) => {
            Err(FetchErr::RateLimited(retry_after_secs(&response)))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(error) => Err(FetchErr::Other(error.to_string())),
    }
}

fn number(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(serde_json::Value::as_f64)
}

/// ISO 8601, Unix seconds, or Unix milliseconds. Zero means no reset.
fn parse_date(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    if let Some(stamp) = value.as_str() {
        return chrono::DateTime::parse_from_rfc3339(stamp)
            .ok()
            .map(|date| date.timestamp_millis().max(0) as u64);
    }
    let mut unix = value.as_f64()?;
    if unix <= 0.0 {
        return None;
    }
    if unix < 1_000_000_000_000.0 {
        unix *= 1000.0;
    }
    Some(unix as u64)
}

fn org_id(whoami: &serde_json::Value) -> Option<&str> {
    whoami
        .get("org")?
        .get("id")?
        .as_str()
        .filter(|id| !id.is_empty())
}

fn subscription_data(subscription: &serde_json::Value) -> &serde_json::Value {
    subscription.get("data").unwrap_or(subscription)
}

fn plan_name(plan_id: Option<&str>) -> Option<String> {
    let plan_id = plan_id?.trim();
    if plan_id.is_empty() {
        return None;
    }
    if plan_id
        .to_ascii_lowercase()
        .replace('-', "_")
        .contains("goat")
    {
        Some("GOAT".into())
    } else {
        Some(plan_id.into())
    }
}

fn limit_window(raw: Option<&serde_json::Value>, id: &str, label: &str) -> Option<LimitWindow> {
    let raw = raw?;
    let cap = number(raw.get("cap"))?;
    if cap <= 0.0 {
        return None;
    }
    let used = number(raw.get("used")).unwrap_or(0.0);
    Some(LimitWindow {
        id: id.into(),
        label: label.into(),
        used: used / cap,
        resets_at: parse_date(raw.get("resetAt")),
        ..Default::default()
    })
}

fn windows_from_documents(
    summary: &serde_json::Value,
    credits_root: &serde_json::Value,
    subscription: &serde_json::Value,
) -> Vec<LimitWindow> {
    let credits = credits_root.get("credits");
    let limits = credits_root.get("windowLimits");
    let used = number(summary.get("totalCost")).unwrap_or(0.0);
    let remaining = number(credits.and_then(|value| value.get("monthlyCredits"))).unwrap_or(0.0);
    let cap = if used > 0.0 || remaining > 0.0 {
        used + remaining
    } else {
        0.0
    };
    if cap <= 0.0 {
        return Vec::new();
    }

    let mut windows = vec![LimitWindow {
        id: "monthly".into(),
        label: "Monthly limit".into(),
        used: used / cap,
        resets_at: parse_date(subscription_data(subscription).get("currentPeriodEnd")),
        ..Default::default()
    }];
    if let Some(window) = limit_window(
        limits.and_then(|value| value.get("fiveHour")),
        "fiveHour",
        "5h limit",
    ) {
        windows.push(window);
    }
    if let Some(window) = limit_window(
        limits.and_then(|value| value.get("weekly")),
        "weekly",
        "Weekly limit",
    ) {
        windows.push(window);
    }
    windows
}

fn fetch_snapshot(credential: &Credential) -> Result<(Vec<LimitWindow>, Option<String>), FetchErr> {
    let whoami = fetch_json(WHOAMI_ENDPOINT, &credential.api_key, &[])?;
    let org = org_id(&whoami);
    let org_query = org.map(|id| vec![("orgId", id)]).unwrap_or_default();
    let credits = fetch_json(CREDITS_ENDPOINT, &credential.api_key, &org_query)?;
    let subscription = fetch_json(SUBSCRIPTIONS_ENDPOINT, &credential.api_key, &org_query)?;
    let sub = subscription_data(&subscription);

    let mut summary_query = org_query;
    let since = sub
        .get("currentPeriodStart")
        .and_then(|value| value.as_str())
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    if let Some(ref since) = since {
        summary_query.push(("since", since));
    }
    let summary = fetch_json(SUMMARY_ENDPOINT, &credential.api_key, &summary_query)?;
    let plan = plan_name(sub.get("planId").and_then(|value| value.as_str()));
    Ok((
        windows_from_documents(&summary, &credits, &subscription),
        plan,
    ))
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
            "Sign in with the Command Code app — Provider Monitor only reads its auth file.".into();
        return snapshot;
    };

    match fetch_snapshot(&credential) {
        Ok((windows, plan)) => {
            snapshot.fetched_at = now_ms();
            snapshot.backoff_until = 0;
            if windows.is_empty() {
                snapshot.status = "none".into();
                snapshot.windows.clear();
                snapshot.note = "Command Code has nothing metered on this account yet".into();
            } else {
                snapshot.status = "ok".into();
                snapshot.windows = windows;
                snapshot.note = match (plan, credential.user_name) {
                    (Some(plan), Some(user)) => format!("{plan} · {user}"),
                    (Some(plan), None) => plan,
                    (None, Some(user)) => user,
                    (None, None) => String::new(),
                };
            }
        }
        Err(FetchErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note =
                "Command Code rejected its sign-in — sign in with the Command Code app again"
                    .into();
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
    *state.commandcode.lock().unwrap() = snapshot.clone();
    persist(&snapshot);
    let _ = app.emit("commandcode", &snapshot);
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
            let snapshot = state.commandcode.lock().unwrap().clone();
            let _ = app.emit("commandcode", &snapshot);
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
                let snapshot = state.commandcode.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            let deadline = snapshot.backoff_until;
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("commandcode: {}", snapshot.note));
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

    fn temporary_file(name: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "provider-monitor-{name}-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn monthly_and_auxiliary_windows_match_the_alpha_documents() {
        let summary = serde_json::json!({"totalCost": 50.5});
        let credits = serde_json::json!({
            "credits": {"monthlyCredits": 19.5},
            "windowLimits": {
                "fiveHour": {"used": 0, "cap": 14, "resetAt": 0},
                "weekly": {"used": 22.45, "cap": 35, "resetAt": 1787857355088_u64}
            }
        });
        let subscription =
            serde_json::json!({"data": {"currentPeriodEnd": "2026-09-13T15:21:27.000Z"}});
        let windows = windows_from_documents(&summary, &credits, &subscription);
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            ["monthly", "fiveHour", "weekly"]
        );
        assert!((windows[0].used - 50.5 / 70.0).abs() < 1e-9);
        assert_eq!(windows[1].resets_at, None);
        assert_eq!(windows[2].resets_at, Some(1787857355088));
    }

    #[test]
    fn empty_credits_are_not_a_reading() {
        assert!(windows_from_documents(
            &serde_json::json!({"totalCost": 0}),
            &serde_json::json!({"credits": {}}),
            &serde_json::json!({})
        )
        .is_empty());
    }

    #[test]
    fn command_code_environment_key_wins_and_legacy_name_is_not_considered() {
        let path = temporary_file(
            "command-code-auth",
            r#"{"apiKey":"user_file","userName":"tester"}"#,
        );
        let from_file = load_credential_from(Some(&path), None).unwrap();
        let from_env = load_credential_from(Some(&path), Some(" user_env ")).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(from_file.api_key, "user_file");
        assert_eq!(from_file.user_name.as_deref(), Some("tester"));
        assert_eq!(
            from_env,
            Credential {
                api_key: "user_env".into(),
                user_name: None
            }
        );
    }

    #[test]
    fn goat_plan_and_org_are_normalized() {
        assert_eq!(plan_name(Some("individual-goat")).as_deref(), Some("GOAT"));
        let whoami = serde_json::json!({"org": {"id": "org-9"}});
        assert_eq!(org_id(&whoami), Some("org-9"));
    }

    #[test]
    fn unix_seconds_milliseconds_and_zero_are_distinguished() {
        assert_eq!(
            parse_date(Some(&serde_json::json!(1_789_000_000_u64))),
            Some(1_789_000_000_000)
        );
        assert_eq!(
            parse_date(Some(&serde_json::json!(1_789_000_000_123_u64))),
            Some(1_789_000_000_123)
        );
        assert_eq!(parse_date(Some(&serde_json::json!(0))), None);
    }
}
