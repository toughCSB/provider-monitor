use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// The Mac's notch sizes, as multiples of the designed size: Small, Medium, Large.
pub const SIZES: [f64; 3] = [0.8, 1.0, 1.25];

/// The physical screen edge the Windows notch is attached to. The strings intentionally match
/// macOS `NotchEdge.RawValue`, so settings and diagnostics use the same vocabulary on both apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotchEdge {
    Right,
    Left,
    Top,
    Bottom,
}

impl NotchEdge {
    pub const ALL: [Self; 4] = [Self::Right, Self::Left, Self::Top, Self::Bottom];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Right | Self::Left)
    }
}

pub fn notch_edge_or_right(value: &str) -> NotchEdge {
    match value {
        "left" => NotchEdge::Left,
        "top" => NotchEdge::Top,
        "bottom" => NotchEdge::Bottom,
        _ => NotchEdge::Right,
    }
}

/// The nearest of `SIZES`, so a scale saved by the old 40–100 % slider still lands on a size that
/// exists. 0.9, halfway between Small and Medium, counts as Medium.
pub fn snap_scale(scale: f64) -> f64 {
    if scale < 0.9 {
        SIZES[0]
    } else if scale < 1.125 || !scale.is_finite() {
        SIZES[1]
    } else {
        SIZES[2]
    }
}

/// One half of the tray icon, or one ring on the notch: which provider. It shows that provider's
/// ring, so the tray and the notch can never disagree. (A `window` key from older builds is ignored.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraySlot {
    pub provider: String,
}

