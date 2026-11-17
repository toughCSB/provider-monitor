//! Independent read-only usage polling for one additional Codex and Claude account.
use crate::{profiles::{self, Kind, Profile}, usage::UsageSnapshot, AppState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

static CODEX_REFRESH: AtomicBool = AtomicBool::new(false);
static CLAUDE_REFRESH: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BackoffRecord { profile_id: String, path: String, identity_tag: u64, until: u64 }

fn identity_tag(identity: &str) -> u64 {
    identity.as_bytes().iter().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3))
}

fn backoff_path(kind: Kind) -> std::path::PathBuf {
    crate::config::config_path().with_file_name(format!("{}-account-2-backoff.json", kind.id()))
}

fn saved_backoff(kind: Kind, profile: &Profile, identity: &str) -> u64 {
    saved_backoff_from(&backoff_path(kind), profile, identity)
}

fn saved_backoff_from(path: &std::path::Path, profile: &Profile, identity: &str) -> u64 {
    let Ok(raw) = std::fs::read_to_string(path) else { return 0 };
    let Ok(record) = serde_json::from_str::<BackoffRecord>(&raw) else { return 0 };
    if record.profile_id == profile.id && record.path == profile.dir.to_string_lossy()
        && record.identity_tag == identity_tag(identity) { record.until } else { 0 }
}

fn save_backoff(kind: Kind, profile: &Profile, identity: &str, until: u64) {
    save_backoff_to(&backoff_path(kind), profile, identity, until);
}

fn save_backoff_to(path: &std::path::Path, profile: &Profile, identity: &str, until: u64) {
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let record = BackoffRecord { profile_id: profile.id.clone(), path: profile.dir.to_string_lossy().into_owned(), identity_tag: identity_tag(identity), until };
    if let Ok(raw) = serde_json::to_vec(&record) { let _ = std::fs::write(path, raw); }
}

fn configured(app: &AppHandle, kind: Kind) -> (Option<String>, Option<String>, bool) {
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    if kind == Kind::Codex { (cfg.codex_profile_2.clone(), cfg.codex_auto_profile_2.clone(), cfg.codex_second_disconnected) }
    else { (cfg.claude_profile_2.clone(), cfg.claude_auto_profile_2.clone(), cfg.claude_second_disconnected) }
}

pub(crate) fn identity(kind: Kind, dir: &std::path::Path) -> Option<String> {
    match kind {
        Kind::Codex => crate::codex::load_credential_in(dir).map(|c| c.account_id),
        Kind::Claude => {
            let account_file = if dir.file_name().is_some_and(|n| n == ".claude") {
                dir.parent()?.join(".claude.json")
            } else { dir.join(".claude.json") };
            let raw = std::fs::read_to_string(account_file).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let account = v.get("oauthAccount")?;
            let email = account.get("emailAddress")?.as_str()?.trim().to_lowercase();
            if email.is_empty() { return None; }
            let org = account.get("organizationUuid").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
            Some(format!("claude:{email}\u{1f}{org}"))
        }
    }
}

/// A candidate is not a second account until its credential differs from account 1.
pub fn active(app: &AppHandle, kind: Kind) -> Option<Profile> {
    if crate::managed::has_provider(app, kind.id()) { return None; }
    let (manual, pinned, disconnected) = configured(app, kind);
    if disconnected { return None; }
    selected_with(app, kind, manual, pinned)
}

/// Settings keeps the registered slot even while monitoring is explicitly disconnected.
pub fn selected(app: &AppHandle, kind: Kind) -> Option<Profile> {
    if crate::managed::has_provider(app, kind.id()) { return None; }
    let (manual, pinned, disconnected) = configured(app, kind);
    if disconnected && manual.is_none() && pinned.is_none() { return None; }
    selected_with(app, kind, manual, pinned)
}

