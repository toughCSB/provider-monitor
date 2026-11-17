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
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const API_KEY_ENDPOINT: &str = "https://opencode.ai/zen/go/v1/usage";
const OAUTH_ENDPOINT: &str = "https://opencode.ai/inference/go/v1/usage";
const DEFAULT_CONSOLE: &str = "https://opencode.ai/console";
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

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(xdg).join("opencode"));
    }
    if let Some(home) = dirs::home_dir() {
        let normal = home.join(".local").join("share").join("opencode");
        if !dirs.contains(&normal) { dirs.push(normal); }
    }
    dirs
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

#[derive(Clone, Debug)]
struct Credential {
    token: String,
    oauth: bool,
    org: Option<String>,
    console: Option<String>,
    expires: Option<u64>,
    source: &'static str,
}

fn nonempty(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn credential_from(entry: &serde_json::Value, source: &'static str) -> Option<Credential> {
    if let Some(token) = nonempty(Some(entry)) {
        return Some(Credential { token, oauth:false,org:None,console:None,expires:None,source });
    }
    let obj = entry.as_object()?;
    if obj.get("type").and_then(|v| v.as_str()) == Some("oauth") {
        let meta = obj.get("metadata");
        return Some(Credential {
            token: nonempty(obj.get("access"))?, oauth:true,
            org: nonempty(meta.and_then(|m| m.get("orgID"))),
            console: nonempty(meta.and_then(|m| m.get("server"))),
            expires: obj.get("expires").and_then(|e| e.as_u64().or_else(|| e.as_str().and_then(|s| s.parse().ok()))),
            source,
        });
    }
    let token = ["key", "apiKey", "api_key", "token", "accessToken"]
        .iter().find_map(|k| nonempty(obj.get(*k)))?;
    Some(Credential { token, oauth:false,org:None,console:None,expires:None,source })
}

fn open_db(path: &std::path::Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    if !path.is_file() { return None; }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(path, flags).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(100));
    Some(conn)
}

fn db_rows(dir: &std::path::Path) -> Vec<(String, String)> {
    let Some(conn) = open_db(&dir.join("opencode.db")) else { return Vec::new() };
    let Ok(mut query) = conn.prepare("SELECT integration_id, value FROM credential WHERE integration_id IN ('opencode', 'opencode-go') AND COALESCE(active, 1) != 0 AND length(value) <= 65536 ORDER BY time_updated DESC LIMIT 32") else { return Vec::new() };
    query.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
}

fn read_auth(dir: &std::path::Path) -> Option<serde_json::Value> {
    let path = dir.join("auth.json");
    if std::fs::metadata(&path).ok()?.len() > 1_048_576 { return None; }
    let mut text = String::new();
    std::fs::File::open(path).ok()?.take(1_048_577).read_to_string(&mut text).ok()?;
    if text.len() > 1_048_576 { return None; }
    serde_json::from_str(&text).ok()
}

fn load_credential_from(dirs: &[PathBuf]) -> Option<Credential> {
    let sources: Vec<_> = dirs.iter().map(|dir| {
        let file = read_auth(dir);
        (file, db_rows(dir))
    }).collect();
    let from_db = |rows: &Vec<(String,String)>, id: &str, console_only: bool| {
        rows.iter().filter(|(key,_)| key == id).find_map(|(_,raw)| {
            let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
            let cred = credential_from(&value, "OpenCode")?;
            (!console_only || (!cred.oauth && cred.token.starts_with("oc_sk_"))).then_some(cred)
        })
    };
    // A console key takes precedence over an expired legacy Go key, even when the
    // console key is in SQLite and the legacy key is in auth.json.
    for id in ["opencode", "opencode-go"] {
        for (_, rows) in &sources { if let Some(c) = from_db(rows, id, true) { return Some(c); } }
    }
    for id in ["opencode", "opencode-go"] {
        for (file, _) in &sources {
            if let Some(c) = file.as_ref().and_then(|v| v.get(id)).and_then(|v| credential_from(v,"OpenCode")) {
                if !c.oauth && c.token.starts_with("oc_sk_") { return Some(c); }
            }
        }
    }
    for (file, _) in &sources { if let Some(c) = file.as_ref().and_then(|v| v.get("opencode-go")).and_then(|v| credential_from(v,"OpenCode")) { return Some(c); } }
    for id in ["opencode-go", "opencode"] {
        for (_, rows) in &sources { if let Some(c) = from_db(rows, id, false) { return Some(c); } }
    }
    for (file, _) in &sources { if let Some(c) = file.as_ref().and_then(|v| v.get("opencode")).and_then(|v| credential_from(v,"OpenCode")) { if c.oauth { return Some(c); } } }
    None
}

fn load_credential() -> Option<Credential> { load_credential_from(&data_dirs()) }

fn usage_url(cred: &Credential) -> &'static str {
    if cred.oauth { OAUTH_ENDPOINT } else { API_KEY_ENDPOINT }
}

pub fn present() -> bool {
    data_dirs().iter().any(|d| d.join("auth.json").is_file() || d.join("opencode.db").is_file())
}

