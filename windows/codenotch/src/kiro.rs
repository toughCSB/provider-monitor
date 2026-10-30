//! Kiro usage adapter, ported from `Sources/Providers/KiroCredentials.swift`,
//! `KiroUsage.swift`, and `KiroProvider.swift`.
//!
//! The primary reading comes from `kiro-cli chat --no-interactive /usage`. The CLI owns sign-in
//! and token refresh. Provider Monitor may additionally read the CLI's SQLite state in read-only,
//! query-only mode to enrich the CLI card with plan/overage limits; it never writes or refreshes a
//! credential, and it never logs CLI output or token values.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OpenFlags};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const POLL_SECS: u64 = 300;
const ABSENT_POLL_SECS: u64 = 600;
const CLI_TIMEOUT_SECS: u64 = 20;
const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_CAP_SECS: u64 = 900;

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CONSECUTIVE_429: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn request_refresh() {
    REFRESH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    if path == "~" {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn state_database_path_from(
    override_dir: Option<&str>,
    data_dir: Option<&Path>,
    local_data_dir: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(directory) = nonempty(override_dir) {
        return Some(expand_tilde(directory, home).join("data.sqlite3"));
    }
    // `dirs::data_dir()` maps the macOS Application Support convention to Windows Roaming AppData.
    // Prefer an existing database, but also accept Local AppData used by some Windows installers.
    let candidates = [
        data_dir.map(|path| path.join("kiro-cli").join("data.sqlite3")),
        local_data_dir.map(|path| path.join("kiro-cli").join("data.sqlite3")),
        home.map(|path| path.join(".kiro-cli").join("data.sqlite3")),
    ];
    candidates
        .iter()
        .flatten()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| candidates.into_iter().flatten().next())
}

fn state_database_path() -> Option<PathBuf> {
    let home = dirs::home_dir();
    let data = dirs::data_dir();
    let local = dirs::data_local_dir();
    state_database_path_from(
        std::env::var("KIRO_DATA_DIR").ok().as_deref(),
        data.as_deref(),
        local.as_deref(),
        home.as_deref(),
    )
}

fn binary_candidates(home: Option<&Path>, local_data: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        for name in ["kiro-cli.exe", "kiro-cli.cmd", "kiro-cli"] {
            candidates.push(home.join(".local").join("bin").join(name));
        }
    }
    if let Some(local) = local_data {
        for prefix in [
            local.join("kiro-cli").join("bin"),
            local.join("Programs").join("Kiro").join("bin"),
            local.join("Kiro").join("bin"),
        ] {
            candidates.push(prefix.join("kiro-cli.exe"));
            candidates.push(prefix.join("kiro-cli.cmd"));
        }
    }
    candidates
}

fn runnable(path: &Path) -> bool {
    path.is_absolute() && path.is_file()
}