/// One of two independent, app-owned sign-ins per provider. No credential is stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAccount {
    pub provider: String,
    pub slot: u8,
    pub directory: PathBuf,
    pub identity: String,
    pub label: String,
    #[serde(default)]
    pub disconnected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    /// "auto" | "zh" | "en" | "ja" | "ko" | "ru" | "uk"
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default)]
    pub bar_x: Option<i32>,
    #[serde(default)]
    pub bar_y: Option<i32>,
    /// Logical width of the bar (wheel-adjustable, 220-520); None = default 360
    #[serde(default)]
    pub bar_w: Option<u32>,
    /// Allow dragging + wheel resizing (tray toggle, off by default to prevent accidental drags)
    #[serde(default)]
    pub drag_enabled: bool,
    /// Vertical position of the notch: the window centre as a fraction of the selected monitor's
    /// height (0 = top, 1 = bottom), default 0.5; saved after a drag.
    #[serde(default = "default_notch_y")]
    pub notch_y: f64,
    /// Screen edge, matching the four choices in the macOS Appearance pane.
    #[serde(default = "default_notch_edge")]
    pub notch_edge: String,
    /// Position along each edge, 0 = the visible pill touches the start of that edge and 1 = it
    /// touches the end. Remembered per edge so switching sides does not discard a placement.
    #[serde(default)]
    pub notch_positions: HashMap<String, f64>,
    /// Stable monitor key. "primary" follows the Windows primary display; an explicit key keeps
    /// the notch on the display chosen in Settings until that display disappears.
    #[serde(default = "default_notch_monitor")]
    pub notch_monitor: String,
    /// Notch size as a multiple of the designed size, one of `SIZES`. The whole notch scales: the
    /// window grows and its WebView zooms, so the rings, text and hover card keep their proportions.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// The ring's number and its graph: "remaining" counts down (the default) and "used" counts
    /// up. The arc and card bars follow the same basis, so the number and painted length agree.
    #[serde(default = "default_percent_basis")]
    pub percent_basis: String,
    /// Where the weekly limit gets a ring of its own: "off", "inside" or "outside".
    #[serde(default = "default_weekly_ring")]
    pub weekly_ring: String,
    /// What the tray icon draws: "off" (the plain mark, the previous behaviour and the default),
    /// "numbers" (up to two readings as digits) or "bars" (a column per reading).
    #[serde(default = "default_tray_mode")]
    pub tray_mode: String,
    /// Which providers the tray icon covers, in the order they are drawn. Ids match the page:
    /// "claude", "codex", "cursor", "gemini". Superseded by `tray_slots`; kept so an existing
    /// config still upgrades cleanly, and migrated in `load()`.
    #[serde(default = "default_tray_providers")]
    pub tray_providers: Vec<String>,
    /// What each part of the tray icon shows, in drawing order: the first entry is the top half of
    /// the digit layout, the second the bottom half, and the bar layout uses them all in order.
    #[serde(default)]
    pub tray_slots: Vec<TraySlot>,
    /// Which providers the notch itself shows, in order. Empty means every provider that has
    /// something to report — the original behaviour, and the default. Superseded by `notch_slots`,
    /// kept so an existing config migrates cleanly.
    #[serde(default)]
    pub notch_providers: Vec<String>,
    /// Which providers get a ring on the notch, in order. An empty list means every provider.
    #[serde(default)]
    pub notch_slots: Vec<TraySlot>,
    /// Each provider's own ring metric — 떡배님's ask: not one setting for every ring, but a choice
    /// per provider (id → "automatic" | "5h" | "weekly" | "monthly"). Missing = "weekly", the
    /// overall default. Started as Antigravity's own "Notch reads" (the Mac app's term); now every
    /// provider gets an entry, keyed the same as the notch/tray ids.
    #[serde(default)]
    pub ring_limits: HashMap<String, String>,
    /// Optional existing account-2 directories. Empty selects a credential-bearing .codex-*/.claude-* sibling.
    #[serde(default)]
    pub codex_profile_2: Option<String>,
    #[serde(default)]
    pub claude_profile_2: Option<String>,
    /// The first verified automatic choice is pinned, so logging out never swaps account 2.
    #[serde(default)]
    pub codex_auto_profile_2: Option<String>,
    #[serde(default)]
    pub claude_auto_profile_2: Option<String>,
    /// Disconnect account 2 without losing its selected directory or reading credentials.
    #[serde(default)]
    pub codex_second_disconnected: bool,
    #[serde(default)]
    pub claude_second_disconnected: bool,
    #[serde(default)]
    pub managed_accounts: Vec<ManagedAccount>,
    /// The model family that choice looks at, as the Mac app's "Model data": "gemini" or "3p"
    #[serde(default = "default_antigravity_model")]
    pub antigravity_model: String,
    /// false = the pill is kept off the screen edge entirely; the tray icon is then the only way in
    #[serde(default = "yes")]
    pub notch_visible: bool,
    /// false = the tray icon is hidden. Refused while the notch is also hidden, because that would
    /// leave the app running with no way to reach it.
    #[serde(default = "yes")]
    pub tray_visible: bool,
    /// true (the default, matching the window's own creation flag) = the notch stays above every
    /// other window. Windows can silently drop a topmost window's z-order (another app claiming
    /// topmost, an exclusive-fullscreen game); re-asserted periodically while this is true rather
    /// than trusted to stick from one `set_always_on_top` call. false = an ordinary window, which
    /// sinks behind whatever is focused, as 떡배님 asked for.
    #[serde(default = "yes")]
    pub always_on_top: bool,
}

fn default_notch_y() -> f64 {
    0.5
}
fn default_notch_edge() -> String {
    NotchEdge::Right.as_str().into()
}
fn default_notch_monitor() -> String {
    "primary".into()
}
fn default_scale() -> f64 {
    1.0
}
fn default_percent_basis() -> String {
    "remaining".into()
}
fn default_weekly_ring() -> String {
    "off".into()
}