fn selected_with(app: &AppHandle, kind: Kind, manual: Option<String>, pinned: Option<String>) -> Option<Profile> {
    if let Some(path) = manual.as_deref() { return profiles::retained(kind, path, true); }
    if let Some(path) = pinned.as_deref() { return profiles::retained(kind, path, false); }
    let first = profiles::default_dir(kind).and_then(|d| identity(kind, &d));
    let selected = choose_distinct(kind, profiles::candidates(kind, None), first.as_deref())?;
    {
        let st = app.state::<AppState>();
        let mut cfg = st.cfg.lock().unwrap();
        let changed = if kind == Kind::Codex { cfg.codex_second_disconnected || cfg.codex_profile_2.is_some() }
            else { cfg.claude_second_disconnected || cfg.claude_profile_2.is_some() };
        if changed { return None; }
        let pin = if kind == Kind::Codex { &mut cfg.codex_auto_profile_2 } else { &mut cfg.claude_auto_profile_2 };
        if let Some(existing) = pin.as_deref() { return profiles::retained(kind, existing, false); }
        *pin = Some(selected.dir.to_string_lossy().into_owned());
        crate::config::save(&cfg);
    }
    Some(selected)
}

fn choose_distinct(kind: Kind, candidates: Vec<Profile>, first: Option<&str>) -> Option<Profile> {
    if kind == Kind::Claude && first.is_none() { return None; }
    candidates.into_iter().find(|profile| {
        identity(kind, &profile.dir).is_some_and(|second| first != Some(second.as_str()))
    })
}

pub fn request_refresh(kind: Kind) {
    let flag = if kind == Kind::Codex { &CODEX_REFRESH } else { &CLAUDE_REFRESH };
    flag.store(true, Ordering::Relaxed);
}

#[derive(Clone, serde::Serialize)]
struct AccountEvent<'a> { id: &'a str, provider: &'a str, snapshot: &'a UsageSnapshot }

fn publish(app: &AppHandle, kind: Kind, id: &str, snapshot: UsageSnapshot) {
    let st = app.state::<AppState>();
    st.second.lock().unwrap().insert(id.to_owned(), snapshot.clone());
    let _ = app.emit("account2", AccountEvent { id, provider: kind.id(), snapshot: &snapshot });
    crate::repaint_tray(app);
}

fn clear(app: &AppHandle, kind: Kind, id: &str) {
    let st = app.state::<AppState>();
    st.second.lock().unwrap().remove(id);
    let empty = UsageSnapshot { status: "absent".into(), ..Default::default() };
    let _ = app.emit("account2", AccountEvent { id, provider: kind.id(), snapshot: &empty });
    crate::repaint_tray(app);
}

fn codex_read(dir: &std::path::Path, old: &UsageSnapshot) -> UsageSnapshot {
    let Some(cred) = crate::codex::load_credential_in(dir) else {
        return UsageSnapshot { status: "needsAuth".into(), note: "Codex profile credential unavailable".into(), ..Default::default() };
    };
    if cred.expired { return stale(old, "Codex profile credential expired; sign in with that profile", 0); }
    let response = ureq::get("https://chatgpt.com/backend-api/wham/usage")
        .set("Authorization", &format!("Bearer {}", cred.access_token))
        .set("ChatGPT-Account-Id", &cred.account_id)
        .set("Accept", "application/json")
        .set("Cache-Control", "no-cache, no-store")
        .timeout(Duration::from_secs(15)).call();
    match response {
        Ok(r) => match r.into_json::<serde_json::Value>() {
            Ok(body) => {
                let windows = crate::codex::windows_from_usage(&body);
                if windows.is_empty() { stale(old, "Codex reported no usage windows", 0) }
                else { UsageSnapshot { status: "ok".into(), windows, fetched_at: now_ms(), ..Default::default() } }
            }
            Err(_) => stale(old, "Codex usage response invalid", 0),
        },
        Err(ureq::Error::Status(401 | 403, _)) => UsageSnapshot {
            status: "needsAuth".into(), note: "Codex profile sign-in rejected".into(), ..Default::default()
        },
        Err(ureq::Error::Status(429, r)) => {
            let secs = crate::polling::retry_after_secs(r.header("retry-after"), now_ms(), 60);
            stale(old, "Codex profile rate limited", crate::polling::deadline_ms(now_ms(), secs))
        }
        Err(_) => stale(old, "Codex profile usage unavailable", 0),
    }
}

