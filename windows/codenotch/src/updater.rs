//! Two deliberately separate GitHub Release checks.
//!
//! The original `vinzdg/codenotch` check is informational only: its result contains no asset URL
//! and therefore has no path to the installer. This project's releases are a second check and the only
//! source `download_and_launch` accepts. No signing keys, no `latest.json` manifest, no auto-apply:
//! a release is a human running the Windows Package workflow, which attaches the installer to the
//! very same `vX.Y.Z` release the Mac disk image is published on (see windows/README.md), so a full
//! `tauri-plugin-updater` pipeline would be new infrastructure this app does not otherwise need.
//! Instead: ask GitHub's public releases API whether a newer `vX.Y.Z` release exists, and if the
//! user clicks the button, download that release's installer and launch it — the installer (and the
//! user, who sees its window) does the rest.

use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const FORK_REPO: &str = "toughCSB/provider-monitor";
const UPSTREAM_REPO: &str = "vinzdg/codenotch";
/// One release serves both platforms now: the Mac disk image and the Windows installer are assets of
/// the same `vX.Y.Z` release, under one version number. The retired `windows-vX.Y.Z` series and the
/// rolling `preview` tag both fail this prefix, which is exactly what is wanted — neither is a
/// stable release to install from.
const TAG_PREFIX: &str = "v";
const INSTALLER_NAME: &str = "Provider-Monitor-Setup.exe";
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Default)]
pub struct UpdateInfo {
    pub checking: bool,
    pub checked: bool,
    pub current: String,
    pub latest: String,
    pub available: bool,
    /// The release page, for "what changed" — opened in a browser, never auto-read.
    pub html_url: String,
    pub asset_url: String,
    pub asset_name: String,
    pub asset_size: u64,
    pub error: String,
}

static LAST: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// The original app's latest release. Deliberately no downloadable asset fields: upstream can be
/// inspected from this fork, but can never be installed over it.
#[derive(Clone, Serialize, Default)]
pub struct UpstreamInfo {
    pub checking: bool,
    pub checked: bool,
    pub latest: String,
    pub html_url: String,
    pub error: String,
}

static UPSTREAM_LAST: Mutex<Option<UpstreamInfo>> = Mutex::new(None);

pub fn last() -> UpdateInfo {
    LAST.lock().unwrap().clone().unwrap_or_default()
}

pub fn upstream_last() -> UpstreamInfo {
    UPSTREAM_LAST.lock().unwrap().clone().unwrap_or_default()
}

#[derive(serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn installable_asset(release: &GhRelease) -> Option<&GhAsset> {
    let version = release.tag_name.strip_prefix(TAG_PREFIX)?;
    if release.draft || release.prerelease || version.split('.').count() != 3
        || !version.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let expected_url = format!(
        "https://github.com/{FORK_REPO}/releases/download/{}/{INSTALLER_NAME}", release.tag_name
    );
    release.assets.iter().find(|asset| {
        asset.name == INSTALLER_NAME && asset.browser_download_url == expected_url
            && (1..=MAX_INSTALLER_BYTES).contains(&asset.size)
    })
}