/// A second arc changes how every reading looks, so an unreadable value means off rather than a
/// guess at what was meant.
pub fn weekly_ring_or_off(value: &str) -> String {
    match value {
        "inside" | "outside" => value.to_string(),
        _ => default_weekly_ring(),
    }
}
pub fn percent_basis_or_remaining(value: &str) -> String {
    if value == "used" { "used" } else { "remaining" }.into()
}
fn yes() -> bool {
    true
}
/// A fresh install shows the two readings straight away — a tray icon nobody knows to look for is
/// a feature nobody finds. An install that predates this setting is handled in `load()` instead:
/// it keeps the plain mark it already has, so upgrading never changes anyone's icon unasked.
fn default_tray_mode() -> String {
    "numbers".into()
}
fn default_tray_providers() -> Vec<String> {
    vec!["claude".into(), "codex".into()]
}
fn default_antigravity_model() -> String {
    "gemini".into()
}

fn default_port() -> u16 {
    48666
}
fn default_lang() -> String {
    "auto".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            lang: default_lang(),
            bar_x: None,
            bar_y: None,
            bar_w: None,
            drag_enabled: false,
            notch_y: default_notch_y(),
            notch_edge: default_notch_edge(),
            notch_positions: HashMap::new(),
            notch_monitor: default_notch_monitor(),
            scale: default_scale(),
            percent_basis: default_percent_basis(),
            weekly_ring: default_weekly_ring(),
            tray_mode: default_tray_mode(),
            tray_providers: default_tray_providers(),
            tray_slots: Vec::new(), // filled in by load(), from tray_providers
            notch_providers: Vec::new(), // empty = show them all
            notch_slots: Vec::new(),     // filled in by load(), from notch_providers
            ring_limits: HashMap::new(),
            codex_profile_2: None,
            claude_profile_2: None,
            codex_auto_profile_2: None,
            claude_auto_profile_2: None,
            codex_second_disconnected: false,
            claude_second_disconnected: false,
            managed_accounts: Vec::new(),
            antigravity_model: default_antigravity_model(),
            notch_visible: true,
            tray_visible: true,
            always_on_top: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("codenotch")
        .join("config.json")
}

pub fn load() -> Config {
    let path = config_path();
    let raw = std::fs::read_to_string(&path).ok();
    let mut cfg: Config = raw
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();

    // Discoverability without surprising anyone. `default_tray_mode` gives a NEW install the
    // numbers icon, but serde applies that same default to an EXISTING config that simply predates
    // the setting — which would silently change the tray icon of everyone who upgrades. So an
    // existing file with no `tray_mode` key is pinned to the plain mark it already has; only a
    // machine with no config at all gets the new default.
    let upgrading = raw
        .as_deref()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(|v| v.get("tray_mode").is_none())
        .unwrap_or(false);
    if upgrading {
        cfg.tray_mode = "off".into();
    }

    // Migration: before slots existed the icon was a plain provider list, one reading each. That
    // is exactly a list of slots, so nobody's choice is lost and nobody has to reconfigure anything.
    if cfg.tray_slots.is_empty() {
        cfg.tray_slots = cfg
            .tray_providers
            .iter()
            .map(|p| TraySlot { provider: p.clone() })
            .collect();
    }

    // Same migration for the notch.
    if cfg.notch_slots.is_empty() {
        cfg.notch_slots = cfg
            .notch_providers
            .iter()
            .map(|p| TraySlot { provider: p.clone() })
            .collect();
    }

    // Migration: `ring_limits` replaces the older `antigravity_limit`, a single value that used to
    // apply to Antigravity alone. An existing file's choice is carried over as Antigravity's own
    // entry rather than silently reverting it to "weekly" on upgrade.
    if cfg.ring_limits.is_empty() {
        if let Some(old) = raw
            .as_deref()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
            .and_then(|v| v.get("antigravity_limit").and_then(|l| l.as_str()).map(str::to_string))
        {
            cfg.ring_limits.insert("gemini".into(), old);
        }
    }

    // Both hidden would leave the app unreachable: no pill, no tray icon, no way to open settings.
    if !cfg.notch_visible && !cfg.tray_visible {
        cfg.tray_visible = true;
    }

    // The old slider's 40–100 %, or a hand-edited file, lands on one of the three sizes
    cfg.scale = snap_scale(cfg.scale);
    cfg.percent_basis = percent_basis_or_remaining(&cfg.percent_basis);
    cfg.weekly_ring = weekly_ring_or_off(&cfg.weekly_ring);
    cfg.notch_edge = notch_edge_or_right(&cfg.notch_edge).as_str().into();
    // `notch_y` was the right-edge position before all four edges existed. Carry it into the new
    // per-edge map once, while leaving the field in the file for older builds that may read it.
    if cfg.notch_positions.is_empty() {
        cfg.notch_positions
            .insert(NotchEdge::Right.as_str().into(), cfg.notch_y.clamp(0.0, 1.0));
    }
    for value in cfg.notch_positions.values_mut() {
        *value = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.5 };
    }
    cfg
}

