//! App-owned CLI accounts. The legacy default/profile readers remain separate.
use crate::{
    config::{self, ManagedAccount},
    usage::{LimitWindow, UsageSnapshot},
    AppState,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};

const POLL_SECS: u64 = 300;
const REQUEST_SECS: u64 = 20;
static NEXT: AtomicU64 = AtomicU64::new(0);
static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
static REFRESH: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
static PERSIST_LOCK: Mutex<()> = Mutex::new(());
static CODEX_GENERATION: AtomicU64 = AtomicU64::new(0);
static CLAUDE_GENERATION: AtomicU64 = AtomicU64::new(0);

fn generation_counter(provider: &str) -> &'static AtomicU64 {
    if provider == "codex" { &CODEX_GENERATION } else { &CLAUDE_GENERATION }
}
pub fn generation(provider: &str) -> u64 {
    generation_counter(provider).load(Ordering::Acquire)
}
pub fn with_legacy_source<R>(app: &AppHandle, provider: &str, expected: u64, publish: impl FnOnce() -> R) -> Option<R> {
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    if !legacy_source_allowed(&cfg.managed_accounts, provider, expected, generation(provider)) {
        return None;
    }
    Some(publish())
}

fn legacy_source_allowed(accounts: &[ManagedAccount], provider: &str, expected: u64, current: u64) -> bool {
    expected == current && !accounts.iter().any(|a| a.provider == provider && valid(&a.provider, a.slot))
}

fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}
fn refreshes() -> &'static Mutex<HashMap<String, bool>> {
    REFRESH.get_or_init(|| Mutex::new(HashMap::new()))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|x| x.as_millis() as u64)
        .unwrap_or(0)
}
fn id(provider: &str, slot: u8) -> String {
    format!("{provider}-managed-{slot}")
}
fn valid(provider: &str, slot: u8) -> bool {
    matches!(provider, "codex" | "claude") && (1..=2).contains(&slot)
}
fn root() -> PathBuf {
    config::config_path().with_file_name("managed-accounts")
}
fn snapshots_path() -> PathBuf {
    config::config_path().with_file_name("managed-snapshots.json")
}
fn save_config(cfg: &config::Config) -> Result<(), String> {
    let path = config::config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot save account registry")?;
    }
    let bytes = serde_json::to_vec_pretty(cfg).map_err(|_| "Cannot serialize account registry")?;
    fs::write(path, bytes).map_err(|_| "Cannot save account registry".into())
}

/// Only a canonical child of our private root can be passed to a CLI.
fn owned(dir: &Path) -> bool {
    if root()
        .symlink_metadata()
        .ok()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        return false;
    }
    let Ok(base) = root().canonicalize() else {
        return false;
    };
    let Ok(actual) = dir.canonicalize() else {
        return false;
    };
    actual.parent() == Some(base.as_path()) && actual.is_dir()
}

fn new_directory(provider: &str, slot: u8) -> Result<PathBuf, String> {
    fs::create_dir_all(root()).map_err(|_| "Cannot create private account storage")?;
    if root()
        .symlink_metadata()
        .ok()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        return Err("Private account storage is redirected".into());
    }
    for _ in 0..8 {
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = root().join(format!(
            "{provider}-{slot}-{}-{}-{nonce}",
            now_ms(),
            std::process::id()
        ));
        if fs::create_dir(&path).is_ok() && owned(&path) {
            return Ok(path);
        }
    }
    Err("Cannot allocate private account storage".into())
}

fn clean_pending_dir(dir: &Path, provider: &str) {
    if !owned(dir) {
        return;
    }
    // The only removable file is ours. CLI-created credentials and sessions are never deleted.
    if provider == "codex" {
        let config_file = dir.join("config.toml");
        if fs::read_to_string(&config_file).ok().as_deref()
            == Some("cli_auth_credentials_store = \"file\"\n")
        {
            let _ = fs::remove_file(config_file);
        }
    }
    let _ = fs::remove_dir(dir); // succeeds only when empty
}

fn cli(provider: &str) -> Result<PathBuf, String> {
    let path = if provider == "codex" {
        crate::codex::find_executable()
    } else {
        crate::usage::find_cli()
    };
    path.ok_or_else(|| format!("Official {provider} CLI is not installed"))
}

fn secret_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "CLAUDE_",
        "ANTHROPIC_",
        "CODEX_",
        "OPENAI_",
        "AZURE_OPENAI_",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

fn command(provider: &str, exe: &Path, dir: &Path) -> Command {
    let mut cmd = if exe
        .extension()
        .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("cmd"))
    {
        let mut shell = Command::new("cmd.exe");
        shell.args(["/d", "/c"]).arg(exe);
        shell
    } else {
        Command::new(exe)
    };
    cmd.current_dir(dir);
    for (name, _) in std::env::vars_os() {
        if secret_env(&name.to_string_lossy()) {
            cmd.env_remove(name);
        }
    }
    if provider == "codex" {
        cmd.env("CODEX_HOME", dir);
    } else {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd
}

#[cfg(windows)]
fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        // A .cmd wrapper may have spawned the real CLI. Limit the whole process tree.
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
    }
    let _ = child.wait();
}
#[cfg(not(windows))]
fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn capture(mut cmd: Command, timeout: Duration) -> Result<Value, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|_| "Official CLI could not start")?;
    let deadline = Instant::now() + timeout;
    let mut out = child.stdout.take().ok_or("CLI output unavailable")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = out.take(65_537).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let bytes = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(bytes)) if bytes.len() <= 65_536 => bytes,
        _ => {
            stop_child(&mut child);
            return Err("Official CLI timed out or returned too much data".into());
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                stop_child(&mut child);
                return Err("Official CLI did not exit before timeout".into());
            }
        }
    };
    let parsed: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Official CLI status was not JSON")?;
    // `claude auth status --json` exits 1 for signed-out accounts but still emits valid JSON.
    if !status.success() && parsed.get("loggedIn").is_none() {
        return Err("Official CLI returned an error".into());
    }
    Ok(parsed)
}

