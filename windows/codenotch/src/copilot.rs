//! GitHub Copilot usage adapter, ported from
//! `Sources/Providers/GitHubCopilotProvider.swift`.
//!
//! Credentials are borrowed read-only from `GH_TOKEN`/`GITHUB_TOKEN`, GitHub CLI's `hosts.yml`,
//! or the output of `gh auth token`. They are re-read for every request and never logged, persisted,
//! or emitted. Both GitHub CLI's Windows config location and its cross-platform/XDG location are
//! supported.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://api.github.com/copilot_internal/user";
const POLL_SECS: u64 = 300;
const BACKOFF_DEFAULT_SECS: u64 = 60;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Credential {
    token: String,
    username: Option<String>,
    source: &'static str,
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn hosts_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(directory) = std::env::var_os("GH_CONFIG_DIR").filter(|value| !value.is_empty()) {
        push_unique(&mut paths, PathBuf::from(directory).join("hosts.yml"));
    }
    if let Some(config) = dirs::config_dir() {
        // GitHub CLI's native Windows default is `%APPDATA%\GitHub CLI\hosts.yml`.
        push_unique(&mut paths, config.join("GitHub CLI").join("hosts.yml"));
        push_unique(&mut paths, config.join("gh").join("hosts.yml"));
    }
    if let Some(home) = dirs::home_dir() {
        push_unique(
            &mut paths,
            home.join(".config").join("gh").join("hosts.yml"),
        );
    }
    paths
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("copilot.json")
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

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct HostEntry {
    username: Option<String>,
    token: Option<String>,
}

fn yaml_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.strip_prefix(':')?.trim();
    let unquoted = rest
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            rest.strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(rest);
    nonempty(Some(unquoted))
}

fn parse_hosts(text: Option<&str>) -> HostEntry {
    let Some(text) = text else {
        return HostEntry::default();
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == "github.com:") else {
        return HostEntry::default();
    };
    let mut entry = HostEntry::default();
    for line in lines.iter().skip(start + 1) {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        let trimmed = line.trim();
        if let Some(value) = yaml_value(trimmed, "user") {
            entry.username = Some(value);
        }
        if let Some(value) = yaml_value(trimmed, "oauth_token") {
            entry.token = Some(value);
        }
    }
    entry
}

fn read_hosts() -> (HostEntry, Option<PathBuf>) {
    for path in hosts_paths() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let entry = parse_hosts(Some(&text));
        if entry.username.is_some() || entry.token.is_some() {
            return (entry, Some(path));
        }
    }
    (HostEntry::default(), None)
}

fn gh_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        push_unique(
            &mut candidates,
            PathBuf::from(program_files)
                .join("GitHub CLI")
                .join("gh.exe"),
        );
    }
    if let Some(local) = dirs::data_local_dir() {
        push_unique(
            &mut candidates,
            local.join("Programs").join("GitHub CLI").join("gh.exe"),
        );
    }
    if let Some(home) = dirs::home_dir() {
        push_unique(
            &mut candidates,
            home.join("scoop").join("shims").join("gh.exe"),
        );
    }
    for fixed in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"] {
        push_unique(&mut candidates, PathBuf::from(fixed));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            for executable in ["gh.exe", "gh.cmd", "gh"] {
                push_unique(&mut candidates, directory.join(executable));
            }
        }
    }
    candidates
}

fn gh_token() -> Option<String> {
    use std::process::{Command, Stdio};

    let executable = gh_candidates().into_iter().find(|path| path.is_file())?;
    let mut command = Command::new(executable);
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let output = child.stdout.take()?;
                let text = std::io::read_to_string(output).ok()?;
                return nonempty(Some(&text));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn load_credential_with(
    environment_token: Option<String>,
    hosts: HostEntry,
    command_token: impl FnOnce() -> Option<String>,
) -> Option<Credential> {
    if let Some(token) = environment_token {
        return Some(Credential {
            token,
            username: hosts.username,
            source: "GitHub",
        });
    }
    if let Some(token) = hosts.token {
        return Some(Credential {
            token,
            username: hosts.username,
            source: "GitHub CLI",
        });
    }
    command_token().map(|token| Credential {
        token,
        username: hosts.username,
        source: "GitHub CLI",
    })
}

fn load_credential() -> Option<Credential> {
    let environment_token = nonempty(
        std::env::var("GH_TOKEN")
            .ok()
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .as_deref(),
    );
    let (hosts, _) = read_hosts();
    load_credential_with(environment_token, hosts, gh_token)
}

pub fn present() -> bool {
    std::env::var_os("GH_TOKEN")
        .filter(|value| !value.is_empty())
        .is_some()
        || std::env::var_os("GITHUB_TOKEN")
            .filter(|value| !value.is_empty())
            .is_some()
        || hosts_paths().iter().any(|path| path.is_file())
        || gh_candidates().iter().any(|path| path.is_file())
}

/// Doctor output includes a source label, never account or token values.
pub fn probe() -> String {
    match load_credential() {
        Some(credential) => format!("GitHub Copilot: credential found via {}", credential.source),
        None if present() => {
            "GitHub Copilot: GitHub CLI or config found, but no usable github.com token".into()
        }
        None => "GitHub Copilot: sign in with `gh auth login` and enable Copilot".into(),
    }
}

#[derive(Debug)]
struct ParsedUsage {
    windows: Vec<LimitWindow>,
    plan: Option<String>,
}

fn finite_number(value: Option<&serde_json::Value>) -> Option<f64> {
    value
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite())
}