/// For doctor: contains no secret values
pub fn probe() -> String {
    match load_credential() {
        Some(c) => format!("OpenCode: {} via {}", if c.oauth { "OAuth sign-in" } else { "Go/console key" }, c.source),
        None if present() => "OpenCode: installed but no usable Go/console credential".into(),
        None => "OpenCode: not installed".into(),
    }
}

enum FetchErr {
    Unauthorized,
    ExpiredKey,
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

fn request(url: &str, cred: &Credential) -> ureq::Request {
    let mut request = ureq::get(url)
        .set("Authorization", &format!("Bearer {}", cred.token))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15));
    if let Some(org) = &cred.org { request = request.set("x-opencode-org-id", org); }
    request
}

fn is_api_key_expired(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).ok()
        .and_then(|v| v.pointer("/error/code").and_then(|x| x.as_str()).map(str::to_owned))
        .as_deref() == Some("api_key_expired")
}

fn signed_in(cred: &Credential) -> bool {
    if !cred.oauth { return false; }
    let console = cred.console.as_deref().unwrap_or(DEFAULT_CONSOLE).trim_end_matches('/');
    // Do not send a borrowed token to an arbitrary metadata URL.
    if !console.starts_with("https://opencode.ai/") && console != "https://opencode.ai" { return false; }
    matches!(request(&format!("{console}/api/user"), cred).call(), Ok(r) if r.status() == 200)
}

fn fetch_usage(cred: &Credential, consecutive_429: u32) -> Result<serde_json::Value, FetchErr> {
    let resp = request(usage_url(cred), cred).call();
    match resp {
        Ok(r) => r.into_json().map_err(|e| FetchErr::Other(format!("parse: {e}"))),
        Err(ureq::Error::Status(401, r)) => {
            let mut body = String::new();
            let _ = r.into_reader().take(65_536).read_to_string(&mut body);
            if is_api_key_expired(&body) { Err(FetchErr::ExpiredKey) } else { Err(FetchErr::Unauthorized) }
        },
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
    let Some(cred) = load_credential() else {
        snap.status = "needsAuth".into();
        snap.note = "Connect Go inside OpenCode (opencode auth login) and the notch reads it.".into();
        return snap;
    };
    let attempt = CONSECUTIVE_429.load(std::sync::atomic::Ordering::Relaxed);
    match fetch_usage(&cred, attempt) {
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
                snap.note = format!("Go · via {}", cred.source);
            }
        }
        Err(FetchErr::ExpiredKey) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = "none".into();
            snap.windows.clear();
            snap.note = "This OpenCode key has expired. Create a new key in the OpenCode console".into();
        }
        Err(FetchErr::Unauthorized) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            if signed_in(&cred) {
                snap.status = "none".into();
                snap.windows.clear();
                snap.note = "No OpenCode Go subscription on this account".into();
            } else {
                snap.status = "needsAuth".into();
                snap.windows.clear();
                snap.note = if cred.expires.is_some_and(|e| e <= now_ms()) {
                    "OpenCode sign-in expired — open OpenCode to renew it"
                } else { "OpenCode key rejected — connect Go again (opencode auth login)" }.into();
            }
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
        assert_eq!(nonempty(Some(&serde_json::json!("  raw-key  "))), Some("raw-key".to_string()));
        assert_eq!(nonempty(Some(&serde_json::json!(""))), None);
    }
    #[test]
    fn console_key_wins_over_legacy_file_and_sqlite_oauth_uses_inference_route() {
        let root = std::env::temp_dir().join(format!("pm-opencode-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("auth.json"), r#"{"opencode-go":{"type":"api","key":"sk-old"},"opencode":{"type":"api","key":"oc_sk_new"}}"#).unwrap();
        let selected = load_credential_from(std::slice::from_ref(&root)).unwrap();
        assert_eq!(selected.token,"oc_sk_new");
        assert_eq!(usage_url(&selected),API_KEY_ENDPOINT);
        let oauth = credential_from(&serde_json::json!({"type":"oauth","access":"opaque","metadata":{"orgID":"org"}}), "OpenCode").unwrap();
        assert_eq!(usage_url(&oauth),OAUTH_ENDPOINT);
        assert_eq!(oauth.org.as_deref(),Some("org"));
        assert!(is_api_key_expired(r#"{"error":{"code":"api_key_expired"}}"#));
        assert!(!is_api_key_expired(r#"{"error":{"code":"invalid_token"}}"#));
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn active_sqlite_console_key_beats_expired_file_key_without_other_integration() {
        let root = std::env::temp_dir().join(format!("pm-opencode-db-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let conn = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
        conn.execute_batch("CREATE TABLE credential (integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER);").unwrap();
        conn.execute("INSERT INTO credential VALUES (?1,?2,1,2)", rusqlite::params!["opencode",r#"{"type":"api","key":"oc_sk_active"}"#]).unwrap();
        conn.execute("INSERT INTO credential VALUES (?1,?2,1,3)", rusqlite::params!["google",r#"{"type":"api","key":"oc_sk_wrong"}"#]).unwrap();
        drop(conn);
        std::fs::write(root.join("auth.json"),r#"{"opencode-go":{"type":"api","key":"sk_expired"}}"#).unwrap();
        assert_eq!(load_credential_from(std::slice::from_ref(&root)).unwrap().token,"oc_sk_active");
        let _ = std::fs::remove_dir_all(root);
    }
}
