//! Devin usage adapter, ported from `Sources/Providers/Devin*.swift`.
//!
//! Devin Desktop's `windsurfAuthStatus` SQLite row is preferred, with the Devin CLI's
//! `credentials.toml` as a fallback. Both stores are opened read-only and re-read on every poll;
//! Provider Monitor never refreshes, writes, logs, or emits the session key.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://server.self-serve.windsurf.com/exa.seat_management_pb.SeatManagementService/GetUserStatus";
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Credential {
    api_key: String,
    email: Option<String>,
    source: &'static str,
}

#[derive(Default)]
struct CredentialPaths {
    desktop: Vec<PathBuf>,
    cli: Vec<PathBuf>,
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn credential_paths() -> CredentialPaths {
    let mut paths = CredentialPaths::default();

    // On Windows dirs::config_dir() is %APPDATA%. `Devin` is the current app name; keep the
    // former `Windsurf` location as a compatibility fallback for upgraded installations.
    if let Some(config) = dirs::config_dir() {
        for product in ["Devin", "Windsurf"] {
            push_unique(
                &mut paths.desktop,
                config
                    .join(product)
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            );
        }
    }
    if let Some(home) = dirs::home_dir() {
        for product in ["Devin", "Windsurf"] {
            push_unique(
                &mut paths.desktop,
                home.join("Library")
                    .join("Application Support")
                    .join(product)
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            );
        }
        push_unique(
            &mut paths.cli,
            home.join(".local")
                .join("share")
                .join("devin")
                .join("credentials.toml"),
        );
    }
    // Native Windows CLI builds may select either roaming or local app data rather than XDG.
    for base in [dirs::data_dir(), dirs::data_local_dir()]
        .into_iter()
        .flatten()
    {
        push_unique(&mut paths.cli, base.join("devin").join("credentials.toml"));
    }
    paths
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("devin.json")
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

/// `mode=ro` sees an active editor's WAL; immutable is the fallback for a closed editor whose
/// shared-memory file has disappeared. Neither mode can modify Devin's database.
fn open_ro(path: &Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    if !path.is_file() {
        return None;
    }
    if let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        if connection
            .prepare("SELECT 1 FROM ItemTable LIMIT 1")
            .and_then(|mut statement| statement.query([]).map(|_| ()))
            .is_ok()
        {
            return Some(connection);
        }
    }
    let mut uri = String::from("file:///");
    uri.push_str(
        &path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .replace('#', "%23")
            .replace('?', "%3F"),
    );
    uri.push_str("?immutable=1");
    rusqlite::Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

fn desktop_credential(path: &Path) -> Option<Credential> {
    let connection = open_ro(path)?;
    let value = connection
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            ["windsurfAuthStatus"],
            |row| row.get::<_, String>(0),
        )
        .ok()?;
    let root: serde_json::Value = serde_json::from_str(&value).ok()?;
    let api_key = root.get("apiKey")?.as_str()?.trim();
    if api_key.is_empty() {
        return None;
    }
    let email = root
        .get("email")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from);
    Some(Credential {
        api_key: api_key.into(),
        email,
        source: "Devin Desktop",
    })
}

fn parse_cli_credential(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let (key, raw) = line.split_once('=')?;
        if key.trim() != "windsurf_api_key" {
            return None;
        }
        let value = raw.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.split_once('"').map(|(value, _)| value))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|v| v.split_once('\'').map(|(value, _)| value))
            })?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn cli_credential(path: &Path) -> Option<Credential> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(Credential {
        api_key: parse_cli_credential(&content)?,
        email: None,
        source: "Devin CLI",
    })
}

fn load_credential_from(paths: &CredentialPaths) -> Option<Credential> {
    paths
        .desktop
        .iter()
        .find_map(|path| desktop_credential(path))
        .or_else(|| paths.cli.iter().find_map(|path| cli_credential(path)))
}

fn load_credential() -> Option<Credential> {
    load_credential_from(&credential_paths())
}

pub fn present() -> bool {
    let paths = credential_paths();
    paths
        .desktop
        .iter()
        .chain(paths.cli.iter())
        .any(|path| path.is_file())
}

/// Doctor output contains a source label, never account or session values.
pub fn probe() -> String {
    let paths = credential_paths();
    match load_credential_from(&paths) {
        Some(credential) => format!("Devin: session found via {}", credential.source),
        None if present() => {
            "Devin: credential source exists but no usable session was found".into()
        }
        None => "Devin: Desktop database and CLI credentials.toml not found".into(),
    }
}