/// `1.2.10` beats `1.2.9`: each dot-separated run of leading digits compares as a number, not as
/// text, so a two-digit patch release is never mistaken for older than a one-digit one.
fn newer(a: &str, b: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.split('.')
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0))
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (pa.get(i).copied().unwrap_or(0), pb.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// The releases page also carries the rolling `preview` tag, which is rebuilt on every push to
/// `main` and is deliberately skipped by `TAG_PREFIX`. Walked a few pages deep rather than one, so a
/// real release is not missed just because a burst of previews pushed it off a single page.
const MAX_PAGES: u32 = 4;
const PER_PAGE: u32 = 30;

fn fetch_latest() -> Result<(String, String, GhAsset), String> {
    let mut last_err = "no release with a Windows installer found".to_string();
    for page in 1..=MAX_PAGES {
        let url = format!("https://api.github.com/repos/{FORK_REPO}/releases?per_page={PER_PAGE}&page={page}");
        let resp = ureq::get(&url)
            .set("User-Agent", concat!("provider-monitor/", env!("CARGO_PKG_VERSION"), " (Windows)"))
            .set("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|e| format!("{e}"))?;
        let releases: Vec<GhRelease> = resp.into_json().map_err(|e| format!("parse: {e}"))?;
        if releases.is_empty() {
            break; // fewer releases than MAX_PAGES * PER_PAGE: nothing further to fetch
        }
        for rel in releases {
            if let Some(asset) = installable_asset(&rel) {
                return Ok((rel.tag_name.clone(), rel.html_url.clone(), GhAsset {
                    name: asset.name.clone(), browser_download_url: asset.browser_download_url.clone(),
                    size: asset.size,
                }));
            }
        }
        last_err = format!("no Windows installer found in the {} most recent releases", page * PER_PAGE);
    }
    Err(last_err)
}

fn fetch_upstream_latest() -> Result<(String, String), String> {
    let url = format!("https://api.github.com/repos/{UPSTREAM_REPO}/releases/latest");
    let rel: GhRelease = ureq::get(&url)
        .set("User-Agent", concat!("provider-monitor/", env!("CARGO_PKG_VERSION"), " (Windows)"))
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("{e}"))?
        .into_json()
        .map_err(|e| format!("parse: {e}"))?;
    if rel.draft || rel.prerelease {
        return Err("the latest upstream release is not a stable release".into());
    }
    Ok((rel.tag_name.trim_start_matches('v').to_string(), rel.html_url))
}

/// Reports the original project's latest release. There is intentionally no install counterpart.
pub fn check_upstream(app: &AppHandle) -> UpstreamInfo {
    let mut info = UpstreamInfo { checking: true, ..Default::default() };
    *UPSTREAM_LAST.lock().unwrap() = Some(info.clone());
    let _ = app.emit("upstream_update_info", &info);
    match fetch_upstream_latest() {
        Ok((latest, html_url)) => {
            info.latest = latest;
            info.html_url = html_url;
        }
        Err(error) => info.error = error,
    }
    info.checking = false;
    info.checked = true;
    *UPSTREAM_LAST.lock().unwrap() = Some(info.clone());
    let _ = app.emit("upstream_update_info", &info);
    info
}

/// Runs the check inline (on whatever thread calls it — callers hop to a background thread first)
/// and both returns and broadcasts the result, so the settings window updates whether it asked or
/// merely has the page open when a background check lands.
pub fn check(app: &AppHandle) -> UpdateInfo {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let mut info = UpdateInfo { checking: true, current: current.clone(), ..Default::default() };
    *LAST.lock().unwrap() = Some(info.clone());
    let _ = app.emit("update_info", &info);

    match fetch_latest() {
        Ok((tag, html_url, asset)) => {
            let latest = tag.strip_prefix(TAG_PREFIX).unwrap_or(&tag).to_string();
            info.available = newer(&latest, &current);
            info.latest = latest;
            info.html_url = html_url;
            info.asset_url = asset.browser_download_url;
            info.asset_name = asset.name;
            info.asset_size = asset.size;
        }
        Err(e) => info.error = e,
    }
    info.checking = false;
    info.checked = true;
    *LAST.lock().unwrap() = Some(info.clone());
    let _ = app.emit("update_info", &info);
    info
}

/// A background check once shortly after launch — quiet unless there is something to say; the
/// settings window's own "Check now" button reruns `check` directly and awaits it.
pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        cleanup_old_installers(&installer_directory(), SystemTime::now());
        std::thread::sleep(Duration::from_secs(20));
        check(&app);
        check_upstream(&app);
    });
}

/// Downloads the release's installer to a temp file and launches it, then quits Codenotch so the
/// installer is not fighting the running exe for its own file lock. The installer's own window is
/// what the user watches from here — nothing here runs silently or without that visible step.
pub fn download_and_launch(app: &AppHandle) -> Result<(), String> {
    // Never accept a URL from the page. Only the asset returned by this fork's last successful
    // release check may become executable; the original-project check has no asset field at all.
    let info = last();
    if !info.available || info.asset_name != INSTALLER_NAME
        || !(1..=MAX_INSTALLER_BYTES).contains(&info.asset_size)
        || info.asset_url != format!(
            "https://github.com/{FORK_REPO}/releases/download/v{}/{INSTALLER_NAME}", info.latest
        ) {
        return Err("no installable update for this Windows app was checked".into());
    }
    let resp = ureq::get(&info.asset_url)
        .set("User-Agent", concat!("provider-monitor/", env!("CARGO_PKG_VERSION"), " (Windows)"))
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let (path, mut file) = unique_installer_file()?;
    let result = (|| {
        copy_installer(resp.into_reader(), &mut file, info.asset_size)?;
        file.flush().map_err(|e| format!("could not save installer: {e}"))?;
        drop(file);
        std::process::Command::new(&path)
            .spawn()
            .map_err(|e| format!("could not start the installer: {e}"))
    })();
    match result {
        Ok(_) => {},
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
    }
    // The app exits while NSIS is still using the file. Reclaim old owned
    // downloads on a later launch, not in a thread that dies with this process.
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        app.exit(0);
    });
    Ok(())
}

fn installer_directory() -> std::path::PathBuf {
    std::env::temp_dir().join("provider-monitor-updates-v1")
}