fn claude_status(dir: &Path) -> Result<Value, String> {
    let exe = cli("claude")?;
    let mut cmd = command("claude", &exe, dir);
    cmd.args(["--safe-mode", "auth", "status", "--json"]);
    let status = capture(cmd, Duration::from_secs(REQUEST_SECS))?;
    if let Some(reported) = status.get("configDirectory").and_then(Value::as_str) {
        let reported = Path::new(reported)
            .canonicalize()
            .map_err(|_| "Claude config directory could not be verified")?;
        let expected = dir
            .canonicalize()
            .map_err(|_| "Claude config directory could not be verified")?;
        if !reported
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.to_string_lossy())
        {
            return Err("Claude CLI did not use this account directory".into());
        }
    }
    Ok(status)
}

fn claude_identity(status: &Value) -> Option<String> {
    if status.get("loggedIn")?.as_bool()? != true {
        return None;
    }
    let account = status
        .get("email")
        .or_else(|| status.get("emailAddress"))?
        .as_str()?
        .trim()
        .to_lowercase();
    if account.is_empty() {
        return None;
    }
    let org = status
        .get("orgId")
        .or_else(|| status.get("organizationId"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    Some(format!("{account}\u{1f}{org}"))
}

fn capture_text(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|_| "Claude CLI could not start")?;
    let deadline = Instant::now() + timeout;
    let mut out = child.stdout.take().ok_or("Claude output unavailable")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = out.take(65_537).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let bytes = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(bytes)) if bytes.len() <= 65_536 => bytes,
        _ => {
            stop_child(&mut child);
            return Err("Claude /usage timed out or returned too much data".into());
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                stop_child(&mut child);
                return Err("Claude /usage did not exit before timeout".into());
            }
        }
    };
    if !status.success() {
        // A nonzero /usage exit can be a network or CLI failure. Only the
        // separate auth-status result may decide that this account signed out.
        return Err("Claude /usage could not complete; waiting to retry".into());
    }
    String::from_utf8(bytes).map_err(|_| "Claude /usage output was not UTF-8".into())
}

fn claude_usage(dir: &Path) -> Result<Vec<LimitWindow>, String> {
    let exe = cli("claude")?;
    let mut cmd = command("claude", &exe, dir);
    cmd.args([
        "--safe-mode",
        "--print",
        "--no-session-persistence",
        "--strict-mcp-config",
        "/usage",
    ]);
    let text = capture_text(cmd, Duration::from_secs(30))?;
    parse_claude_usage(&text)
}

fn parse_claude_usage(text: &str) -> Result<Vec<LimitWindow>, String> {
    parse_claude_usage_at(text, chrono::Utc::now())
}

fn parse_claude_usage_at(text: &str, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<LimitWindow>, String> {
    let mut windows = Vec::new();
    for line in text.lines() {
        let Some((head, body)) = line.trim().split_once(':') else {
            continue;
        };
        let kind = if head.eq_ignore_ascii_case("Current session") {
            "session".to_owned()
        } else if let Some(week) = head
            .strip_prefix("Current week (")
            .and_then(|x| x.strip_suffix(')'))
        {
            let slug = if week.eq_ignore_ascii_case("all models") {
                "all".to_owned()
            } else {
                week.to_ascii_lowercase().replace(' ', "_")
            };
            format!("weekly_{slug}")
        } else {
            continue;
        };
        let Some((percent, rest)) = body.trim().split_once("% used") else {
            continue;
        };
        let Ok(value) = percent.trim().parse::<f64>() else {
            continue;
        };
        if !value.is_finite()
            || !(0.0..=100.0).contains(&value)
            || windows.iter().any(|w: &LimitWindow| w.id == kind)
        {
            continue;
        }
        let label = if kind == "session" {
            "Current session".into()
        } else if kind == "weekly_all" {
            "All models".into()
        } else {
            head.trim_start_matches("Current week (")
                .trim_end_matches(')')
                .to_owned()
        };
        windows.push(LimitWindow {
            id: kind,
            label,
            used: value / 100.0,
            resets_at: rest.trim().strip_prefix('·').map(str::trim)
                .and_then(|s| s.strip_prefix("resets "))
                .and_then(|s| parse_claude_reset(s, now)),
            ..Default::default()
        });
    }
    if windows.is_empty() {
        return Err("Claude /usage did not return a usage limit".into());
    }
    windows.sort_by_key(|w| if w.id == "session" { 0 } else { 1 });
    Ok(windows)
}

fn parse_claude_reset(text: &str, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone};
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text.trim()) {
        return u64::try_from(date.timestamp_millis()).ok();
    }
    let (date_time, zone) = text.trim().rsplit_once(" (")?;
    let zone = zone.strip_suffix(')')?;
    // The CLI prints IANA names without offsets. Only zones whose offset is unambiguous
    // throughout the year can be converted without a timezone database.
    let seconds = match zone {
        "UTC" | "Etc/UTC" => 0,
        "Asia/Seoul" | "Asia/Tokyo" => 9 * 3600,
        "Asia/Jakarta" | "Asia/Bangkok" => 7 * 3600,
        "Asia/Singapore" => 8 * 3600,
        _ => return None,
    };
    let offset = chrono::FixedOffset::east_opt(seconds)?;
    let (date, clock) = date_time.split_once(" at ")?;
    let mut clock_text = clock.trim().to_ascii_lowercase();
    if !clock_text.contains(':') {
        for suffix in ["am", "pm"] {
            if let Some(hour) = clock_text.strip_suffix(suffix) {
                clock_text = format!("{}:00{suffix}", hour.trim());
                break;
            }
        }
    }
    let clock = NaiveTime::parse_from_str(&clock_text, "%I:%M%P")
        .or_else(|_| NaiveTime::parse_from_str(&clock_text, "%I:%M %P"))
        .or_else(|_| NaiveTime::parse_from_str(&clock_text, "%I%P"))
        .or_else(|_| NaiveTime::parse_from_str(&clock_text, "%I %P"))
        .or_else(|_| NaiveTime::parse_from_str(&clock_text, "%H:%M"))
        .ok()?;
    let parts: Vec<_> = date.trim().replace(',', "").split_whitespace().map(String::from).collect();
    if !(2..=3).contains(&parts.len()) { return None; }
    let month = chrono::NaiveDate::parse_from_str(&format!("{} 1 2000", parts[0]), "%b %d %Y").ok()?.month();
    let day = parts[1].parse::<u32>().ok()?;
    let years: Vec<i32> = if parts.len() == 3 {
        vec![parts[2].parse().ok()?]
    } else {
        let local_year = now.with_timezone(&offset).year();
        vec![local_year - 1, local_year, local_year + 1]
    };
    years.into_iter()
        .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
        .filter_map(|date| offset.from_local_datetime(&date.and_time(clock)).single())
        .map(|date| date.with_timezone(&chrono::Utc))
        // Like the Mac parser, retain a just-expired reset rather than turn it
        // into next year's deadline. No year is supplied by the CLI.
        .min_by_key(|date| date.signed_duration_since(now).num_seconds().unsigned_abs())
        .and_then(|date| u64::try_from(date.timestamp_millis()).ok())
}