#[derive(Debug)]
struct ParsedUsage {
    windows: Vec<LimitWindow>,
    overage_balance: Option<String>,
}

fn finite_number(value: Option<&serde_json::Value>) -> Option<f64> {
    let value = value?;
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))?;
    number.is_finite().then_some(number)
}

fn quota_window(plan: &serde_json::Value, id: &str, label: &str) -> Option<LimitWindow> {
    let hidden_key = if id == "daily" {
        "hideDailyQuota"
    } else {
        "hideWeeklyQuota"
    };
    if plan.get(hidden_key).and_then(|value| value.as_bool()) == Some(true) {
        return None;
    }
    let reset_key = format!("{id}QuotaResetAtUnix");
    let remaining_key = format!("{id}QuotaRemainingPercent");
    let resets_at = finite_number(plan.get(&reset_key))
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as u64);
    let remaining = match finite_number(plan.get(&remaining_key)) {
        Some(value) if (0.0..=100.0).contains(&value) => value,
        _ if resets_at.is_some() => 0.0,
        _ => return None,
    };
    Some(LimitWindow {
        id: id.into(),
        label: label.into(),
        used: (100.0 - remaining) / 100.0,
        resets_at,
        group: Some("Usage".into()),
        ..Default::default()
    })
}

fn overage_balance(plan: &serde_json::Value) -> Option<String> {
    let micros = finite_number(plan.get("overageBalanceMicros"))?;
    if micros < 0.0 {
        return None;
    }
    let cents = (micros / 10_000.0).round();
    if !cents.is_finite() || cents < i64::MIN as f64 || cents > i64::MAX as f64 {
        return None;
    }
    Some(format!("${:.2}", cents / 100.0))
}

fn parse_usage(value: &serde_json::Value) -> Result<ParsedUsage, String> {
    let plan = value
        .get("userStatus")
        .and_then(|value| value.get("planStatus"))
        .ok_or_else(|| "Devin response has no userStatus.planStatus".to_string())?;
    let mut windows = Vec::new();
    if let Some(window) = quota_window(plan, "daily", "Daily quota") {
        windows.push(window);
    }
    if let Some(window) = quota_window(plan, "weekly", "Weekly quota") {
        windows.push(window);
    }
    let overage_balance = overage_balance(plan);
    if windows.is_empty() && overage_balance.is_none() {
        return Err("Devin response has no valid quota reading".into());
    }
    Ok(ParsedUsage {
        windows,
        overage_balance,
    })
}

fn retry_after_secs(response: &ureq::Response) -> u64 {
    crate::polling::retry_after_secs(response.header("retry-after"), now_ms(), BACKOFF_MIN_SECS)
}

enum FetchErr {
    NeedsAuth,
    AccessDenied,
    RateLimited(u64),
    Other(String),
}