impl Config {
    pub fn edge(&self) -> NotchEdge {
        notch_edge_or_right(&self.notch_edge)
    }

    pub fn notch_position(&self, edge: NotchEdge) -> f64 {
        self.notch_positions
            .get(edge.as_str())
            .copied()
            .filter(|v| v.is_finite())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0)
    }

    pub fn set_notch_position(&mut self, edge: NotchEdge, value: f64) {
        let value = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.5 };
        self.notch_positions.insert(edge.as_str().into(), value);
        if edge == NotchEdge::Right {
            self.notch_y = value;
        }
    }
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, txt);
    }
}

#[cfg(test)]
mod tests {
    use super::{notch_edge_or_right, percent_basis_or_remaining, Config, NotchEdge, snap_scale, weekly_ring_or_off};

    #[test]
    fn a_saved_scale_snaps_to_the_nearest_size() {
        assert_eq!(snap_scale(0.4), 0.8);
        assert_eq!(snap_scale(0.85), 0.8);
        assert_eq!(snap_scale(0.9), 1.0);
        assert_eq!(snap_scale(1.0), 1.0);
        assert_eq!(snap_scale(1.2), 1.25);
        assert_eq!(snap_scale(3.0), 1.25);
    }

    #[test]
    fn only_the_two_placements_are_kept() {
        assert_eq!(weekly_ring_or_off("inside"), "inside");
        assert_eq!(weekly_ring_or_off("outside"), "outside");
        assert_eq!(weekly_ring_or_off("Inside"), "off");
        assert_eq!(weekly_ring_or_off(""), "off");
    }

    #[test]
    fn ring_number_defaults_to_remaining() {
        assert_eq!(percent_basis_or_remaining("remaining"), "remaining");
        assert_eq!(percent_basis_or_remaining("used"), "used");
        assert_eq!(percent_basis_or_remaining("anything else"), "remaining");
    }

    #[test]
    fn all_four_mac_edges_are_valid_and_unknown_values_fall_back_to_right() {
        assert_eq!(NotchEdge::ALL.map(NotchEdge::as_str), ["right", "left", "top", "bottom"]);
        assert_eq!(notch_edge_or_right("left"), NotchEdge::Left);
        assert_eq!(notch_edge_or_right("top"), NotchEdge::Top);
        assert_eq!(notch_edge_or_right("bottom"), NotchEdge::Bottom);
        assert_eq!(notch_edge_or_right("somewhere"), NotchEdge::Right);
    }

    #[test]
    fn placement_is_remembered_per_edge_and_the_legacy_right_value_stays_in_sync() {
        let mut cfg = Config::default();
        cfg.set_notch_position(NotchEdge::Right, 0.1);
        cfg.set_notch_position(NotchEdge::Top, 0.9);
        assert_eq!(cfg.notch_position(NotchEdge::Right), 0.1);
        assert_eq!(cfg.notch_position(NotchEdge::Top), 0.9);
        assert_eq!(cfg.notch_position(NotchEdge::Left), 0.5);
        assert_eq!(cfg.notch_y, 0.1);
    }
}
