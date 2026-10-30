//! GLM Coding Plan usage adapter, ported from `Sources/Providers/GLM*.swift`.
//!
//! The credential is borrowed read-only from Claude Code, ZCode, or OpenCode. A token is only
//! accepted when its surrounding configuration identifies Z.ai/BigModel, and secret values are
//! never logged or emitted. The quota endpoint expects the raw token in `Authorization` (without
//! `Bearer`) and can report business errors inside an HTTP 200 response.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const POLL_SECS: u64 = 300;
const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_CAP_SECS: u64 = 900;
const GLOBAL_CONSOLE: &str = "https://api.z.ai";
const CHINA_CONSOLE: &str = "https://open.bigmodel.cn";
const ENCRYPTED_MARKER: &str = "enc:v1:";
const OPEN_CODE_IDS: [&str; 7] = [
    "zai-coding-plan",
    "zai",
    "z-ai",
    "z.ai",
    "glm",
    "zhipu",
    "zhipuai",
];

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CONSECUTIVE_429: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
    token: String,
    console: &'static str,
    source: &'static str,
}

#[derive(Default)]
struct CredentialPaths {
    claude_settings: Option<PathBuf>,
    zcode_config: Option<PathBuf>,
    zcode_credentials: Option<PathBuf>,
    opencode_auth: Vec<PathBuf>,
}

fn credential_paths() -> CredentialPaths {
    let Some(home) = dirs::home_dir() else {
        return CredentialPaths::default();
    };
    let mut opencode_auth = vec![home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("auth.json")];

    // OpenCode follows the platform data directory in some Windows builds and XDG's path in
    // others. Keep all known locations, in stable order, without replacing the cross-platform one.
    for base in [dirs::data_dir(), dirs::data_local_dir()]
        .into_iter()
        .flatten()
    {
        let path = base.join("opencode").join("auth.json");
        if !opencode_auth.contains(&path) {
            opencode_auth.push(path);
        }
    }

    CredentialPaths {
        claude_settings: Some(home.join(".claude").join("settings.json")),
        zcode_config: Some(home.join(".zcode").join("v2").join("config.json")),
        zcode_credentials: Some(home.join(".zcode").join("v2").join("credentials.json")),
        opencode_auth,
    }
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("glm.json")
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

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn nonempty(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn host_from_url(value: &str) -> Option<String> {
    let after_scheme = value
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value.trim());
    let authority = after_scheme.split(['/', '?', '#']).next()?.trim();
    let host = authority
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim()
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn console_for_host(host: &str) -> Option<&'static str> {
    if host == "api.z.ai" || host.ends_with(".z.ai") {
        Some(GLOBAL_CONSOLE)
    } else if host == "open.bigmodel.cn" || host.ends_with(".bigmodel.cn") {
        Some(CHINA_CONSOLE)
    } else {
        None
    }
}

fn claude_credential(path: &Path) -> Option<Credential> {
    let root = read_json(path)?;
    let env = root.get("env")?.as_object()?;
    let token = nonempty(env.get("ANTHROPIC_AUTH_TOKEN"))
        .or_else(|| nonempty(env.get("ANTHROPIC_API_KEY")))?;
    let base = nonempty(env.get("ANTHROPIC_BASE_URL"))?;
    let console = console_for_host(&host_from_url(&base)?)?;
    Some(Credential {
        token,
        console,
        source: "Claude Code",
    })
}

fn zcode_plan_credential(path: &Path) -> Option<Credential> {
    let root = read_json(path)?;
    let providers = root.get("provider")?.as_object()?;
    let mut ids: Vec<&String> = providers
        .keys()
        .filter(|id| id.contains("coding-plan"))
        .collect();
    ids.sort();
    for id in ids {
        let Some(provider) = providers.get(id).and_then(|v| v.as_object()) else {
            continue;
        };
        if provider.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let Some(options) = provider.get("options").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(token) = nonempty(options.get("apiKey")) else {
            continue;
        };
        let console = nonempty(options.get("baseURL"))
            .and_then(|v| host_from_url(&v))
            .and_then(|h| console_for_host(&h))
            .unwrap_or(GLOBAL_CONSOLE);
        return Some(Credential {
            token,
            console,
            source: "ZCode",
        });
    }
    None
}

fn zcode_has_start_plan(path: &Path) -> bool {
    let Some(root) = read_json(path) else {
        return false;
    };
    let Some(providers) = root.get("provider").and_then(|v| v.as_object()) else {
        return false;
    };
    providers.iter().any(|(id, value)| {
        let Some(provider) = value.as_object() else {
            return false;
        };
        let Some(options) = provider.get("options").and_then(|v| v.as_object()) else {
            return false;
        };
        id.contains("start-plan")
            && provider
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
            && nonempty(options.get("apiKey")).is_some()
    })
}

fn zcode_oauth_credential(path: &Path) -> Option<Credential> {
    let root = read_json(path)?;
    let token = nonempty(root.get("oauth:zai:access_token"))?;
    if token.starts_with(ENCRYPTED_MARKER) {
        return None;
    }
    Some(Credential {
        token,
        console: GLOBAL_CONSOLE,
        source: "ZCode",
    })
}

fn opencode_credential(path: &Path) -> Option<Credential> {
    let root = read_json(path)?;
    for id in OPEN_CODE_IDS {
        let Some(entry) = root.get(id) else { continue };
        let token = nonempty(Some(entry)).or_else(|| {
            let object = entry.as_object()?;
            [
                "apiKey",
                "api_key",
                "token",
                "key",
                "accessToken",
                "auth_token",
            ]
            .iter()
            .find_map(|key| nonempty(object.get(*key)))
        });
        if let Some(token) = token {
            let console = if id.starts_with("zhipu") {
                CHINA_CONSOLE
            } else {
                GLOBAL_CONSOLE
            };
            return Some(Credential {
                token,
                console,
                source: "OpenCode",
            });
        }
    }
    None
}

fn load_credential_from(paths: &CredentialPaths) -> Option<Credential> {
    paths
        .claude_settings
        .as_deref()
        .and_then(claude_credential)
        .or_else(|| {
            paths
                .zcode_config
                .as_deref()
                .and_then(zcode_plan_credential)
        })
        .or_else(|| {
            paths
                .zcode_credentials
                .as_deref()
                .and_then(zcode_oauth_credential)
        })
        .or_else(|| {
            paths
                .opencode_auth
                .iter()
                .find_map(|p| opencode_credential(p))
        })
}

fn has_start_plan(paths: &CredentialPaths) -> bool {
    paths
        .zcode_config
        .as_deref()
        .map(zcode_has_start_plan)
        .unwrap_or(false)
}

pub fn present() -> bool {
    let paths = credential_paths();
    load_credential_from(&paths).is_some() || has_start_plan(&paths)
}

/// Doctor output contains source metadata only, never credential values or fragments.
pub fn probe() -> String {
    let paths = credential_paths();
    if let Some(cred) = load_credential_from(&paths) {
        return format!(
            "GLM: Coding Plan credential found via {} ({})",
            cred.source, cred.console
        );
    }
    if has_start_plan(&paths) {
        return "GLM: ZCode Start Plan found; Z.ai publishes no compatible usage endpoint".into();
    }
    "GLM: no Coding Plan credential found in Claude Code, ZCode, or OpenCode".into()
}

#[derive(Debug, PartialEq)]
enum ParseErr {
    NeedsAuth,
    RateLimited,
    Other(String),
}

#[derive(Debug)]
struct ParsedUsage {
    level: Option<String>,
    windows: Vec<LimitWindow>,
}

fn number(v: Option<&serde_json::Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64()).filter(|v| v.is_finite())
}

