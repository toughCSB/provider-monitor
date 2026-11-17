//! Read-only account profile selection. The default profile remains the legacy home directory.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind { Codex, Claude }

#[derive(Clone, PartialEq, Eq)]
pub struct Profile { pub id: String, pub dir: PathBuf }

impl Kind {
    fn dirname(self) -> &'static str { if self == Self::Codex { ".codex" } else { ".claude" } }
    pub fn id(self) -> &'static str { if self == Self::Codex { "codex" } else { "claude" } }
}

pub fn default_dir(kind: Kind) -> Option<PathBuf> { dirs::home_dir().map(|h| h.join(kind.dirname())) }

pub fn usable(kind: Kind, dir: &Path) -> bool {
    if !dir.is_dir() { return false; }
    let files: &[&str] = if kind == Kind::Codex { &["auth.json"] } else { &[".credentials.json", "credentials.json"] };
    files.iter().any(|name| {
        let Ok(raw) = std::fs::read_to_string(dir.join(name)) else { return false };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else { return false };
        if kind == Kind::Codex {
            let Some(tokens) = value.get("tokens") else { return false };
            ["access_token", "account_id"].iter().all(|key| tokens.get(*key).and_then(|v| v.as_str()).is_some_and(|v| !v.trim().is_empty()))
        } else {
            value.get("claudeAiOauth").unwrap_or(&value).get("accessToken")
                .and_then(|v| v.as_str()).is_some_and(|v| !v.trim().is_empty())
        }
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b)
}

/// A previously selected directory stays visible after logout. Initial selection still requires
/// a credential through `candidates` or `validate_registration`.
pub fn retained(kind: Kind, path: &str, explicit: bool) -> Option<Profile> {
    let dir = PathBuf::from(path);
    if !dir.is_absolute() || default_dir(kind).is_some_and(|d| same_dir(&dir, &d)) { return None; }
    let id = if explicit { format!("{}-account-2", kind.id()) } else {
        let name = dir.file_name()?.to_string_lossy().to_lowercase();
        let slug = name.strip_prefix(&format!("{}-", kind.dirname()))?;
        if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; }
        format!("{}-{slug}", kind.id())
    };
    Some(Profile { id, dir })
}

pub fn second(kind: Kind, configured: Option<&str>) -> Option<Profile> {
    candidates(kind, configured).into_iter().next()
}

pub fn candidates(kind: Kind, configured: Option<&str>) -> Vec<Profile> {
    dirs::home_dir().map(|home| candidates_in(kind, &home, configured)).unwrap_or_default()
}

fn second_in(kind: Kind, home: &Path, configured: Option<&str>) -> Option<Profile> {
    candidates_in(kind, home, configured).into_iter().next()
}

fn candidates_in(kind: Kind, home: &Path, configured: Option<&str>) -> Vec<Profile> {
    let default = home.join(kind.dirname());
    if let Some(path) = configured.filter(|s| !s.trim().is_empty()) {
        let dir = PathBuf::from(path);
        return (usable(kind, &dir) && !same_dir(&dir, &default))
            .then_some(Profile { id: format!("{}-account-2", kind.id()), dir }).into_iter().collect();
    }
    let prefix = format!("{}-", kind.dirname());
    let mut candidates: Vec<_> = match std::fs::read_dir(home) { Ok(rd) => rd, Err(_) => return vec![] }.flatten().filter_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.starts_with(&prefix) || name.len() == prefix.len() { return None; }
        if !name[prefix.len()..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; }
        // Old copies and plugin data can contain copied auth.json. Never auto-enrol those.
        if ["backup", "bak", "copy", "old", "archive", "plugin", "tmp", "temp"]
            .iter().any(|part| name.contains(part)) { return None; }
        let path = entry.path();
        (usable(kind, &path) && !same_dir(&path, &default)).then_some(path)
    }).collect();
    candidates.sort();
    candidates.into_iter().map(|dir| {
        let name = dir.file_name().unwrap().to_string_lossy().to_lowercase();
        let slug = name.strip_prefix(&prefix).unwrap_or_default();
        Profile { id: format!("{}-{slug}", kind.id()), dir }
    }).collect()
}

pub fn validate_registration(kind: Kind, path: &str) -> Result<String, String> {
    let dir = PathBuf::from(path.trim());
    if !dir.is_absolute() { return Err("Choose an absolute profile directory path".into()); }
    if !usable(kind, &dir) { return Err("No usable account credential in that directory".into()); }
    let canonical = dir.canonicalize().map_err(|_| "Cannot read profile directory")?;
    if default_dir(kind).is_some_and(|default| same_dir(&canonical, &default)) {
        return Err("That is already account 1".into());
    }
    Ok(canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_require_actual_tokens() {
        let root = std::env::temp_dir().join(format!("provider-monitor-profiles-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(root.join("auth.json"), r#"{"tokens":{"access_token":"x"}}"#).unwrap();
        assert!(!usable(Kind::Codex, &root));
        std::fs::write(root.join("auth.json"), r#"{"tokens":{"access_token":"x","account_id":"a"}}"#).unwrap();
        assert!(usable(Kind::Codex, &root));
        std::fs::write(root.join(".credentials.json"), r#"{"plugins":{"accessToken":"x"}}"#).unwrap();
        assert!(!usable(Kind::Claude, &root));
        std::fs::write(root.join(".credentials.json"), r#"{"claudeAiOauth":{"accessToken":"x"}}"#).unwrap();
        assert!(usable(Kind::Claude, &root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_only_a_distinct_signed_in_profile() {
        let home = std::env::temp_dir().join(format!("provider-monitor-discovery-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        for name in [".codex", ".codex-backup", ".codex-work", ".codex-plugin", ".codex-empty"] {
            std::fs::create_dir_all(home.join(name)).unwrap();
        }
        let auth = r#"{"tokens":{"access_token":"x","account_id":"other"}}"#;
        for name in [".codex-backup", ".codex-work", ".codex-plugin"] {
            std::fs::write(home.join(name).join("auth.json"), auth).unwrap();
        }
        let found = second_in(Kind::Codex, &home, None).unwrap();
        assert_eq!(found.id, "codex-work");
        assert_eq!(found.dir, home.join(".codex-work"));
        let explicit = second_in(Kind::Codex, &home, Some(home.join(".codex-work").to_str().unwrap())).unwrap();
        assert_eq!(explicit.id, "codex-account-2");
        assert!(second_in(Kind::Codex, &home, Some(home.join(".codex-empty").to_str().unwrap())).is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn retained_selection_keeps_its_id_after_logout() {
        let home = std::env::temp_dir().join(format!("provider-monitor-retained-{}", std::process::id()));
        let dir = home.join(".codex-work");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!usable(Kind::Codex, &dir));
        assert_eq!(retained(Kind::Codex, dir.to_str().unwrap(), false).unwrap().id, "codex-work");
        assert_eq!(retained(Kind::Codex, dir.to_str().unwrap(), true).unwrap().id, "codex-account-2");
        let _ = std::fs::remove_dir_all(home);
    }
}