#[cfg(windows)]
fn codex_process_job(child: &Child) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let handle = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let job = OwnedHandle::from_raw_handle(handle.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
        .ok()?;
        AssignProcessToJobObject(handle, HANDLE(child.as_raw_handle())).ok()?;
        Some(job)
    }
}

fn rpc(dir: &Path, include_limits: bool) -> Result<(Value, Option<Value>), String> {
    let exe = cli("codex")?;
    let mut cmd = command("codex", &exe, dir);
    cmd.arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| "Codex app-server could not start")?;
    // Attach the app-owned probe before sending it any requests.
    #[cfg(windows)]
    let job = codex_process_job(&child);
    let result = (|| {
        let stdout = child.stdout.take().ok_or("Codex output unavailable")?;
        let mut stdin = child.stdin.take().ok_or("Codex input unavailable")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.len() > 262_144 {
                    break;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let send = |stdin: &mut std::process::ChildStdin, v: Value| -> Result<(), String> {
            writeln!(stdin, "{v}")
                .and_then(|_| stdin.flush())
                .map_err(|_| "Codex app-server disconnected".into())
        };
        let receive = |want: i64| -> Result<Value, String> {
            let deadline = Instant::now() + Duration::from_secs(REQUEST_SECS);
            loop {
                let remain = deadline.saturating_duration_since(Instant::now());
                let v = rx
                    .recv_timeout(remain)
                    .map_err(|_| "Codex app-server timed out")?;
                if v.get("id").and_then(Value::as_i64) != Some(want) {
                    continue;
                }
                if v.get("error").is_some() {
                    return Err("Codex app-server request failed".into());
                }
                return v
                    .get("result")
                    .cloned()
                    .ok_or("Codex app-server result missing".into());
            }
        };
        send(
            &mut stdin,
            json!({"id":0,"method":"initialize","params":{"clientInfo":{"name":"provider_monitor","title":"Provider Monitor","version":env!("CARGO_PKG_VERSION")}}}),
        )?;
        receive(0)?;
        send(&mut stdin, json!({"method":"initialized"}))?;
        send(
            &mut stdin,
            json!({"id":1,"method":"account/read","params":{"refreshToken":false}}),
        )?;
        let before = receive(1)?;
        let before_identity = codex_identity(&before, dir);
        if before_identity.is_none() {
            return Err("Codex sign-in required".into());
        }
        if !include_limits {
            return Ok((before, None));
        }
        send(
            &mut stdin,
            json!({"id":2,"method":"account/rateLimits/read"}),
        )?;
        let limits = receive(2)?;
        send(
            &mut stdin,
            json!({"id":3,"method":"account/read","params":{"refreshToken":false}}),
        )?;
        let after = receive(3)?;
        if before_identity.is_none() || before_identity != codex_identity(&after, dir) {
            return Err("Codex identity changed while reading usage".into());
        }
        Ok((after, Some(limits)))
    })();

    #[cfg(windows)]
    if let Some(job) = job {
        drop(job);
        let _ = child.wait();
        return result;
    }
    stop_child(&mut child);
    result
}

fn codex_identity(result: &Value, dir: &Path) -> Option<String> {
    let account = result.get("account")?;
    if account.get("type")?.as_str()? != "chatgpt" {
        return None;
    }
    let email = account.get("email")?.as_str()?.trim().to_lowercase();
    if email.is_empty() {
        return None;
    }
    // Only an account identifier is read from this app-owned CLI home. No token leaves the CLI.
    let raw = fs::read_to_string(dir.join("auth.json")).ok()?;
    let auth: Value = serde_json::from_str(&raw).ok()?;
    let account_id = auth
        .get("tokens")?
        .get("account_id")?
        .as_str()?
        .trim()
        .to_lowercase();
    if account_id.is_empty() {
        return None;
    }
    if let Some(reported) = account.get("id").or_else(|| account.get("accountId")).and_then(Value::as_str) {
        if !reported.trim().eq_ignore_ascii_case(&account_id) { return None; }
    }
    Some(format!("{email}\u{1f}{account_id}"))
}

fn identity(provider: &str, dir: &Path) -> Result<String, String> {
    if !owned(dir) {
        return Err("Account directory is outside private storage".into());
    }
    if provider == "codex" {
        codex_identity(&rpc(dir, false)?.0, dir).ok_or("Codex login has no account identity".into())
    } else {
        claude_identity(&claude_status(dir)?).ok_or("Claude login has no account identity".into())
    }
}