fn locate_binary_from(
    override_path: Option<&str>,
    home: Option<&Path>,
    local_data: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(override_path) = nonempty(override_path) {
        let candidate = expand_tilde(override_path, home);
        return runnable(&candidate).then_some(candidate);
    }
    if let Some(found) = binary_candidates(home, local_data)
        .into_iter()
        .find(|candidate| runnable(candidate))
    {
        return Some(found);
    }
    let path_env = path_env?;
    for directory in std::env::split_paths(path_env).filter(|path| path.is_absolute()) {
        for name in ["kiro-cli.exe", "kiro-cli.cmd", "kiro-cli"] {
            let candidate = directory.join(name);
            if runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn locate_binary() -> Option<PathBuf> {
    let home = dirs::home_dir();
    let local = dirs::data_local_dir();
    locate_binary_from(
        std::env::var("KIRO_CLI_PATH").ok().as_deref(),
        home.as_deref(),
        local.as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("kiro.json")
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

fn sqlite_json_value(database: &Path, table: &str, key: &str, fields: &[&str]) -> Option<String> {
    if !matches!(table, "auth_kv" | "state") || !database.is_file() {
        return None;
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection.pragma_update(None, "query_only", "ON").ok()?;
    let sql = format!("SELECT value FROM {table} WHERE key = ?1 LIMIT 1");
    let json: String = connection.query_row(&sql, [key], |row| row.get(0)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    fields
        .iter()
        .find_map(|field| nonempty(value.get(*field)?.as_str()).map(String::from))
}

fn access_token(database: &Path) -> Option<String> {
    sqlite_json_value(
        database,
        "auth_kv",
        "kirocli:odic:token",
        &["access_token", "accessToken"],
    )
}

fn profile_arn(database: &Path) -> Option<String> {
    sqlite_json_value(database, "state", "api.codewhisperer.profile", &["arn"])
}

pub fn present() -> bool {
    locate_binary().is_some()
        || state_database_path()
            .as_deref()
            .and_then(access_token)
            .is_some()
}

/// For doctor output. It reports only presence and never a token, token length, or CLI output.
pub fn probe() -> String {
    let binary = locate_binary();
    let database = state_database_path();
    let has_token = database.as_deref().and_then(access_token).is_some();
    match (binary, database, has_token) {
        (Some(binary), Some(database), true) => format!(
            "Kiro: CLI at {}; read-only session in {}",
            binary.display(),
            database.display()
        ),
        (Some(binary), _, false) => format!(
            "Kiro: CLI at {}; no readable session (run kiro-cli login)",
            binary.display()
        ),
        (None, Some(database), true) => format!(
            "Kiro: session found in {}, but kiro-cli is not installed",
            database.display()
        ),
        _ => "Kiro: kiro-cli and its session were not found".into(),
    }
}

#[derive(Debug)]
enum AdapterErr {
    NeedsAuth,
    NothingMetered(String),
    TimedOut,
    Other(String),
}

fn command_for(binary: &Path) -> Command {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let is_cmd = binary
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("cmd"))
            .unwrap_or(false);
        let mut command = if is_cmd {
            let mut shell = Command::new("cmd.exe");
            shell.arg("/D").arg("/S").arg("/C").arg(binary);
            shell
        } else {
            Command::new(binary)
        };
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        command
    };
    #[cfg(not(windows))]
    let mut command = Command::new(binary);

    command
        .arg("chat")
        .arg("--no-interactive")
        .arg("/usage")
        .env("TERM", "dumb")
        .env("KIRO_CHAT_UI", "classic")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let pid = pid.to_string();
    let _ = Command::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .creation_flags(0x0800_0000)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_process_tree(_: u32) {}

fn choose_cli_output(stdout: String, stderr: String) -> Option<String> {
    if stdout.contains("Estimated Usage") {
        return Some(stdout);
    }
    if stderr.contains("Estimated Usage") {
        return Some(stderr);
    }
    if !stdout.trim().is_empty() {
        return Some(stdout);
    }
    if !stderr.trim().is_empty() {
        return Some(stderr);
    }
    None
}

fn run_cli(binary: &Path) -> Result<String, AdapterErr> {
    let mut child = command_for(binary)
        .spawn()
        .map_err(|error| AdapterErr::Other(format!("kiro-cli could not start: {error}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stdout {
            let _ = stream.read_to_end(&mut bytes);
        }
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = stream.read_to_end(&mut bytes);
        }
        bytes
    });

    let deadline = Instant::now() + Duration::from_secs(CLI_TIMEOUT_SECS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                kill_process_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(AdapterErr::TimedOut);
            }
            Err(error) => return Err(AdapterErr::Other(format!("kiro-cli wait failed: {error}"))),
        }
    };
    let stdout = String::from_utf8_lossy(&stdout_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).into_owned();
    if !status.success() {
        return Err(AdapterErr::NeedsAuth);
    }
    choose_cli_output(stdout, stderr)
        .ok_or_else(|| AdapterErr::Other("kiro-cli returned no usage output".into()))
}

fn strip_ansi(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '\u{1b}' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        index += 1;
        let Some(next) = chars.get(index).copied() else {
            break;
        };
        match next {
            '[' => {
                index += 1;
                while index < chars.len() {
                    let code = chars[index] as u32;
                    index += 1;
                    if (0x40..=0x7e).contains(&code) {
                        break;
                    }
                }
            }
            ']' => {
                index += 1;
                while index < chars.len() {
                    if chars[index] == '\u{7}' {
                        index += 1;
                        break;
                    }
                    if chars[index] == '\u{1b}' && chars.get(index + 1) == Some(&'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            '(' | ')' => index = (index + 2).min(chars.len()),
            c if ('@'..='_').contains(&c) => index += 1,
            _ => index += 1,
        }
    }
    output
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn parse_number_prefix(text: &str) -> Option<f64> {
    let token: String = text
        .trim_start()
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect();
    (!token.is_empty())
        .then(|| token.parse::<f64>().ok())
        .flatten()
}

fn number_before(text: &str, marker: &str) -> Option<f64> {
    let end = text.find(marker)?;
    let before = &text[..end];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_digit() || *character == '.')
        .last()
        .map(|(index, _)| index)?;
    before[start..].parse().ok()
}

fn normalize_plan(raw: &str) -> Option<String> {
    let stripped = strip_ansi(raw);
    let words: Vec<&str> = stripped.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    if !words.iter().any(|word| word.eq_ignore_ascii_case("kiro")) {
        return Some(words.join(" "));
    }
    Some(
        words
            .into_iter()
            .map(|word| {
                if word.eq_ignore_ascii_case("kiro") {
                    return "Kiro".into();
                }
                let lowercase = word.to_ascii_lowercase();
                let mut characters = lowercase.chars();
                characters
                    .next()
                    .map(|first| format!("{}{}", first.to_ascii_uppercase(), characters.as_str()))
                    .unwrap_or_default()
            })
            .collect::<Vec<String>>()
            .join(" "),
    )
}

fn plan_name(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(index) = find_ascii_case_insensitive(line, "Plan:") {
            let tail = line[index + "Plan:".len()..].trim();
            let raw = tail.split('|').next().unwrap_or(tail).trim();
            if !raw.is_empty() {
                return normalize_plan(raw);
            }
        }
    }
    for line in text.lines() {
        if find_ascii_case_insensitive(line, "Estimated Usage").is_some() {
            if let Some(raw) = line.split('|').nth(2).and_then(normalize_plan) {
                return Some(raw);
            }
        }
    }
    for line in text.lines() {
        let trimmed = line
            .trim()
            .trim_matches(|character| matches!(character, '|' | '┃'))
            .trim();
        if trimmed.to_ascii_lowercase().starts_with("kiro ") {
            return normalize_plan(trimmed);
        }
    }
    None
}

fn credit_pair(text: &str) -> Option<(f64, f64)> {
    let lower = text.to_ascii_lowercase();
    let covered = lower.find(" covered")?;
    let before = &text[..covered];
    let of = before.to_ascii_lowercase().rfind(" of ")?;
    let total = parse_number_prefix(&before[of + 4..])?;
    let open = before[..of].rfind('(')?;
    let used = parse_number_prefix(&before[open + 1..of])?;
    Some((used, total))
}

fn bonus_credits(text: &str) -> (Option<f64>, Option<f64>, Option<u64>) {
    let lower = text.to_ascii_lowercase();
    let Some(start) = lower.find("bonus credits:") else {
        return (None, None, None);
    };
    let tail = &text[start + "bonus credits:".len()..];
    let used = parse_number_prefix(tail);
    let total = tail
        .find('/')
        .and_then(|index| parse_number_prefix(&tail[index + 1..]));
    let expiry = find_ascii_case_insensitive(tail, "expires in ").and_then(|index| {
        parse_number_prefix(&tail[index + "expires in ".len()..]).map(|days| days as u64)
    });
    (used, total, expiry)
}

fn local_midnight_ms(year: i32, month: u32, day: u32) -> Option<u64> {
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let naive = date.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|datetime| datetime.timestamp_millis().max(0) as u64)
}

fn reset_date(text: &str, now: chrono::DateTime<Local>) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("resets on ")? + "resets on ".len();
    let stamp: String = text[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit() || matches!(character, '-' | '/'))
        .collect();
    if stamp.contains('-') {
        let date = NaiveDate::parse_from_str(&stamp, "%Y-%m-%d").ok()?;
        return local_midnight_ms(date.year(), date.month(), date.day());
    }
    let mut parts = stamp.split('/');
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || NaiveDate::from_ymd_opt(now.year(), month, day).is_none() {
        return None;
    }
    let year = if (month, day) >= (now.month(), now.day()) {
        now.year()
    } else {
        now.year() + 1
    };
    local_midnight_ms(year, month, day)
}

#[derive(Debug)]
struct Reading {
    plan: Option<String>,
    windows: Vec<LimitWindow>,
    has_usage_metrics: bool,
}

fn parse_cli_output_at(text: &str, now: chrono::DateTime<Local>) -> Result<Reading, AdapterErr> {
    let stripped = strip_ansi(text);
    let lowered = stripped.to_ascii_lowercase();
    if [
        "not logged in",
        "login required",
        "failed to initialize auth portal",
        "kiro-cli login",
        "oauth error",
    ]
    .iter()
    .any(|phrase| lowered.contains(phrase))
    {
        return Err(AdapterErr::NeedsAuth);
    }

    let plan = plan_name(&stripped);
    let percent = stripped
        .lines()
        .find(|line| line.contains('█'))
        .and_then(|line| number_before(line, "%"));
    let credits = credit_pair(&stripped);
    let resets_at = reset_date(&stripped, now);
    let fraction = percent.map(|percent| percent / 100.0).or_else(|| {
        credits
            .filter(|(_, total)| *total > 0.0)
            .map(|(used, total)| used / total)
    });
    let (bonus_used, bonus_total, expiry_days) = bonus_credits(&stripped);
    if plan.is_none() && fraction.is_none() && bonus_used.is_none() {
        return Err(AdapterErr::NothingMetered(
            "Kiro CLI reported no usage".into(),
        ));
    }

    let mut windows = Vec::new();
    if let Some(used) = fraction {
        windows.push(LimitWindow {
            id: "credits".into(),
            label: "Credits".into(),
            used,
            resets_at,
            ..Default::default()
        });
    }
    if let (Some(used), Some(total)) = (bonus_used, bonus_total) {
        if total > 0.0 {
            windows.push(LimitWindow {
                id: "bonus".into(),
                group: Some("Bonus".into()),
                label: "Credits".into(),
                used: used / total,
                resets_at: expiry_days
                    .map(|days| now.timestamp_millis().max(0) as u64 + days * 86_400_000),
                ..Default::default()
            });
        }
    }
    Ok(Reading {
        plan,
        has_usage_metrics: fraction.is_some(),
        windows,
    })
}

fn parse_cli_output(text: &str) -> Result<Reading, AdapterErr> {
    parse_cli_output_at(text, Local::now())
}

#[derive(Debug)]
struct CreditLimits {
    plan_used: f64,
    plan_limit: f64,
    overage_used: f64,
    overage_cap: Option<f64>,
    reset_at: Option<u64>,
    has_unseparated_bonus: bool,
}

fn finite_nonnegative(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn first_number(object: &serde_json::Value, fields: &[&str]) -> Option<f64> {
    fields
        .iter()
        .find_map(|field| object.get(*field).and_then(serde_json::Value::as_f64))
        .and_then(finite_nonnegative)
}

fn parse_credit_limits(root: &serde_json::Value) -> Option<CreditLimits> {
    let credits: Vec<&serde_json::Value> = root
        .get("usageBreakdownList")?
        .as_array()?
        .iter()
        .filter(|row| row.get("resourceType").and_then(|value| value.as_str()) == Some("CREDIT"))
        .collect();
    if credits.len() != 1 {
        return None;
    }
    let credit = credits[0];
    let plan_limit = first_number(credit, &["usageLimitWithPrecision", "usageLimit"])?;
    let total_used = first_number(credit, &["currentUsageWithPrecision", "currentUsage"])?;
    let overage_used =
        first_number(credit, &["currentOveragesWithPrecision", "currentOverages"]).unwrap_or(0.0);
    if total_used < overage_used {
        return None;
    }
    let plan_used = total_used - overage_used;
    let has_unseparated_bonus = credit
        .get("bonuses")
        .and_then(serde_json::Value::as_array)
        .map(|bonuses| !bonuses.is_empty())
        .unwrap_or(false);
    if !has_unseparated_bonus && plan_used > plan_limit {
        return None;
    }
    let overage_enabled = root
        .get("overageConfiguration")
        .and_then(|value| value.get("overageStatus"))
        .and_then(|value| value.as_str())
        .map(|status| status.eq_ignore_ascii_case("ENABLED"));
    let overage_cap = if overage_enabled == Some(true) {
        first_number(credit, &["overageCapWithPrecision", "overageCap"])
    } else {
        None
    };
    let reset = first_number(credit, &["nextDateReset"])
        .or_else(|| first_number(root, &["nextDateReset"]))
        .filter(|seconds| (1_000_000_000.0..=4_102_444_800.0).contains(seconds))
        .map(|seconds| (seconds * 1000.0) as u64);
    Some(CreditLimits {
        plan_used,
        plan_limit,
        overage_used,
        overage_cap,
        reset_at: reset,
        has_unseparated_bonus,
    })
}

fn endpoint_for_arn(arn: &str) -> Option<&'static str> {
    if arn.chars().any(char::is_whitespace) {
        return None;
    }
    let parts: Vec<&str> = arn.splitn(6, ':').collect();
    if parts.len() != 6
        || parts[0] != "arn"
        || parts[1] != "aws"
        || parts[2] != "codewhisperer"
        || parts[4].is_empty()
        || !parts[5].starts_with("profile/")
        || parts[5].len() == "profile/".len()
    {
        return None;
    }
    match parts[3] {
        "us-east-1" => Some("https://codewhisperer.us-east-1.amazonaws.com/"),
        "eu-central-1" => Some("https://q.eu-central-1.amazonaws.com/"),
        _ => None,
    }
}

fn retry_after_secs(response: &ureq::Response) -> Option<u64> {
    crate::polling::retry_after_hint_secs(response.header("retry-after"), now_ms())
}

fn backoff_secs(attempt: u32, retry_after: Option<u64>) -> u64 {
    let local = BACKOFF_BASE_SECS
        .saturating_mul(1_u64 << attempt.min(4))
        .clamp(BACKOFF_BASE_SECS, BACKOFF_CAP_SECS);
    crate::polling::backoff_secs(local, retry_after)
}

enum EnrichResult {
    Limits(CreditLimits),
    RateLimited(u64),
    Unavailable,
}

fn fetch_limits(token: &str, arn: &str, attempt: u32) -> EnrichResult {
    let Some(endpoint) = endpoint_for_arn(arn) else {
        return EnrichResult::Unavailable;
    };
    let response = ureq::post(endpoint)
        .set("Content-Type", "application/x-amz-json-1.0")
        .set("X-Amz-Target", "AmazonCodeWhispererService.GetUsageLimits")
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send_json(serde_json::json!({"profileArn": arn}));
    match response {
        Ok(response) => response
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|value| parse_credit_limits(&value))
            .map(EnrichResult::Limits)
            .unwrap_or(EnrichResult::Unavailable),
        Err(ureq::Error::Status(429, response)) => {
            EnrichResult::RateLimited(backoff_secs(attempt, retry_after_secs(&response)))
        }
        _ => EnrichResult::Unavailable,
    }
}

fn apply_limits(reading: &mut Reading, limits: CreditLimits) {
    if limits.plan_limit > 0.0 && !limits.has_unseparated_bonus {
        let existing_reset = reading
            .windows
            .iter()
            .find(|window| window.id == "credits")
            .and_then(|window| window.resets_at);
        let credits = LimitWindow {
            id: "credits".into(),
            label: "Credits".into(),
            used: limits.plan_used / limits.plan_limit,
            resets_at: limits.reset_at.or(existing_reset),
            ..Default::default()
        };
        if let Some(index) = reading
            .windows
            .iter()
            .position(|window| window.id == "credits")
        {
            reading.windows[index] = credits;
        } else {
            reading.windows.insert(0, credits);
        }
        reading.has_usage_metrics = true;
    }
    if let Some(cap) = limits.overage_cap.filter(|cap| *cap > 0.0) {
        let overage = LimitWindow {
            id: "overage".into(),
            label: "Overage".into(),
            used: limits.overage_used / cap,
            resets_at: limits.reset_at,
            ..Default::default()
        };
        reading.windows.push(overage);
    }
}

fn maybe_enrich(reading: &mut Reading, held_until: u64) -> u64 {
    if held_until > now_ms() {
        return held_until;
    }
    let Some(database) = state_database_path() else {
        return 0;
    };
    let (Some(token), Some(arn)) = (access_token(&database), profile_arn(&database)) else {
        return 0;
    };
    let attempt = CONSECUTIVE_429.load(std::sync::atomic::Ordering::Relaxed);
    match fetch_limits(&token, &arn, attempt) {
        EnrichResult::Limits(limits) => {
            CONSECUTIVE_429.store(0, std::sync::atomic::Ordering::Relaxed);
            apply_limits(reading, limits);
            0
        }
        EnrichResult::RateLimited(seconds) => {
            CONSECUTIVE_429.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::polling::deadline_ms(now_ms(), seconds)
        }
        EnrichResult::Unavailable => 0,
    }
}

fn read_once(previous: &UsageSnapshot) -> UsageSnapshot {
    let mut snapshot = previous.clone();
    let Some(binary) = locate_binary() else {
        snapshot.status = "none".into();
        snapshot.windows.clear();
        snapshot.note = "Kiro CLI is not installed".into();
        return snapshot;
    };
    let text = match run_cli(&binary) {
        Ok(text) => text,
        Err(AdapterErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note =
                "Run kiro-cli login — Provider Monitor only reads the CLI session.".into();
            return snapshot;
        }
        Err(AdapterErr::TimedOut) => {
            snapshot.status = if snapshot.windows.is_empty() {
                "error"
            } else {
                "stale"
            }
            .into();
            snapshot.note = "kiro-cli /usage timed out".into();
            return snapshot;
        }
        Err(AdapterErr::NothingMetered(message) | AdapterErr::Other(message)) => {
            snapshot.status = if snapshot.windows.is_empty() {
                "error"
            } else {
                "stale"
            }
            .into();
            snapshot.note = message;
            return snapshot;
        }
    };
    let mut reading = match parse_cli_output(&text) {
        Ok(reading) => reading,
        Err(AdapterErr::NeedsAuth) => {
            snapshot.status = "needsAuth".into();
            snapshot.note =
                "Run kiro-cli login — Provider Monitor only reads the CLI session.".into();
            return snapshot;
        }
        Err(AdapterErr::NothingMetered(message)) => {
            snapshot.status = "none".into();
            snapshot.windows.clear();
            snapshot.note = message;
            return snapshot;
        }
        Err(AdapterErr::TimedOut | AdapterErr::Other(_)) => unreachable!(),
    };
    snapshot.backoff_until = maybe_enrich(&mut reading, snapshot.backoff_until);
    snapshot.fetched_at = now_ms();
    snapshot.windows = reading.windows;
    if snapshot.windows.is_empty() {
        snapshot.status = "none".into();
        snapshot.note = reading
            .plan
            .map(|plan| format!("{plan} · No usage meters"))
            .unwrap_or_else(|| "Kiro CLI reported no usage".into());
    } else {
        snapshot.status = "ok".into();
        snapshot.note = reading.plan.unwrap_or_else(|| "Kiro CLI".into());
    }
    snapshot
}

fn broadcast(app: &AppHandle, snapshot: UsageSnapshot) {
    let state = app.state::<AppState>();
    *state.kiro.lock().unwrap() = snapshot.clone();
    persist(&snapshot);
    let _ = app.emit("kiro", &snapshot);
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
            let snapshot = state.kiro.lock().unwrap().clone();
            let _ = app.emit("kiro", &snapshot);
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
                sleep_interruptible(ABSENT_POLL_SECS);
                if present() {
                    break;
                }
            }
        }
        loop {
            let previous = {
                let state = app.state::<AppState>();
                let snapshot = state.kiro.lock().unwrap().clone();
                snapshot
            };
            let snapshot = read_once(&previous);
            if snapshot.status == "error" || snapshot.status == "stale" {
                crate::applog(&format!("kiro: {}", snapshot.note));
            }
            broadcast(&app, snapshot);
            // GetUsageLimits backoff never pauses the authoritative CLI reading.
            sleep_interruptible(POLL_SECS);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn local_time(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .earliest()
            .unwrap()
    }

    #[test]
    fn free_tier_bar_and_reset_are_parsed() {
        let text = "| KIRO FREE |\n████ 25%\n(12.50 of 50 covered in plan), resets on 01/15";
        let reading = parse_cli_output_at(text, local_time(2026, 1, 10, 12)).unwrap();
        assert_eq!(reading.plan.as_deref(), Some("Kiro Free"));
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].id, "credits");
        assert!((reading.windows[0].used - 0.25).abs() < 1e-9);
        assert_eq!(reading.windows[0].resets_at, local_midnight_ms(2026, 1, 15));
    }

    #[test]
    fn bonus_and_ansi_two_x_card_are_parsed() {
        let text = "\u{1b}[1mEstimated Usage\u{1b}[0m | resets on 2026-06-01 | \u{1b}[mKIRO FREE\u{1b}[0m\n\
                    Bonus credits: 45.53/2000 credits used, expires in 19 days\n\
                    Credits (0.17 of 50 covered in plan)\n████ 0%\nOverages: Disabled";
        let reading = parse_cli_output_at(text, local_time(2026, 5, 15, 12)).unwrap();
        assert_eq!(reading.plan.as_deref(), Some("Kiro Free"));
        assert_eq!(
            reading
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["credits", "bonus"]
        );
        assert_eq!(reading.windows[0].used, 0.0);
        assert!((reading.windows[1].used - 45.53 / 2000.0).abs() < 1e-9);
        assert_eq!(reading.windows[1].group.as_deref(), Some("Bonus"));
    }

    #[test]
    fn plan_only_is_not_an_invented_zero_meter() {
        let reading = parse_cli_output_at(
            "┃ Plan: KIRO PRO MAX | 1 usage breakdowns ┃",
            local_time(2026, 1, 10, 12),
        )
        .unwrap();
        assert_eq!(reading.plan.as_deref(), Some("Kiro Pro Max"));
        assert!(!reading.has_usage_metrics);
        assert!(reading.windows.is_empty());
    }

    #[test]
    fn login_phrases_need_auth_and_garbage_is_unmetered() {
        assert!(matches!(
            parse_cli_output_at(
                "Failed to initialize auth portal. Run kiro-cli login.",
                local_time(2026, 1, 10, 12)
            ),
            Err(AdapterErr::NeedsAuth)
        ));
        assert!(matches!(
            parse_cli_output_at("hello", local_time(2026, 1, 10, 12)),
            Err(AdapterErr::NothingMetered(_))
        ));
    }

    #[test]
    fn month_day_reset_wraps_only_after_the_printed_day() {
        let text = "| KIRO FREE |\n████ 25%, resets on 01/15";
        let same_day = parse_cli_output_at(text, local_time(2026, 1, 15, 12)).unwrap();
        let later = parse_cli_output_at(text, local_time(2026, 1, 20, 12)).unwrap();
        assert_eq!(
            same_day.windows[0].resets_at,
            local_midnight_ms(2026, 1, 15)
        );
        assert_eq!(later.windows[0].resets_at, local_midnight_ms(2027, 1, 15));
    }

    #[test]
    fn ansi_stripping_handles_csi_osc_and_charset_sequences() {
        assert_eq!(strip_ansi("\u{1b}[38:2:255:0:0m50%\u{1b}[m"), "50%");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}Plan"), "Plan");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{1b}\\Plan"), "Plan");
        assert_eq!(strip_ansi("\u{1b}(BKIRO"), "KIRO");
    }

    #[test]
    fn precise_limits_split_overage_from_plan_usage() {
        let value = serde_json::json!({
            "nextDateReset": 1.7882208E9,
            "overageConfiguration": {"overageStatus": "ENABLED"},
            "usageBreakdownList": [{
                "resourceType": "CREDIT", "currentUsageWithPrecision": 13603.49,
                "currentOveragesWithPrecision": 3603.49, "usageLimitWithPrecision": 10000.0,
                "overageCapWithPrecision": 10000.0, "bonuses": []
            }]
        });
        let limits = parse_credit_limits(&value).unwrap();
        assert_eq!(limits.plan_used, 10_000.0);
        assert_eq!(limits.overage_used, 3603.49);
        assert_eq!(limits.overage_cap, Some(10_000.0));
        assert_eq!(limits.reset_at, Some(1_788_220_800_000));
    }

    #[test]
    fn unsupported_or_ambiguous_limits_are_rejected() {
        let multiple = serde_json::json!({"usageBreakdownList": [
            {"resourceType":"CREDIT","currentUsage":1,"usageLimit":10},
            {"resourceType":"CREDIT","currentUsage":2,"usageLimit":20}
        ]});
        assert!(parse_credit_limits(&multiple).is_none());
        assert!(endpoint_for_arn("arn:aws:codewhisperer:ap-southeast-1:1:profile/test").is_none());
        assert_eq!(
            endpoint_for_arn("arn:aws:codewhisperer:us-east-1:123:profile/test"),
            Some("https://codewhisperer.us-east-1.amazonaws.com/")
        );
    }

    #[test]
    fn windows_data_directory_and_override_are_supported() {
        let roaming = Path::new(r"C:\Users\tester\AppData\Roaming");
        let local = Path::new(r"C:\Users\tester\AppData\Local");
        let home = Path::new(r"C:\Users\tester");
        assert_eq!(
            state_database_path_from(Some(r"D:\KiroData"), Some(roaming), Some(local), Some(home)),
            Some(PathBuf::from(r"D:\KiroData").join("data.sqlite3"))
        );
        assert_eq!(
            state_database_path_from(None, Some(roaming), Some(local), Some(home)),
            Some(roaming.join("kiro-cli").join("data.sqlite3"))
        );
    }

    #[test]
    fn retry_backoff_doubles_and_caps() {
        assert_eq!(backoff_secs(0, None), 60);
        assert_eq!(backoff_secs(3, None), 480);
        assert_eq!(backoff_secs(8, None), 900);
        assert_eq!(backoff_secs(0, Some(1200)), 1200);
        assert_eq!(backoff_secs(3, Some(2)), 480);
    }

    #[test]
    fn sqlite_session_is_read_without_modifying_the_database() {
        let path = std::env::temp_dir().join(format!(
            "provider-monitor-kiro-{}-{}.sqlite3",
            std::process::id(),
            now_ms()
        ));
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute("CREATE TABLE auth_kv(key TEXT PRIMARY KEY, value TEXT)", [])
                .unwrap();
            connection
                .execute(
                    "INSERT INTO auth_kv(key, value) VALUES (?1, ?2)",
                    rusqlite::params!["kirocli:odic:token", r#"{"accessToken":"synthetic-token"}"#],
                )
                .unwrap();
        }
        let before = fs::read(&path).unwrap();
        let sidecars = ["-wal", "-shm", "-journal"]
            .map(|suffix| PathBuf::from(format!("{}{suffix}", path.display())));
        let before_sidecars = sidecars.each_ref().map(|sidecar| sidecar.exists());

        assert_eq!(access_token(&path).as_deref(), Some("synthetic-token"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            sidecars.each_ref().map(|sidecar| sidecar.exists()),
            before_sidecars
        );
        fs::remove_file(path).unwrap();
    }
}