fn cleanup_old_installers(directory: &std::path::Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(numbers) = name.strip_prefix("Provider-Monitor-Setup-").and_then(|s| s.strip_suffix(".exe")) else { continue };
        let parts: Vec<_> = numbers.split('-').collect();
        if parts.len() != 3 || !parts.iter().all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())) { continue; }
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) { continue; }
        let old = entry.metadata().ok().and_then(|m| m.modified().ok())
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age >= Duration::from_secs(24 * 3600));
        if old { let _ = std::fs::remove_file(entry.path()); }
    }
}

fn unique_installer_file() -> Result<(std::path::PathBuf, std::fs::File), String> {
    let directory = installer_directory();
    std::fs::create_dir_all(&directory).map_err(|e| format!("could not create installer directory: {e}"))?;
    for _ in 0..10 {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|e| format!("clock error: {e}"))?.as_nanos();
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "Provider-Monitor-Setup-{}-{stamp}-{sequence}.exe", std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("could not save installer: {error}")),
        }
    }
    Err("could not reserve a unique installer file".into())
}

fn copy_installer(mut input: impl Read, output: &mut impl Write, expected_size: u64) -> Result<(), String> {
    if !(1..=MAX_INSTALLER_BYTES).contains(&expected_size) {
        return Err("invalid installer size".into());
    }
    let copied = std::io::copy(&mut input.by_ref().take(expected_size + 1), output)
        .map_err(|e| format!("download failed: {e}"))?;
    if copied != expected_size {
        return Err("downloaded installer does not match its published size".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{copy_installer, installable_asset, newer, unique_installer_file, GhRelease};

    #[test]
    fn cleanup_preserves_recent_downloads_and_unrelated_files() {
        let dir = std::env::temp_dir().join(format!("pm-cleanup-test-{}-{:?}",std::process::id(),super::TEMP_SEQUENCE.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let owned = dir.join("Provider-Monitor-Setup-1-123456-1.exe");
        let unrelated = dir.join("user-installer.exe");
        std::fs::write(&owned,b"fixture").unwrap();
        std::fs::write(&unrelated,b"fixture").unwrap();
        let now = std::time::SystemTime::now();
        super::cleanup_old_installers(&dir,now);
        assert!(owned.exists());
        super::cleanup_old_installers(&dir,now+std::time::Duration::from_secs(48*3600));
        assert!(!owned.exists());
        assert!(unrelated.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_higher_version_is_newer() {
        assert!(newer("0.4.0", "0.3.0"));
        assert!(newer("0.3.1", "0.3.0"));
        assert!(newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn the_same_or_an_older_version_is_not_newer() {
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
    }

    /// `1.2.10` must not be read as older than `1.2.9` by comparing the strings byte for byte.
    #[test]
    fn a_two_digit_component_is_compared_numerically() {
        assert!(newer("1.2.10", "1.2.9"));
    }

    /// Real production data (api.github.com/repos/toughCSB/provider-monitor/releases): a
    /// same-version release must not be reported as an update.
    #[test]
    fn the_current_shipped_release_is_not_flagged_as_an_update() {
        assert!(!newer(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn missing_installer_and_unrelated_exe_are_not_installable() {
        let release: GhRelease = serde_json::from_str(r#"{
            "tag_name":"v2.3.4", "html_url":"https://example.invalid/release",
            "assets":[
                {"name":"ProviderMonitor-2.3.4-unsigned.dmg", "browser_download_url":"https://example.invalid/mac", "size":100},
                {"name":"Other-Setup.exe", "browser_download_url":"https://example.invalid/other", "size":100},
                {"name":"Provider-Monitor-Setup.exe", "browser_download_url":"https://example.invalid/wrong-origin", "size":100}
            ]
        }"#).unwrap();
        assert!(installable_asset(&release).is_none());
    }

    #[test]
    fn exact_fork_installer_requires_published_size() {
        let release: GhRelease = serde_json::from_str(r#"{
            "tag_name":"v2.3.4", "html_url":"https://example.invalid/release",
            "assets":[{"name":"Provider-Monitor-Setup.exe",
              "browser_download_url":"https://github.com/toughCSB/provider-monitor/releases/download/v2.3.4/Provider-Monitor-Setup.exe",
              "size":123}]
        }"#).unwrap();
        assert_eq!(installable_asset(&release).unwrap().size, 123);
    }

    #[test]
    fn streamed_download_rejects_truncation_and_extra_bytes() {
        let mut exact = Vec::new();
        copy_installer(&b"abc"[..], &mut exact, 3).unwrap();
        assert_eq!(exact, b"abc");
        assert!(copy_installer(&b"ab"[..], &mut Vec::new(), 3).is_err());
        assert!(copy_installer(&b"abcd"[..], &mut Vec::new(), 3).is_err());
    }

    #[test]
    fn simultaneous_downloads_get_distinct_owned_temp_files() {
        let (first, first_file) = unique_installer_file().unwrap();
        let (second, second_file) = unique_installer_file().unwrap();
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
        drop(first_file);
        drop(second_file);
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
    }
}