fn parse_codex_limits(value: &Value) -> Vec<LimitWindow> {
    let result = value.get("rateLimitsByLimitId").and_then(Value::as_object);
    let buckets: Vec<(&str, &Value)> = if let Some(map) = result {
        map.iter().map(|(k, v)| (k.as_str(), v)).collect()
    } else {
        value
            .get("rateLimits")
            .map(|v| vec![("codex", v)])
            .unwrap_or_default()
    };
    let mut windows = Vec::new();
    for (bucket, body) in buckets {
        for (name, id_suffix) in [("primary", "primary"), ("secondary", "secondary")] {
            let Some(v) = body.get(name) else { continue };
            let Some(used) = v.get("usedPercent").and_then(Value::as_f64) else {
                continue;
            };
            if !used.is_finite() {
                continue;
            }
            let id = if bucket == "codex" {
                id_suffix.to_owned()
            } else {
                format!("{bucket}-{id_suffix}")
            };
            let duration = v.get("windowDurationSeconds").and_then(Value::as_f64)
                .map(|seconds| seconds / 60.0)
                .or_else(|| v.get("windowDurationMins").and_then(Value::as_f64))
                .filter(|minutes| minutes.is_finite() && *minutes > 0.0);
            windows.push(LimitWindow {
                id,
                label: crate::codex::label_for(duration, name),
                used: (used / 100.0).clamp(0.0, 1.0),
                resets_at: v
                    .get("resetsAt")
                    .and_then(Value::as_u64)
                    .map(|s| s.saturating_mul(1000)),
                group: (bucket != "codex").then(|| bucket.to_owned()),
                ..Default::default()
            });
        }
    }
    windows
}

fn snapshot(provider: &str, dir: &Path, pinned: &str, old: &UsageSnapshot) -> UsageSnapshot {
    let result = if provider == "codex" {
        rpc(dir, true).and_then(|(account, limits)| {
            let current = codex_identity(&account, dir).ok_or("Codex sign-in required")?;
            if current != pinned {
                return Err("Codex account changed; reauthenticate this slot".into());
            }
            let windows = parse_codex_limits(&limits.ok_or("Codex limits unavailable")?);
            if windows.is_empty() {
                return Err("Codex returned no rate limits".into());
            }
            Ok(UsageSnapshot {
                status: "ok".into(),
                windows,
                fetched_at: now_ms(),
                ..Default::default()
            })
        })
    } else {
        claude_windows_with(pinned, || claude_status(dir), || claude_usage(dir)).map(|windows| {
            UsageSnapshot {
                status: "ok".into(),
                windows,
                fetched_at: now_ms(),
                ..Default::default()
            }
        })
    };
    match result {
        Ok(v) => v,
        Err(note) => failed_snapshot(note, old),
    }
}

fn claude_windows_with<S, U>(pinned: &str, mut status: S, usage: U) -> Result<Vec<LimitWindow>, String>
where S: FnMut() -> Result<Value, String>, U: FnOnce() -> Result<Vec<LimitWindow>, String> {
    let current = claude_identity(&status()?).ok_or("Claude sign-in required")?;
    if current != pinned { return Err("Claude account changed; reauthenticate this slot".into()); }
    let result = usage();
    let after = claude_identity(&status()?).ok_or("Claude sign-in required")?;
    if after != current { return Err("Claude account changed while reading usage".into()); }
    result
}