fn parse_usage(v: &serde_json::Value) -> Result<ParsedUsage, ParseErr> {
    let code = v.get("code").and_then(|v| v.as_i64());
    let success = v.get("success").and_then(|v| v.as_bool());
    let succeeded = success.unwrap_or(false) || code.is_none() || code == Some(200);
    if !succeeded {
        return match code {
            Some(401 | 403) => Err(ParseErr::NeedsAuth),
            Some(429) => Err(ParseErr::RateLimited),
            Some(code) => Err(ParseErr::Other(format!("GLM response code {code}"))),
            None => Err(ParseErr::Other("GLM response reported failure".into())),
        };
    }

    let data = v.get("data").unwrap_or(&serde_json::Value::Null);
    let level = nonempty(data.get("level"));
    let mut windows = data
        .get("limits")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|limit| {
            let percentage = number(limit.get("percentage"))?;
            let kind = limit.get("type").and_then(|v| v.as_str());
            let unit = limit.get("unit").and_then(|v| v.as_i64());
            let amount = limit.get("number").and_then(|v| v.as_i64());
            let (id, label) = match (kind, unit, amount) {
                (Some("TIME_LIMIT"), _, _) => ("mcp".into(), "MCP (1 month)".into()),
                (_, Some(3), Some(5)) => ("session".into(), "Current session".into()),
                (_, Some(6), Some(1)) => ("weekly".into(), "Weekly".into()),
                (_, Some(unit), Some(amount)) => {
                    let label = match unit {
                        3 => format!("Usage ({amount} h)"),
                        6 => format!("Usage ({amount} wk)"),
                        _ => "Usage".into(),
                    };
                    (format!("window-{unit}x{amount}"), label)
                }
                _ => (
                    kind.unwrap_or("unknown").to_ascii_lowercase(),
                    "Usage".into(),
                ),
            };
            let resets_at = number(limit.get("nextResetTime")).map(|ms| ms.max(0.0) as u64);
            Some(LimitWindow {
                id,
                label,
                // Preserve over-limit readings (for example 128%); the renderer clamps only the arc.
                used: percentage / 100.0,
                resets_at,
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by(|a, b| {
        window_rank(&a.id)
            .cmp(&window_rank(&b.id))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(ParsedUsage { level, windows })
}

fn window_rank(id: &str) -> u8 {
    match id {
        "session" => 0,
        "weekly" => 1,
        "mcp" => 2,
        _ => 3,
    }
}

fn retry_after_secs(resp: &ureq::Response) -> Option<u64> {
    crate::polling::retry_after_hint_secs(resp.header("retry-after"), now_ms())
}

fn backoff_secs(attempt: u32, retry_after: Option<u64>) -> u64 {
    let doubled = BACKOFF_BASE_SECS.saturating_mul(1u64 << attempt.min(4));
    crate::polling::backoff_secs(doubled.clamp(BACKOFF_BASE_SECS, BACKOFF_CAP_SECS), retry_after)
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn fetch_usage(credential: &Credential, attempt: u32) -> Result<ParsedUsage, FetchErr> {
    let endpoint = format!("{}/api/monitor/usage/quota/limit", credential.console);
    let response = ureq::get(&endpoint)
        .set("Authorization", &credential.token)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call();
    let value = match response {
        Ok(r) => r
            .into_json::<serde_json::Value>()
            .map_err(|e| FetchErr::Other(format!("parse: {e}")))?,
        Err(ureq::Error::Status(401 | 403, _)) => return Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(429, r)) => {
            return Err(FetchErr::RateLimited(backoff_secs(
                attempt,
                retry_after_secs(&r),
            )))
        }
        Err(ureq::Error::Status(code, _)) => return Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(e) => return Err(FetchErr::Other(format!("{e}"))),
    };
    parse_usage(&value).map_err(|error| match error {
        ParseErr::NeedsAuth => FetchErr::NeedsAuth,
        ParseErr::RateLimited => FetchErr::RateLimited(backoff_secs(attempt, None)),
        ParseErr::Other(message) => FetchErr::Other(message),
    })
}

fn read_once(prev: &UsageSnapshot) -> UsageSnapshot {
    let mut snap = prev.clone();
    let now = now_ms();
    if snap.backoff_until > now {
        snap.note = format!(
            "Rate limited — retrying in {}s",
            (snap.backoff_until - now) / 1000
        );
        return snap;
    }
    let paths = credential_paths();
    let Some(credential) = load_credential_from(&paths) else {
        snap.backoff_until = 0;
        if has_start_plan(&paths) {
            snap.status = "none".into();
            snap.windows.clear();
            snap.note =
                "Z.ai does not publish GLM Start Plan usage; Coding Plan is supported.".into();
        } else {
            snap.status = "needsAuth".into();
            snap.note = "Set up a GLM Coding Plan key in Claude Code, ZCode, or OpenCode.".into();
        }
        return snap;
    };
    let attempt = CONSECUTIVE_429.load(std::sync::atomic::Ordering::Relaxed);
    match fetch_usage(&credential, attempt) {
        Ok(parsed) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.fetched_at = now_ms();
            snap.backoff_until = 0;
            snap.windows = parsed.windows;
            let plan = parsed.level.map(|p| format!(" · {p}")).unwrap_or_default();
            if snap.windows.is_empty() {
                snap.status = "none".into();
                snap.note = format!(
                    "GLM reported no usage windows · via {}{}",
                    credential.source, plan
                );
            } else {
                snap.status = "ok".into();
                snap.note = format!("via {}{}", credential.source, plan);
            }
        }
        Err(FetchErr::NeedsAuth) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = "needsAuth".into();
            snap.note = format!(
                "GLM rejected the Coding Plan credential from {}",
                credential.source
            );
        }
        Err(FetchErr::RateLimited(seconds)) => {
            CONSECUTIVE_429.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            snap.backoff_until = crate::polling::deadline_ms(now_ms(), seconds);
            snap.status = if snap.windows.is_empty() {
                "backoff"
            } else {
                "stale"
            }
            .into();
            snap.note = format!("Rate limited — retrying in {seconds}s");
        }
        Err(FetchErr::Other(message)) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            snap.status = if snap.windows.is_empty() {
                "error"
            } else {
                "stale"
            }
            .into();
            snap.note = message;
        }
    }
    snap
}

fn broadcast(app: &AppHandle, snap: UsageSnapshot) {
    let state = app.state::<AppState>();
    *state.glm.lock().unwrap() = snap.clone();
    persist(&snap);
    let _ = app.emit("glm", &snap);
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
            let snap = state.glm.lock().unwrap().clone();
            let _ = app.emit("glm", &snap);
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
                let snapshot = state.glm.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            let deadline = snapshot.backoff_until;
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("glm: {}", snapshot.note));
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
    fn live_quota_shape_keeps_order_reset_and_over_limit_value() {
        let parsed = parse_usage(&json(
            r#"{"code":200,"success":true,"data":{"level":"pro","limits":[
                {"type":"TIME_LIMIT","percentage":4.0},
                {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":8.1,"nextResetTime":1789190400000},
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":128.0,"nextResetTime":1788682200000}]}}"#,
        ))
        .unwrap();
        assert_eq!(parsed.level.as_deref(), Some("pro"));
        assert_eq!(
            parsed
                .windows
                .iter()
                .map(|w| w.id.as_str())
                .collect::<Vec<_>>(),
            ["session", "weekly", "mcp"]
        );
        assert!((parsed.windows[0].used - 1.28).abs() < 1e-9);
        assert_eq!(parsed.windows[0].resets_at, Some(1_788_682_200_000));
    }

    #[test]
    fn envelope_auth_and_rate_limit_errors_are_not_treated_as_usage() {
        assert_eq!(
            parse_usage(&json(r#"{"code":401,"success":false}"#)).unwrap_err(),
            ParseErr::NeedsAuth
        );
        assert_eq!(
            parse_usage(&json(r#"{"code":429,"success":false}"#)).unwrap_err(),
            ParseErr::RateLimited
        );
    }

    #[test]
    fn credential_sources_are_strict_and_priority_is_stable() {
        let root = std::env::temp_dir().join(format!("glm-rust-test-{}", now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let claude = root.join("claude.json");
        let zcode = root.join("zcode.json");
        let opencode = root.join("opencode.json");
        std::fs::write(&claude, r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.z.ai/api/anthropic","ANTHROPIC_AUTH_TOKEN":"claude-secret"}}"#).unwrap();
        std::fs::write(&zcode, r#"{"oauth:zai:access_token":"zcode-secret"}"#).unwrap();
        std::fs::write(&opencode, r#"{"zhipu":{"apiKey":"cn-secret"}}"#).unwrap();
        let paths = CredentialPaths {
            claude_settings: Some(claude.clone()),
            zcode_credentials: Some(zcode),
            opencode_auth: vec![opencode],
            ..Default::default()
        };
        let credential = load_credential_from(&paths).unwrap();
        assert_eq!(credential.source, "Claude Code");
        assert_eq!(credential.console, GLOBAL_CONSOLE);

        std::fs::write(&claude, r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.anthropic.com","ANTHROPIC_AUTH_TOKEN":"anthropic-secret"}}"#).unwrap();
        let credential = load_credential_from(&paths).unwrap();
        assert_eq!(credential.source, "ZCode");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn encrypted_zcode_token_and_disabled_plan_are_skipped() {
        let root = std::env::temp_dir().join(format!("glm-rust-disabled-test-{}", now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.json");
        let credentials = root.join("credentials.json");
        std::fs::write(&config, r#"{"provider":{"builtin:zai-coding-plan":{"enabled":false,"options":{"apiKey":"secret"}}}}"#).unwrap();
        std::fs::write(
            &credentials,
            r#"{"oauth:zai:access_token":"enc:v1:not-readable"}"#,
        )
        .unwrap();
        assert!(zcode_plan_credential(&config).is_none());
        assert!(zcode_oauth_credential(&credentials).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn backoff_doubles_and_server_hint_can_extend_it() {
        assert_eq!(backoff_secs(0, None), 60);
        assert_eq!(backoff_secs(2, None), 240);
        assert_eq!(backoff_secs(99, None), 900);
        assert_eq!(backoff_secs(0, Some(600)), 600);
        assert_eq!(backoff_secs(3, Some(2)), 480);
    }
}