fn fetch_usage(api_key: &str) -> Result<ParsedUsage, FetchErr> {
    let body = serde_json::json!({
        "metadata": {
            "apiKey": api_key,
            "ideName": "windsurf",
            "ideVersion": "1.108.2",
            "extensionName": "windsurf",
            "extensionVersion": "1.108.2",
            "locale": "en"
        }
    });
    // Refuse redirects so the credential-bearing POST can never be forwarded to another host.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .redirects(0)
        .build();
    let response = agent
        .post(ENDPOINT)
        .set("Content-Type", "application/json")
        .set("Connect-Protocol-Version", "1")
        .send_string(&body.to_string());
    let value = match response {
        Ok(response) => response
            .into_json::<serde_json::Value>()
            .map_err(|error| FetchErr::Other(format!("parse: {error}")))?,
        Err(ureq::Error::Status(401, _)) => return Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(403, _)) => return Err(FetchErr::AccessDenied),
        Err(ureq::Error::Status(429, response)) => {
            return Err(FetchErr::RateLimited(retry_after_secs(&response)))
        }
        Err(ureq::Error::Status(code, _)) => return Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(error) => return Err(FetchErr::Other(format!("{error}"))),
    };
    parse_usage(&value).map_err(FetchErr::Other)
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
        snapshot.note = "Sign in to Devin Desktop or run `devin auth login`.".into();
        return snapshot;
    };
    match fetch_usage(&credential.api_key) {
        Ok(parsed) => {
            snapshot.fetched_at = now_ms();
            snapshot.backoff_until = 0;
            snapshot.windows = parsed.windows;
            let balance = parsed
                .overage_balance
                .map(|value| format!(" · Extra usage balance {value}"))
                .unwrap_or_default();
            snapshot.note = format!("via {}{}", credential.source, balance);
            snapshot.status = if snapshot.windows.is_empty() {
                "none"
            } else {
                "ok"
            }
            .into();
        }
        Err(FetchErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note = "Devin rejected its session — sign in again.".into();
        }
        Err(FetchErr::AccessDenied) => {
            snapshot.status = if snapshot.windows.is_empty() {
                "error"
            } else {
                "stale"
            }
            .into();
            snapshot.note = "Devin denied access to this account's usage.".into();
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
    *state.devin.lock().unwrap() = snapshot.clone();
    persist(&snapshot);
    let _ = app.emit("devin", &snapshot);
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
            let snapshot = state.devin.lock().unwrap().clone();
            let _ = app.emit("devin", &snapshot);
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
                let snapshot = state.devin.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            let deadline = snapshot.backoff_until;
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("devin: {}", snapshot.note));
            }
            broadcast(&app, snapshot);
            sleep_interruptible(crate::polling::poll_wait_secs(POLL_SECS, deadline, now_ms()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: &str) -> serde_json::Value {
        serde_json::from_str(value).unwrap()
    }

    #[test]
    fn remaining_percent_is_inverted_and_string_resets_become_milliseconds() {
        let parsed = parse_usage(&json(
            r#"{"userStatus":{"planStatus":{"dailyQuotaRemainingPercent":99,"weeklyQuotaRemainingPercent":"50","dailyQuotaResetAtUnix":"1789113600","weeklyQuotaResetAtUnix":1789286400,"overageBalanceMicros":"14277951"}}}"#,
        ))
        .unwrap();
        assert_eq!(
            parsed
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["daily", "weekly"]
        );
        assert!((parsed.windows[0].used - 0.01).abs() < 1e-9);
        assert!((parsed.windows[1].used - 0.50).abs() < 1e-9);
        assert_eq!(parsed.windows[0].resets_at, Some(1_789_113_600_000));
        assert_eq!(parsed.overage_balance.as_deref(), Some("$14.28"));
    }

    #[test]
    fn missing_remaining_with_reset_means_fully_used() {
        let parsed = parse_usage(&json(
            r#"{"userStatus":{"planStatus":{"dailyQuotaResetAtUnix":"1789113600"}}}"#,
        ))
        .unwrap();
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].used, 1.0);
    }

    #[test]
    fn hidden_and_invalid_quotas_are_not_invented() {
        let parsed = parse_usage(&json(
            r#"{"userStatus":{"planStatus":{"hideDailyQuota":true,"dailyQuotaRemainingPercent":99,"weeklyQuotaRemainingPercent":50}}}"#,
        ))
        .unwrap();
        assert_eq!(
            parsed
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["weekly"]
        );
        assert!(parse_usage(&json(
            r#"{"userStatus":{"planStatus":{"dailyQuotaRemainingPercent":101}}}"#
        ))
        .is_err());
        assert!(parse_usage(&json(r#"{"userStatus":{"planStatus":{}}}"#)).is_err());
    }

    #[test]
    fn cli_parser_accepts_only_the_named_nonempty_key() {
        assert_eq!(
            parse_cli_credential(
                "other = \"wrong\"\nwindsurf_api_key = \"session-token\" # comment"
            ),
            Some("session-token".into())
        );
        assert_eq!(
            parse_cli_credential("windsurf_api_key_extra = \"wrong\""),
            None
        );
        assert_eq!(parse_cli_credential("windsurf_api_key = \"\""), None);
    }

    #[test]
    fn desktop_database_is_read_only_and_wins_over_cli() {
        let root = std::env::temp_dir().join(format!("devin-rust-test-{}", now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("state.vscdb");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                (
                    "windsurfAuthStatus",
                    r#"{"apiKey":"desktop-secret","email":"user@example.com"}"#,
                ),
            )
            .unwrap();
        drop(connection);
        let cli = root.join("credentials.toml");
        std::fs::write(&cli, "windsurf_api_key = \"cli-secret\"").unwrap();
        let credential = load_credential_from(&CredentialPaths {
            desktop: vec![database],
            cli: vec![cli],
        })
        .unwrap();
        assert_eq!(credential.source, "Devin Desktop");
        assert_eq!(credential.email.as_deref(), Some("user@example.com"));
        let _ = std::fs::remove_dir_all(root);
    }
}