fn claude_read(dir: &std::path::Path, old: &UsageSnapshot) -> UsageSnapshot {
    let Some(cred) = crate::usage::read_credentials_in(dir) else {
        return UsageSnapshot { status: "needsAuth".into(), note: "Claude profile credential unavailable".into(), ..Default::default() };
    };
    if cred.expired(now_ms()) { return stale(old, "Claude profile credential expired; sign in with that profile", 0); }
    let response = ureq::get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {}", cred.token))
        .set("anthropic-beta", "oauth-2025-04-20")
        .timeout(Duration::from_secs(15)).call();
    match response {
        Ok(r) => match r.into_json::<serde_json::Value>() {
            Ok(body) => {
                let windows = crate::usage::parse_response(&body);
                if windows.is_empty() { stale(old, "Claude reported no usage windows", 0) }
                else { UsageSnapshot { status: "ok".into(), windows, fetched_at: now_ms(), ..Default::default() } }
            }
            Err(_) => stale(old, "Claude usage response invalid", 0),
        },
        Err(ureq::Error::Status(401 | 403, _)) => UsageSnapshot {
            status: "needsAuth".into(), note: "Claude profile sign-in rejected".into(), ..Default::default()
        },
        Err(ureq::Error::Status(429, r)) => {
            let secs = crate::polling::retry_after_secs(r.header("retry-after"), now_ms(), 60);
            stale(old, "Claude profile rate limited", crate::polling::deadline_ms(now_ms(), secs))
        }
        Err(_) => stale(old, "Claude profile usage unavailable", 0),
    }
}

fn stale(old: &UsageSnapshot, note: &str, backoff_until: u64) -> UsageSnapshot {
    UsageSnapshot {
        status: if old.windows.is_empty() { "none" } else { "stale" }.into(),
        windows: old.windows.clone(), fetched_at: old.fetched_at,
        note: note.into(), backoff_until,
    }
}