fn failed_snapshot(note: String, old: &UsageSnapshot) -> UsageSnapshot {
    let auth = note.contains("sign-in")
        || note.contains("login")
        || note.contains("identity")
        || note.contains("changed")
        || note.contains("directory");
    UsageSnapshot {
        status: if auth {
            "needsAuth"
        } else if old.windows.is_empty() {
            "error"
        } else {
            "stale"
        }
        .into(),
        windows: if auth {
            Vec::new()
        } else {
            old.windows.clone()
        },
        fetched_at: if auth { 0 } else { old.fetched_at },
        note,
        backoff_until: now_ms().saturating_add(60_000),
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSnapshot {
    identity: String,
    snapshot: UsageSnapshot,
}

pub fn load_snapshots(accounts: &[ManagedAccount]) -> HashMap<String, UsageSnapshot> {
    let saved: HashMap<String, StoredSnapshot> = fs::read_to_string(snapshots_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    restored_snapshots(accounts, &saved)
}

fn restored_snapshots(accounts: &[ManagedAccount], saved: &HashMap<String, StoredSnapshot>) -> HashMap<String, UsageSnapshot> {
    accounts
        .iter()
        .filter(|a| valid(&a.provider, a.slot) && owned(&a.directory))
        .filter_map(|a| {
            let row = saved.get(&id(&a.provider, a.slot))?;
            (row.identity == a.identity).then(|| (id(&a.provider, a.slot), restored_snapshot(&row.snapshot)))
        })
        .collect()
}

fn restored_snapshot(saved: &UsageSnapshot) -> UsageSnapshot {
    let mut snapshot = saved.clone();
    if !snapshot.windows.is_empty() { snapshot.status = "stale".into(); }
    snapshot
}

fn persist(app: &AppHandle) {
    let _serial = PERSIST_LOCK.lock().unwrap();
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    let snapshots = st.second.lock().unwrap();
    let rows: HashMap<_, _> = cfg
        .managed_accounts
        .iter()
        .filter_map(|a| {
            snapshots.get(&id(&a.provider, a.slot)).map(|s| {
                (
                    id(&a.provider, a.slot),
                    StoredSnapshot {
                        identity: a.identity.clone(),
                        snapshot: s.clone(),
                    },
                )
            })
        })
        .collect();
    drop(snapshots);
    drop(cfg);
    if let Ok(bytes) = serde_json::to_vec(&rows) {
        let _ = fs::write(snapshots_path(), bytes);
    }
}

#[derive(Clone, Serialize)]
pub struct AccountView {
    pub id: String,
    pub provider: String,
    pub slot: u8,
    pub identity: String,
    pub label: String,
    pub disconnected: bool,
    pub snapshot: UsageSnapshot,
}
pub fn views(app: &AppHandle) -> Vec<AccountView> {
    let st = app.state::<AppState>();
    let cfg = st.cfg.lock().unwrap();
    let snaps = st.second.lock().unwrap();
    cfg.managed_accounts
        .iter()
        .filter(|a| valid(&a.provider, a.slot))
        .map(|a| AccountView {
            id: id(&a.provider, a.slot),
            provider: a.provider.clone(),
            slot: a.slot,
            identity: a.identity.clone(),
            label: a.label.clone(),
            disconnected: a.disconnected,
            snapshot: if !owned(&a.directory) {
                failed_snapshot("Account directory unavailable".into(), &UsageSnapshot::default())
            } else if a.disconnected {
                UsageSnapshot { status: "absent".into(), ..Default::default() }
            } else {
                snaps.get(&id(&a.provider, a.slot)).cloned().unwrap_or_default()
            },
        })
        .collect()
}
pub fn has_provider(app: &AppHandle, provider: &str) -> bool {
    app.state::<AppState>().cfg.lock().unwrap().managed_accounts.iter().any(|a| a.provider == provider && valid(&a.provider,a.slot))
}

struct Pending {
    provider: String,
    slot: u8,
    dir: PathBuf,
    child: Option<Child>,
    reauth: bool,
    started: Instant,
    state: String,
    message: String,
    identity: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct LoginState {
    pub provider: String,
    pub slot: u8,
    pub state: String,
    pub message: String,
    pub identity: Option<String>,
}
fn describe(p: &Pending) -> LoginState {
    LoginState {
        provider: p.provider.clone(),
        slot: p.slot,
        state: p.state.clone(),
        message: p.message.clone(),
        identity: p.identity.clone(),
    }
}
fn same_attempt(p: &Pending, dir: &Path, identity: Option<&str>, state: &str) -> bool {
    p.dir == dir && p.identity.as_deref() == identity && p.state == state
}
fn duplicate(accounts: &[ManagedAccount], provider: &str, slot: u8, identity: &str) -> bool {
    accounts
        .iter()
        .any(|a| a.provider == provider && a.slot != slot && a.identity == identity)
}

fn insert_slot(slots: &mut Vec<config::TraySlot>, provider: &str, slot: u8) {
    let new_id = id(provider, slot);
    if slots.iter().any(|s| s.provider == new_id) {
        return;
    }
    if slot == 1 {
        if let Some(s) = slots.iter_mut().find(|s| s.provider == provider) {
            s.provider = new_id;
            return;
        }
    }
    let predecessor = if slot == 2 {
        id(provider, 1)
    } else {
        provider.to_owned()
    };
    let index = slots
        .iter()
        .position(|s| s.provider == predecessor)
        .map(|x| x + 1)
        .unwrap_or(slots.len());
    slots.insert(index, config::TraySlot { provider: new_id });
}

fn remove_slot(slots: &mut Vec<config::TraySlot>, provider: &str, slot: u8, restore_legacy: bool) {
    let key = id(provider, slot);
    if let Some(index) = slots.iter().position(|s| s.provider == key) {
        slots.remove(index);
        if restore_legacy && !slots.iter().any(|s| s.provider == provider) {
            slots.insert(
                index,
                config::TraySlot {
                    provider: provider.into(),
                },
            );
        }
    }
}

pub fn start_login(app: &AppHandle, provider: String, slot: u8) -> Result<LoginState, String> {
    if !valid(&provider, slot) {
        return Err("Choose Codex or Claude slot 1 or 2".into());
    }
    let key = id(&provider, slot);
    let existing = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .managed_accounts
        .iter()
        .find(|a| a.provider == provider && a.slot == slot)
        .cloned();
    let reauth = existing.is_some();
    let mut map = pending().lock().unwrap();
    if map.contains_key(&key) {
        return Err("Login already in progress".into());
    }
    if let Some(a) = existing {
        if !owned(&a.directory) {
            return Err("Registered directory is not app-owned".into());
        }
    }
    // Reauthentication is transactional: the registered CLI home keeps working until confirmation.
    let dir = new_directory(&provider, slot)?;
    let exe = match cli(&provider) {
        Ok(x) => x,
        Err(e) => {
            clean_pending_dir(&dir, &provider);
            return Err(e);
        }
    };
    if provider == "codex" {
        fs::write(
            dir.join("config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .map_err(|_| "Cannot configure private Codex credential store")?;
    }
    let mut cmd = command(&provider, &exe, &dir);
    if provider == "codex" {
        cmd.arg("login");
    } else {
        cmd.args(["--safe-mode", "auth", "login", "--claudeai"]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0010);
    }
    let child = match cmd.spawn() {
        Ok(x) => x,
        Err(_) => {
            clean_pending_dir(&dir, &provider);
            return Err("Official CLI login could not start".into());
        }
    };
    let p = Pending {
        provider,
        slot,
        dir,
        child: Some(child),
        reauth,
        started: Instant::now(),
        state: "authenticating".into(),
        message: "Complete sign-in in the official CLI window".into(),
        identity: None,
    };
    let state = describe(&p);
    map.insert(key, p);
    Ok(state)
}

pub fn login_state(app: &AppHandle, provider: String, slot: u8) -> Result<LoginState, String> {
    if !valid(&provider, slot) {
        return Err("Invalid account slot".into());
    }
    let key = id(&provider, slot);
    let mut map = pending().lock().unwrap();
    let p = map.get_mut(&key).ok_or("No login in progress")?;
    if p.state == "authenticating" {
        if p.started.elapsed() > Duration::from_secs(600) {
            if let Some(mut child) = p.child.take() {
                stop_child(&mut child);
            }
            p.state = "failed".into();
            p.message = "Login timed out".into();
        } else if let Some(child) = p.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    p.child.take();
                    if exit.success() {
                        // Identity lookup can be slow. Run outside the registry lock below.
                        p.state = "checking".into();
                    } else {
                        p.state = "failed".into();
                        p.message = "Official CLI login failed or was cancelled".into();
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    p.state = "failed".into();
                    p.message = "Cannot read login process state".into();
                }
            }
        }
    }
    if p.state == "checking" {
        let dir = p.dir.clone();
        let provider = p.provider.clone();
        drop(map);
        let found = identity(&provider, &dir);
        let mut map = pending().lock().unwrap();
        let p = map.get_mut(&key).ok_or("Login was cancelled")?;
        if !same_attempt(p, &dir, None, "checking") {
            return Err("Login changed while checking identity".into());
        }
        match found {
            Ok(identity) => {
                let cfg = app.state::<AppState>();
                let accounts = cfg.cfg.lock().unwrap();
                if duplicate(&accounts.managed_accounts, &provider, slot, &identity) {
                    p.state = "failed".into();
                    p.message = "This account is already monitored".into();
                } else {
                    p.state = "ready".into();
                    p.message = "Confirm this account to register it".into();
                    p.identity = Some(identity);
                }
            }
            Err(e) => {
                p.state = "failed".into();
                p.message = e;
            }
        }
        return Ok(describe(p));
    }
    Ok(describe(p))
}

pub fn confirm(app: &AppHandle, provider: String, slot: u8, label: String) -> Result<(), String> {
    let key = id(&provider, slot);
    let (dir, expected, reauth) = {
        let map = pending().lock().unwrap();
        let p = map.get(&key).ok_or("No login to confirm")?;
        if p.state != "ready" {
            return Err("Finish official login before confirming".into());
        }
        (
            p.dir.clone(),
            p.identity.clone().ok_or("Account identity unavailable")?,
            p.reauth,
        )
    };
    let actual = identity(&provider, &dir)?;
    if actual != expected {
        return Err("Account identity changed before confirmation".into());
    }
    let mut map = pending().lock().unwrap();
    let current = map.get(&key).ok_or("Login was cancelled")?;
    if !same_attempt(current, &dir, Some(&expected), "ready") {
        return Err("Login changed before confirmation".into());
    }
    let st = app.state::<AppState>();
    let mut cfg = st.cfg.lock().unwrap();
    let before = cfg.clone();
    if duplicate(&cfg.managed_accounts, &provider, slot, &actual) {
        return Err("This account is already monitored".into());
    }
    if reauth {
        let account = cfg
            .managed_accounts
            .iter_mut()
            .find(|a| a.provider == provider && a.slot == slot)
            .ok_or("Registered slot disappeared")?;
        if account.identity != actual {
            return Err("Signed-in identity differs from this slot".into());
        }
        account.directory = dir;
        account.disconnected = false;
    } else {
        if cfg
            .managed_accounts
            .iter()
            .any(|a| a.provider == provider && a.slot == slot)
        {
            return Err("Slot already registered".into());
        }
        let label = label.trim();
        cfg.managed_accounts.push(ManagedAccount {
            provider: provider.clone(),
            slot,
            directory: dir,
            identity: actual.clone(),
            label: if label.is_empty() {
                format!(
                    "{} {slot}",
                    if provider == "codex" {
                        "Codex"
                    } else {
                        "Claude"
                    }
                )
            } else {
                label.chars().take(48).collect()
            },
            disconnected: false,
        });
        insert_slot(&mut cfg.tray_slots, &provider, slot);
        insert_slot(&mut cfg.notch_slots, &provider, slot);
    }
    if let Err(e) = save_config(&cfg) {
        *cfg = before;
        return Err(e);
    }
    generation_counter(&provider).fetch_add(1, Ordering::AcqRel);
    let notch_slots = cfg.notch_slots.clone();
    st.second.lock().unwrap().remove(&key);
    drop(cfg);
    if provider == "claude" { crate::usage::request_refresh(); } else { crate::codex::request_refresh(); }
    map.remove(&key);
    drop(map);
    request_refresh(&key);
    let empty = UsageSnapshot::default();
    let _ = app.emit(
        "account2",
        AccountEvent {
            id: &key,
            provider: &provider,
            snapshot: &empty,
        },
    );
    let _ = app.emit("notch_slots", &notch_slots);
    crate::repaint_tray(app);
    Ok(())
}

pub fn cancel(provider: String, slot: u8) -> Result<(), String> {
    if !valid(&provider, slot) {
        return Err("Invalid account slot".into());
    }
    if let Some(mut p) = pending().lock().unwrap().remove(&id(&provider, slot)) {
        if let Some(mut child) = p.child.take() {
            stop_child(&mut child);
        }
        clean_pending_dir(&p.dir, &provider);
    }
    Ok(())
}

pub fn disconnect(
    app: &AppHandle,
    provider: String,
    slot: u8,
    disconnected: bool,
) -> Result<(), String> {
    if !valid(&provider, slot) {
        return Err("Invalid account slot".into());
    }
    let st = app.state::<AppState>();
    let mut cfg = st.cfg.lock().unwrap();
    let before = cfg.clone();
    let a = cfg
        .managed_accounts
        .iter_mut()
        .find(|a| a.provider == provider && a.slot == slot)
        .ok_or("Account is not registered")?;
    a.disconnected = disconnected;
    if let Err(e) = save_config(&cfg) {
        *cfg = before;
        return Err(e);
    }
    if disconnected {
        st.second.lock().unwrap().remove(&id(&provider, slot));
    }
    drop(cfg);
    request_refresh(&id(&provider, slot));
    let key = id(&provider, slot);
    let snapshot = if disconnected {
        UsageSnapshot {
            status: "absent".into(),
            ..Default::default()
        }
    } else {
        UsageSnapshot::default()
    };
    let _ = app.emit(
        "account2",
        AccountEvent {
            id: &key,
            provider: &provider,
            snapshot: &snapshot,
        },
    );
    crate::repaint_tray(app);
    Ok(())
}

pub fn remove(app: &AppHandle, provider: String, slot: u8) -> Result<(), String> {
    if !valid(&provider, slot) {
        return Err("Invalid account slot".into());
    }
    cancel(provider.clone(), slot)?;
    let st = app.state::<AppState>();
    let mut cfg = st.cfg.lock().unwrap();
    let previous = cfg.clone();
    let before = cfg.managed_accounts.len();
    cfg.managed_accounts
        .retain(|a| !(a.provider == provider && a.slot == slot));
    if cfg.managed_accounts.len() == before {
        return Err("Account is not registered".into());
    }
    let restore = !cfg.managed_accounts.iter().any(|a| a.provider == provider);
    remove_slot(&mut cfg.tray_slots, &provider, slot, restore);
    remove_slot(&mut cfg.notch_slots, &provider, slot, restore);
    let notch_slots = cfg.notch_slots.clone();
    if let Err(e) = save_config(&cfg) {
        *cfg = previous;
        return Err(e);
    }
    generation_counter(&provider).fetch_add(1, Ordering::AcqRel);
    st.second.lock().unwrap().remove(&id(&provider, slot));
    drop(cfg);
    if provider == "claude" { crate::usage::request_refresh(); } else { crate::codex::request_refresh(); }
    persist(app);
    let key = id(&provider, slot);
    let absent = UsageSnapshot {
        status: "absent".into(),
        ..Default::default()
    };
    let _ = app.emit(
        "account2",
        AccountEvent {
            id: &key,
            provider: &provider,
            snapshot: &absent,
        },
    );
    let _ = app.emit("notch_slots", &notch_slots);
    crate::multi::request_refresh(if provider == "codex" {
        crate::profiles::Kind::Codex
    } else {
        crate::profiles::Kind::Claude
    });
    crate::repaint_tray(app);
    Ok(())
}

pub fn request_refresh(key: &str) {
    refreshes().lock().unwrap().insert(key.to_owned(), true);
}

#[derive(Clone, Serialize)]
struct AccountEvent<'a> {
    id: &'a str,
    provider: &'a str,
    snapshot: &'a UsageSnapshot,
}

fn start_one(app: AppHandle, provider: &'static str, slot: u8) {
    std::thread::spawn(move || {
        let key = id(provider, slot);
        loop {
            let account = app
                .state::<AppState>()
                .cfg
                .lock()
                .unwrap()
                .managed_accounts
                .iter()
                .find(|a| a.provider == provider && a.slot == slot)
                .cloned();
            if let Some(a) = account.filter(|a| !a.disconnected) {
                let old = app
                    .state::<AppState>()
                    .second
                    .lock()
                    .unwrap()
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let next = if !owned(&a.directory) {
                    failed_snapshot("Account directory unavailable".into(), &old)
                } else if old.backoff_until > now_ms() {
                    old
                } else {
                    snapshot(provider, &a.directory, &a.identity, &old)
                };
                let st = app.state::<AppState>();
                let cfg = st.cfg.lock().unwrap();
                let still = cfg.managed_accounts.iter().any(|x| {
                    x.provider == provider
                        && x.slot == slot
                        && x.identity == a.identity
                        && x.directory == a.directory
                        && !x.disconnected
                });
                // A reauthentication or disconnect during an in-flight read must not publish its reply.
                if still {
                    st.second.lock().unwrap().insert(key.clone(), next.clone());
                    let _ = app.emit(
                        "account2",
                        AccountEvent {
                            id: &key,
                            provider,
                            snapshot: &next,
                        },
                    );
                }
                drop(cfg);
                if still {
                    persist(&app);
                    crate::repaint_tray(&app);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(POLL_SECS);
            while Instant::now() < deadline {
                if refreshes().lock().unwrap().remove(&key).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

pub fn start(app: AppHandle) {
    for provider in ["codex", "claude"] {
        for slot in 1..=2 {
            start_one(app.clone(), provider, slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_result_uses_documented_windows() {
        let data = json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":25.0,"windowDurationMins":300,"resetsAt":1730947200},"secondary":null}}});
        let rows = parse_codex_limits(&data);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "primary");
        assert_eq!(rows[0].used, 0.25);
        assert_eq!(rows[0].resets_at, Some(1730947200000));
        assert_eq!(rows[0].label, "5h limit");
    }

    #[test]
    fn codex_duration_labels_match_existing_accounts_and_keep_reset_deadline() {
        for (minutes, label) in [(10080, "Weekly limit"), (43200, "Monthly limit"), (100800, "70d limit")] {
            let data=json!({"rateLimits":{"primary":{"usedPercent":12,"windowDurationMins":minutes,"resetsAt":1800000000}}});
            let rows=parse_codex_limits(&data);
            assert_eq!(rows[0].label,label);
            assert_eq!(rows[0].resets_at,Some(1800000000000));
        }
        let data=json!({"rateLimits":{"primary":{"usedPercent":12,"windowDurationSeconds":604800,"resetsAt":1800000000}}});
        assert_eq!(parse_codex_limits(&data)[0].label,"Weekly limit");
    }
    #[test]
    fn account_identities_require_official_result_and_are_stable() {
        let dir =
            std::env::temp_dir().join(format!("provider-managed-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("auth.json"),
            r#"{"tokens":{"account_id":"Workspace-A"}}"#,
        )
        .unwrap();
        assert_eq!(
            codex_identity(
                &json!({"account":{"type":"chatgpt","email":"A@EXAMPLE.COM","planType":"pro"}}),
                &dir
            ),
            Some("a@example.com\u{1f}workspace-a".into())
        );
        assert_eq!(codex_identity(&json!({"account":{"type":"chatgpt","email":"a@example.com","id":"workspace-b"}}), &dir), None);
        assert_eq!(
            codex_identity(&json!({"account":{"type":"apiKey"}}), &dir),
            None
        );
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            claude_identity(&json!({"loggedIn":false,"email":"a@example.com"})),
            None
        );
        assert_eq!(
            claude_identity(&json!({"loggedIn":true,"email":"A@EXAMPLE.COM","orgId":"Org"})),
            Some("a@example.com\u{1f}org".into())
        );
    }

    #[test]
    fn claude_usage_failure_does_not_invent_a_logout_but_real_logout_clears_usage() {
        let status=json!({"loggedIn":true,"authMethod":"claude.ai","email":"a@example.test","orgId":"org"});
        let pinned=claude_identity(&status).unwrap();
        let old=UsageSnapshot {status:"ok".into(),fetched_at:123,windows:vec![LimitWindow{id:"session".into(),used:0.4,..Default::default()}],..Default::default()};
        let failure=claude_windows_with(&pinned, || Ok(status.clone()), || Err("Claude /usage could not complete; waiting to retry".into())).unwrap_err();
        let stale=failed_snapshot(failure,&old);
        assert_eq!(stale.status,"stale");
        assert_eq!(stale.fetched_at,123);
        assert_eq!(stale.windows[0].used,0.4);
        let mut calls=0;
        let failure=claude_windows_with(&pinned, || {calls+=1; Ok(if calls==1 {status.clone()} else {json!({"loggedIn":false})})}, || Err("network unavailable".into())).unwrap_err();
        let logged_out=failed_snapshot(failure,&old);
        assert_eq!(logged_out.status,"needsAuth");
        assert!(logged_out.windows.is_empty());
    }
    #[test]
    fn failed_read_keeps_last_good_timestamp() {
        let old = UsageSnapshot {
            status: "ok".into(),
            fetched_at: 123,
            windows: vec![LimitWindow {
                id: "primary".into(),
                used: 0.4,
                ..Default::default()
            }],
            ..Default::default()
        };
        let next = failed_snapshot("Codex app-server timed out".into(), &old);
        assert_eq!(next.fetched_at, 123);
        assert_eq!(next.windows[0].used, 0.4);
        let changed = failed_snapshot("Codex account changed while reading usage".into(), &old);
        assert_eq!(changed.status, "needsAuth");
        assert!(changed.windows.is_empty());
        assert_eq!(changed.fetched_at, 0);
    }
    #[test]
    fn claude_usage_accepts_only_limit_lines() {
        let text="Current session: 38% used · resets Sep 7 at 2:59pm (Asia/Jakarta)\nCurrent week (all models): 4% used\n37% of your usage was at >150k context";
        let windows = parse_claude_usage(text).unwrap();
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["session", "weekly_all"]
        );
        assert_eq!(windows[0].used, 0.38);
        assert!(parse_claude_usage("Please run /login first").is_err());
    }
    #[test]
    fn claude_weekly_only_and_reset_times_do_not_invent_a_session() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-12-31T23:00:00Z").unwrap().to_utc();
        let text = "Current week (all models): 34% used · resets Jan 7 at 2:59pm (Asia/Seoul)\nCurrent week (Opus): 12% used · resets Jan 7, 2027 at 2:59pm (Asia/Jakarta)";
        let rows = parse_claude_usage_at(text, now).unwrap();
        assert_eq!(rows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["weekly_all", "weekly_opus"]);
        assert_eq!(rows[0].used, 0.34);
        assert_eq!(rows[0].resets_at, parse_claude_reset("2027-01-07T14:59:00+09:00", now));
        assert_eq!(rows[1].resets_at, parse_claude_reset("2027-01-07T14:59:00+07:00", now));
        assert_eq!(parse_claude_reset("Jan 7 at 2:59pm (America/New_York)", now), None);
        let january = chrono::DateTime::parse_from_rfc3339("2027-01-02T01:00:00Z").unwrap().to_utc();
        assert_eq!(parse_claude_reset("Dec 31 at 3pm (Asia/Seoul)", january),
                   parse_claude_reset("2026-12-31T15:00:00+09:00", january));
    }
    #[test]
    fn restored_managed_reading_is_stale_but_keeps_measurement_and_retry() {
        let snapshot = UsageSnapshot {status:"ok".into(),fetched_at:123,backoff_until:456,windows:vec![LimitWindow{id:"weekly_all".into(),used:0.4,..Default::default()}],..Default::default()};
        let restored = restored_snapshot(&snapshot);
        assert_eq!(restored.status,"stale");
        assert_eq!(restored.fetched_at,123);
        assert_eq!(restored.backoff_until,456);
        assert_eq!(restored.windows[0].used,0.4);
    }
    #[test]
    fn cancel_or_restart_cannot_reuse_an_old_login_result() {
        let a = PathBuf::from("C:/private/attempt-a");
        let b = PathBuf::from("C:/private/attempt-b");
        let p = Pending {
            provider: "codex".into(),
            slot: 1,
            dir: b,
            child: None,
            reauth: false,
            started: Instant::now(),
            state: "ready".into(),
            message: String::new(),
            identity: Some("second".into()),
        };
        assert!(!same_attempt(&p, &a, Some("first"), "ready"));
        assert!(!same_attempt(&p, &a, None, "checking"));
    }
    #[test]
    fn duplicate_registration_and_env_scope() {
        let a = ManagedAccount {
            provider: "codex".into(),
            slot: 1,
            directory: PathBuf::from("C:/private/a"),
            identity: "same".into(),
            label: "A".into(),
            disconnected: false,
        };
        assert!(duplicate(&[a.clone()], "codex", 2, "same"));
        assert!(!duplicate(&[a], "claude", 1, "same"));
        assert!(secret_env("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(secret_env("AZURE_OPENAI_API_KEY"));
        let original = std::env::var_os("CODEX_HOME");
        let cmd = command("codex", Path::new("codex.exe"), Path::new("C:/private/a"));
        assert_eq!(std::env::var_os("CODEX_HOME"), original);
        assert!(cmd
            .get_envs()
            .any(|(name, value)| name == "CODEX_HOME" && value.is_some()));
    }
    #[test]
    fn legacy_reply_is_rejected_after_registration_generation_changes() {
        let account = ManagedAccount {provider:"claude".into(),slot:1,directory:PathBuf::from("private"),identity:"a".into(),label:"A".into(),disconnected:true};
        assert!(legacy_source_allowed(&[],"claude",1,1));
        assert!(!legacy_source_allowed(std::slice::from_ref(&account),"claude",1,1));
        assert!(!legacy_source_allowed(&[],"claude",1,2));
        assert!(legacy_source_allowed(&[],"claude",2,2));
        assert!(legacy_source_allowed(std::slice::from_ref(&account),"codex",1,1));
    }
}