fn parse_date(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    if let Some(number) = finite_number(Some(value)) {
        let milliseconds = if number > 10_000_000_000.0 {
            number
        } else {
            number * 1000.0
        };
        return (milliseconds.is_finite() && milliseconds >= 0.0).then_some(milliseconds as u64);
    }
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|date| date.timestamp_millis().max(0) as u64)
}

fn label(id: &str) -> String {
    match id {
        "premium_interactions" => "Premium requests".into(),
        "chat" => "Chat requests".into(),
        "completions" => "Completions".into(),
        _ => id
            .split('_')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut characters = part.chars();
                characters
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn quota_window(
    id: &str,
    quota: &serde_json::Value,
    root_reset: Option<u64>,
) -> Option<LimitWindow> {
    if quota.get("unlimited").and_then(|value| value.as_bool()) == Some(true) {
        return None;
    }
    let entitlement = finite_number(quota.get("entitlement"));
    let remaining = finite_number(quota.get("remaining"));
    let used = finite_number(quota.get("used"));
    let resets_at = ["reset_date", "reset_at", "resets_at"]
        .iter()
        .find_map(|key| parse_date(quota.get(*key)))
        .or(root_reset);

    if entitlement == Some(0.0) {
        return None;
    }
    if let Some(entitlement) = entitlement.filter(|value| *value > 0.0) {
        let consumed =
            used.unwrap_or_else(|| (entitlement - remaining.unwrap_or(entitlement)).max(0.0));
        return Some(LimitWindow {
            id: id.into(),
            label: label(id),
            used: (consumed / entitlement).max(0.0),
            resets_at,
            ..Default::default()
        });
    }
    // The Windows snapshot has one raw-count field. Preserve the fallback readings there; labels
    // state their direction because, unlike a fraction, a count alone cannot encode used/remaining.
    if let Some(remaining) = remaining.filter(|value| *value >= 0.0) {
        if used.is_none() {
            return Some(LimitWindow {
                id: id.into(),
                label: format!("{} remaining", label(id)),
                count: Some(remaining.round() as i64),
                resets_at,
                ..Default::default()
            });
        }
    }
    used.filter(|value| *value >= 0.0).map(|used| LimitWindow {
        id: id.into(),
        label: format!("{} used", label(id)),
        count: Some(used.round() as i64),
        resets_at,
        ..Default::default()
    })
}

fn parse_usage(root: &serde_json::Value) -> Result<ParsedUsage, String> {
    let quotas = root
        .get("quota_snapshots")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "GitHub Copilot response has no quota_snapshots".to_string())?;
    let root_reset = parse_date(root.get("quota_reset_date"));
    let mut keys = Vec::new();
    for id in ["premium_interactions", "chat", "completions"] {
        if quotas.contains_key(id) {
            keys.push(id.to_string());
        }
    }
    let mut unknown = quotas
        .keys()
        .filter(|id| !keys.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    keys.extend(unknown);
    let windows = keys
        .iter()
        .filter_map(|id| quota_window(id, quotas.get(id)?, root_reset))
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return Err("GitHub Copilot reported no metered quotas".into());
    }
    let plan = nonempty(
        root.get("copilot_plan")
            .or_else(|| root.get("plan"))
            .and_then(|value| value.as_str()),
    );
    Ok(ParsedUsage { windows, plan })
}

fn retry_after_secs(response: &ureq::Response) -> u64 {
    crate::polling::retry_after_secs(response.header("retry-after"), now_ms(), BACKOFF_DEFAULT_SECS)
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn fetch_usage(token: &str) -> Result<ParsedUsage, FetchErr> {
    let response = ureq::get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "Provider Monitor")
        .timeout(Duration::from_secs(15))
        .call();
    let value = match response {
        Ok(response) => response
            .into_json::<serde_json::Value>()
            .map_err(|error| FetchErr::Other(format!("parse: {error}")))?,
        Err(ureq::Error::Status(401 | 403, _)) => return Err(FetchErr::NeedsAuth),
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
        snapshot.note = "Sign in with `gh auth login`, then enable GitHub Copilot.".into();
        return snapshot;
    };
    match fetch_usage(&credential.token) {
        Ok(parsed) => {
            snapshot.fetched_at = now_ms();
            snapshot.backoff_until = 0;
            snapshot.windows = parsed.windows;
            snapshot.status = "ok".into();
            let plan = parsed
                .plan
                .map(|plan| format!(" · {plan}"))
                .unwrap_or_default();
            snapshot.note = format!("via {}{}", credential.source, plan);
        }
        Err(FetchErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note = "GitHub rejected the token — run `gh auth login` again.".into();
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
        Err(FetchErr::Other(message)) if message == "GitHub Copilot reported no metered quotas" => {
            snapshot.fetched_at = now_ms();
            snapshot.backoff_until = 0;
            snapshot.status = "none".into();
            snapshot.windows.clear();
            snapshot.note = message;
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
    *state.copilot.lock().unwrap() = snapshot.clone();
    persist(&snapshot);
    let _ = app.emit("copilot", &snapshot);
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
            let snapshot = state.copilot.lock().unwrap().clone();
            let _ = app.emit("copilot", &snapshot);
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
                let snapshot = state.copilot.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            let deadline = snapshot.backoff_until;
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("copilot: {}", snapshot.note));
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
    fn reads_copilot_quotas_in_provider_order() {
        let parsed = parse_usage(&json(
            r#"{"copilot_plan":"individual","quota_reset_date":"2026-10-01T00:00:00Z","quota_snapshots":{"chat":{"entitlement":50,"remaining":48,"used":2,"unlimited":false},"completions":{"entitlement":2000,"remaining":1990,"used":10,"unlimited":false},"premium_interactions":{"entitlement":300,"remaining":294,"used":6,"unlimited":false}}}"#,
        ))
        .unwrap();
        assert_eq!(parsed.plan.as_deref(), Some("individual"));
        assert_eq!(
            parsed
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["premium_interactions", "chat", "completions"]
        );
        assert_eq!(parsed.windows[0].label, "Premium requests");
        assert!((parsed.windows[0].used - 0.02).abs() < 1e-9);
        assert!((parsed.windows[1].used - 0.04).abs() < 1e-9);
        assert_eq!(
            parsed.windows[0].resets_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
                    .unwrap()
                    .timestamp_millis() as u64
            )
        );
    }

    #[test]
    fn skips_unlimited_and_zero_entitlement() {
        let result = parse_usage(&json(
            r#"{"quota_snapshots":{"chat":{"entitlement":0,"remaining":0,"used":0,"unlimited":false},"completions":{"entitlement":0,"remaining":0,"used":0,"unlimited":true}}}"#,
        ));
        assert_eq!(
            result.unwrap_err(),
            "GitHub Copilot reported no metered quotas"
        );
    }

    #[test]
    fn derives_fraction_when_used_is_absent_and_keeps_count_fallbacks() {
        let parsed = parse_usage(&json(
            r#"{"quota_snapshots":{"premium_interactions":{"entitlement":300,"remaining":294},"remaining_only":{"remaining":12},"used_only":{"used":7}}}"#,
        ))
        .unwrap();
        assert!((parsed.windows[0].used - 0.02).abs() < 1e-9);
        assert_eq!(parsed.windows[1].id, "remaining_only");
        assert_eq!(parsed.windows[1].count, Some(12));
        assert!(parsed.windows[1].label.ends_with("remaining"));
        assert_eq!(parsed.windows[2].count, Some(7));
        assert!(parsed.windows[2].label.ends_with("used"));
    }

    #[test]
    fn environment_token_wins_and_hosts_identity_is_retained() {
        let hosts = parse_hosts(Some(
            "github.com:\n    user: octocat\n    oauth_token: hosts-token\n",
        ));
        let credential = load_credential_with(Some("env-token".into()), hosts, || {
            panic!("gh command must not run")
        })
        .unwrap();
        assert_eq!(credential.token, "env-token");
        assert_eq!(credential.username.as_deref(), Some("octocat"));
        assert_eq!(credential.source, "GitHub");
    }

    #[test]
    fn parses_quoted_github_cli_hosts_and_ignores_other_hosts() {
        let entry = parse_hosts(Some(
            "github.com:\n    user: 'octocat'\n    oauth_token: \"cli-token\"\nenterprise.example:\n    oauth_token: wrong\n",
        ));
        assert_eq!(entry.username.as_deref(), Some("octocat"));
        assert_eq!(entry.token.as_deref(), Some("cli-token"));
    }

    #[test]
    fn numeric_reset_accepts_seconds_or_milliseconds() {
        assert_eq!(
            parse_date(Some(&json("1789113600"))),
            Some(1_789_113_600_000)
        );
        assert_eq!(
            parse_date(Some(&json("1789113600000"))),
            Some(1_789_113_600_000)
        );
    }
}