fn start_one(app: AppHandle, kind: Kind) {
    std::thread::spawn(move || {
        let flag = if kind == Kind::Codex { &CODEX_REFRESH } else { &CLAUDE_REFRESH };
        let mut previous_profile: Option<Profile> = None;
        let mut previous_identity: Option<String> = None;
        let mut snapshot = UsageSnapshot::default();
        loop {
            let current = active(&app, kind);
            if current != previous_profile {
                if let Some(old) = previous_profile.take() { clear(&app, kind, &old.id); }
                snapshot = UsageSnapshot::default();
                previous_identity = None;
                previous_profile = current.clone();
            }
            if let Some(profile) = current {
                let current_identity = identity(kind, &profile.dir);
                if current_identity != previous_identity {
                    snapshot = UsageSnapshot::default();
                    previous_identity = current_identity.clone();
                    if let Some(ref account) = current_identity {
                        let until = saved_backoff(kind, &profile, account);
                        if until > now_ms() {
                            snapshot.status = "backoff".into();
                            snapshot.note = "Account 2 rate limited; waiting for retry deadline".into();
                            snapshot.backoff_until = until;
                        }
                    }
                }
                if let Some(ref account) = current_identity {
                    let default_identity = profiles::default_dir(kind).and_then(|d| identity(kind, &d));
                    if default_identity.as_deref() == Some(account.as_str()) || (kind == Kind::Claude && default_identity.is_none()) {
                        snapshot = UsageSnapshot { status: "needsAuth".into(), note: "Account 2 must be a different signed-in account".into(), ..Default::default() };
                    } else if snapshot.backoff_until <= now_ms() {
                        let next = if kind == Kind::Codex { codex_read(&profile.dir, &snapshot) }
                            else { claude_read(&profile.dir, &snapshot) };
                        // A profile can be switched or signed out while the HTTP request is in flight.
                        // Such a reply must never become this slot's usage or retry state.
                        if identity(kind, &profile.dir).as_deref() == Some(account.as_str())
                            && active(&app, kind).as_ref() == Some(&profile)
                            && profiles::default_dir(kind).and_then(|d| identity(kind, &d)).as_deref() != Some(account.as_str()) {
                            snapshot = next;
                            save_backoff(kind, &profile, account, snapshot.backoff_until);
                        } else {
                            snapshot = UsageSnapshot { status: "needsAuth".into(), note: "Account changed while reading usage".into(), ..Default::default() };
                        }
                    }
                } else {
                    snapshot = UsageSnapshot { status: "needsAuth".into(), note: "Account 2 signed out or identity unavailable".into(), ..Default::default() };
                }
                publish(&app, kind, &profile.id, snapshot.clone());
            }
            let seconds = if previous_profile.is_some() {
                crate::polling::poll_wait_secs(300, snapshot.backoff_until, now_ms())
            } else { 30 };
            for _ in 0..seconds {
                if flag.swap(false, Ordering::Relaxed) { break; }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

pub fn start(app: AppHandle) {
    start_one(app.clone(), Kind::Codex);
    start_one(app, Kind::Claude);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_candidate_is_skipped_without_reusing_its_usage() {
        let home = std::env::temp_dir().join(format!("provider-monitor-identity-{}", std::process::id()));
        let a = home.join(".codex-a"); let b = home.join(".codex-b");
        std::fs::create_dir_all(&a).unwrap(); std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("auth.json"), r#"{"tokens":{"access_token":"one","account_id":"same"}}"#).unwrap();
        std::fs::write(b.join("auth.json"), r#"{"tokens":{"access_token":"two","account_id":"different"}}"#).unwrap();
        let candidates = vec![Profile { id: "codex-a".into(), dir: a }, Profile { id: "codex-b".into(), dir: b }];
        assert_eq!(choose_distinct(Kind::Codex, candidates, Some("same")).unwrap().id, "codex-b");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn last_good_is_local_to_one_identity_and_keeps_its_retry_deadline() {
        let previous = UsageSnapshot { status: "ok".into(), windows: vec![crate::usage::LimitWindow { id: "secondary".into(), used: 0.4, ..Default::default() }], fetched_at: 123, ..Default::default() };
        let held = stale(&previous, "Rate limited", 456);
        assert_eq!(held.status, "stale");
        assert_eq!(held.windows[0].id, previous.windows[0].id);
        assert_eq!(held.windows[0].used, previous.windows[0].used);
        assert_eq!(held.backoff_until, 456);
        // The poller resets to Default when the profile or credential identity changes.
        assert!(stale(&UsageSnapshot::default(), "Unavailable", 0).windows.is_empty());
    }

    #[test]
    fn claude_identity_survives_token_rotation() {
        let home = std::env::temp_dir().join(format!("provider-monitor-claude-identity-{}", std::process::id()));
        let profile = home.join(".claude-work");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"Work@Example.com","organizationUuid":"Org"}}"#).unwrap();
        std::fs::write(profile.join(".credentials.json"), r#"{"claudeAiOauth":{"accessToken":"old"}}"#).unwrap();
        let before = identity(Kind::Claude, &profile);
        std::fs::write(profile.join(".credentials.json"), r#"{"claudeAiOauth":{"accessToken":"new"}}"#).unwrap();
        assert_eq!(before, identity(Kind::Claude, &profile));
        assert_eq!(before.as_deref(), Some("claude:work@example.com\u{1f}org"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn retry_deadline_restores_only_for_the_same_profile_and_account() {
        let root = std::env::temp_dir().join(format!("provider-monitor-backoff-{}", std::process::id()));
        let path = root.join("retry.json");
        let profile = Profile { id: "codex-work".into(), dir: root.join(".codex-work") };
        save_backoff_to(&path, &profile, "account-a", 987654);
        assert_eq!(saved_backoff_from(&path, &profile, "account-a"), 987654);
        assert_eq!(saved_backoff_from(&path, &profile, "account-b"), 0);
        assert_eq!(saved_backoff_from(&path, &Profile { id: profile.id.clone(), dir: root.join(".codex-other") }, "account-a"), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
