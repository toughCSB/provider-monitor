#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod autostart;
mod config;
mod doctor;
mod focus;
mod hooks_install;
mod i18n;
mod server;
mod state;
mod tray;
mod usage;
mod codex;
mod cursor;
mod antigravity;
mod grok;
mod opencode;
mod glm;
mod devin;
mod command_code;
mod kimi;
mod copilot;
mod kiro;
mod agy_cli;
mod glyphs;
mod trayicon;
mod activity;
mod diag;
mod updater;
mod watcher;

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use config::NotchEdge;

/// Logical size of the notch window: the 70 pt pill column on the right plus room for the hover card
/// and its tail on the left. `fitZoom` in ui/notch.html divides by the same width.
pub const NOTCH_W: f64 = 360.0;
/// Horizontal edges need enough inward depth for the 96px body, tail and the tallest hover card.
/// A simple 360×700 rotation clipped the bottom of cards because the card begins beyond the body.
pub const HORIZONTAL_NOTCH_H: f64 = 700.0;
/// Hand-bumped build tag, written to run.log at startup so a log can always be matched to the exe that wrote it.
pub const BUILD: &str = "r43-grok-click-parity";
// The macOS geometry is 526 pt tall for five provider cells before a hover card is considered.
// 700 leaves the same end slack the native panel keeps, so the first/last card can stay wholly on
// screen without shrinking the rings or folding the spacing back toward the old Windows design.
pub const NOTCH_H: f64 = 700.0;

pub struct AppState {
    pub store: Mutex<state::Store>,
    pub cfg: Mutex<config::Config>,
    pub usage: Mutex<usage::UsageSnapshot>,
    /// Codex snapshot (same UsageSnapshot shape; status may also be none/absent)
    pub codex: Mutex<usage::UsageSnapshot>,
    pub cursor: Mutex<usage::UsageSnapshot>,
    pub antigravity: Mutex<usage::UsageSnapshot>,
    pub grok: Mutex<usage::UsageSnapshot>,
    pub opencode: Mutex<usage::UsageSnapshot>,
    pub glm: Mutex<usage::UsageSnapshot>,
    pub devin: Mutex<usage::UsageSnapshot>,
    pub commandcode: Mutex<usage::UsageSnapshot>,
    pub kimi: Mutex<usage::UsageSnapshot>,
    pub copilot: Mutex<usage::UsageSnapshot>,
    pub kiro: Mutex<usage::UsageSnapshot>,
    /// Provider glyph cache, collected at launch and again on a tray refresh
    pub glyphs: Mutex<std::collections::HashMap<String, glyphs::Glyph>>,
    /// Working state of the non-Claude providers (Cursor reports it; Codex and Antigravity are inferred from recent writes)
    pub activity: Mutex<Vec<activity::Activity>>,
}

fn resolved_lang(raw: &str) -> String {
    if raw == "auto" {
        i18n::resolve_auto().to_string()
    } else {
        raw.to_string()
    }
}

/// The size multiplier chosen with the slider, clamped to what the config allows.
pub fn ui_scale(app: &AppHandle) -> f64 {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    config::snap_scale(c.scale)
}

pub fn broadcast(app: &AppHandle) {
    let st = app.state::<AppState>();
    let snap = {
        let store = st.store.lock().unwrap();
        let cfg = st.cfg.lock().unwrap();
        store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), i18n::clock_24h(), false)
    };
    let _ = app.emit("state", &snap);
}

fn monitor_key(mon: &tauri::Monitor) -> String {
    mon.name().cloned().unwrap_or_else(|| {
        format!(
            "display-{}x{}",
            mon.size().width,
            mon.size().height
        )
    })
}

fn selected_monitor(app: &AppHandle) -> Option<tauri::Monitor> {
    let w = app.get_webview_window("notch")?;
    let wanted = {
        let st = app.state::<AppState>();
        let value = st.cfg.lock().ok()?.notch_monitor.clone();
        value
    };
    if wanted != "primary" {
        if let Ok(monitors) = w.available_monitors() {
            if let Some(mon) = monitors.into_iter().find(|m| monitor_key(m) == wanted) {
                return Some(mon);
            }
        }
    }
    w.primary_monitor().ok().flatten()
}

#[derive(serde::Serialize)]
struct DisplayChoice {
    id: String,
    label: String,
    primary: bool,
    selected: bool,
}

#[tauri::command]
fn get_displays(app: AppHandle) -> Vec<DisplayChoice> {
    let Some(w) = app.get_webview_window("notch") else {
        return Vec::new();
    };
    let wanted = {
        let st = app.state::<AppState>();
        st.cfg
            .lock()
            .map(|c| c.notch_monitor.clone())
            .unwrap_or_else(|_| "primary".into())
    };
    let primary_key = w
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| monitor_key(&m));
    let mut monitors = w.available_monitors().unwrap_or_default();
    monitors.sort_by_key(|m| (m.position().x, m.position().y));
    let mut choices = vec![DisplayChoice {
        id: "primary".into(),
        label: "Follow primary".into(),
        primary: true,
        selected: wanted == "primary",
    }];
    choices.extend(
        monitors.into_iter().enumerate().map(|(index, mon)| {
            let id = monitor_key(&mon);
            let primary = primary_key.as_deref() == Some(id.as_str());
            let name = mon.name().cloned().unwrap_or_default();
            DisplayChoice {
                selected: wanted == id,
                id,
                label: if name.is_empty() {
                    format!("Monitor {}", index + 1)
                } else {
                    format!("Monitor {} · {}", index + 1, name)
                },
                primary,
            }
        }),
    );
    choices
}

#[tauri::command]
fn set_display(app: AppHandle, id: String) -> Vec<DisplayChoice> {
    let valid = id == "primary"
        || app
            .get_webview_window("notch")
            .and_then(|w| w.available_monitors().ok())
            .is_some_and(|monitors| monitors.iter().any(|m| monitor_key(m) == id));
    if valid {
        let st = app.state::<AppState>();
        if let Ok(mut cfg) = st.cfg.lock() {
            cfg.notch_monitor = id;
            config::save(&cfg);
        }
        place_notch(&app);
    }
    get_displays(app)
}

#[tauri::command]
fn get_notch_edge(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    st.cfg
        .lock()
        .map(|c| c.edge().as_str().to_string())
        .unwrap_or_else(|_| NotchEdge::Right.as_str().into())
}

#[tauri::command]
fn set_notch_edge(app: AppHandle, edge: String) -> String {
    let edge = config::notch_edge_or_right(&edge);
    {
        let st = app.state::<AppState>();
        if let Ok(mut cfg) = st.cfg.lock() {
            cfg.notch_edge = edge.as_str().into();
            config::save(&cfg);
        };
    }
    // The first rectangle reported after the CSS rotates is the source of truth for full-range
    // drag clamping. An old right-edge rectangle would stop a horizontal notch short.
    if let Ok(mut hot) = HOT.lock() {
        hot.clear();
    }
    place_notch(&app);
    let value = edge.as_str().to_string();
    let _ = app.emit("notch_edge", &value);
    value
}

fn notch_window_size(edge: NotchEdge, scale: f64) -> tauri::PhysicalSize<u32> {
    let (width, height) = if edge.is_vertical() {
        (NOTCH_W, NOTCH_H)
    } else {
        (NOTCH_H, HORIZONTAL_NOTCH_H)
    };
    tauri::PhysicalSize::new((width * scale).round() as u32, (height * scale).round() as u32)
}

/// The legal window-origin range along an edge. `pill_start` and `pill_length` describe the
/// visible pill inside the much larger transparent window. Letting the transparent slack leave the
/// monitor is intentional: it is what allows the visible pill itself to reach both ends.
fn along_origin_range(
    monitor_start: i32,
    monitor_length: i32,
    window_length: i32,
    pill: Option<(f64, f64)>,
) -> (i32, i32) {
    let (start, length) = pill
        .filter(|(start, length)| start.is_finite() && length.is_finite() && *start >= 0.0 && *length > 0.0)
        .unwrap_or((0.0, window_length.max(0) as f64));
    let lo = monitor_start - start.round() as i32;
    let hi = monitor_start + monitor_length - (start + length).round() as i32;
    if lo <= hi { (lo, hi) } else { (monitor_start, monitor_start) }
}

fn origin_for_ratio(lo: i32, hi: i32, ratio: f64) -> i32 {
    let ratio = if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.5 };
    (lo as f64 + (hi - lo) as f64 * ratio).round() as i32
}

fn ratio_for_origin(lo: i32, hi: i32, origin: i32) -> f64 {
    if hi <= lo {
        0.5
    } else {
        ((origin - lo) as f64 / (hi - lo) as f64).clamp(0.0, 1.0)
    }
}

fn pill_along_rect(edge: NotchEdge) -> Option<(f64, f64)> {
    HOT.lock().ok().and_then(|rects| rects.first().map(|rect| {
        if edge.is_vertical() { (rect[1], rect[3]) } else { (rect[0], rect[2]) }
    }))
}

/// Pins the notch to the selected edge of the display selected in Settings. A disconnected
/// explicit display falls back to the current primary display until it returns.
pub fn place_notch(app: &AppHandle) {
    let Some(w) = app.get_webview_window("notch") else {
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    if let Some(mon) = selected_monitor(app) {
        // Two monitors at different scales (150 % and 200 % in practice): the physical size can
        // end up converted with the *other* monitor's scale factor depending on where the window
        // is created and then moved, leaving the WebView ~256 logical px wide instead of 340.
        // So the physical size is pinned straight from mon.scale_factor() before placing the
        // window; if it still reports a different scale afterwards, it is pinned once more.
        let (edge, ratio) = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            let edge = c.edge();
            (edge, c.notch_position(edge))
        };
        let ms = mon.scale_factor();
        let target = notch_window_size(edge, ms);
        let _ = w.set_size(target);
        // Position from the window's measured physical size — deriving it from the scale factor
        // pushed the window past the right edge at 125 % / 150 % (the ring's right side was clipped).
        let (ww, wh) = w
            .outer_size()
            .map(|s| (s.width as i32, s.height as i32))
            .unwrap_or((target.width as i32, target.height as i32));
        let pill = pill_along_rect(edge);
        let (x, y) = if edge.is_vertical() {
            let (lo, hi) = along_origin_range(mon.position().y, mon.size().height as i32, wh, pill);
            let y = origin_for_ratio(lo, hi, ratio);
            let x = if edge == NotchEdge::Right {
                mon.position().x + mon.size().width as i32 - ww
            } else {
                mon.position().x
            };
            (x, y)
        } else {
            let (lo, hi) = along_origin_range(mon.position().x, mon.size().width as i32, ww, pill);
            let x = origin_for_ratio(lo, hi, ratio);
            let y = if edge == NotchEdge::Bottom {
                mon.position().y + mon.size().height as i32 - wh
            } else {
                mon.position().y
            };
            (x, y)
        };
        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        if w.outer_size().map(|s| s != target).unwrap_or(false) {
            let _ = w.set_size(target);
            let (tw, th) = (target.width as i32, target.height as i32);
            let (x, y) = if edge.is_vertical() {
                let (lo, hi) = along_origin_range(mon.position().y, mon.size().height as i32, th, pill);
                let x = if edge == NotchEdge::Right {
                    mon.position().x + mon.size().width as i32 - tw
                } else {
                    mon.position().x
                };
                (x, origin_for_ratio(lo, hi, ratio))
            } else {
                let (lo, hi) = along_origin_range(mon.position().x, mon.size().width as i32, tw, pill);
                let y = if edge == NotchEdge::Bottom {
                    mon.position().y + mon.size().height as i32 - th
                } else {
                    mon.position().y
                };
                (origin_for_ratio(lo, hi, ratio), y)
            };
            let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        }
        // Placement log line: the first thing to check when the notch is not visible
        let log = config::config_path().with_file_name("run.log");
        let _ = std::fs::write(
            log,
            format!(
                "notch placed build={BUILD}: edge={} ratio={ratio:.3} pos=({x},{y}) size=({ww}x{wh}) inner={:?} win_scale={scale} mon_scale={ms} monitor=({},{} {}x{})\n",
                edge.as_str(),
                w.inner_size().map(|s| (s.width, s.height)).unwrap_or((0, 0)),
                mon.position().x,
                mon.position().y,
                mon.size().width,
                mon.size().height
            ),
        );
        enforce_saved_topmost(app);
    }
}

/// A cheap fingerprint of the current monitor layout: every monitor's position, size and scale,
/// in the order Windows reports them. Two different layouts are astronomically unlikely to collide.
fn monitor_fingerprint(app: &AppHandle) -> Option<String> {
    let w = app.get_webview_window("notch")?;
    let mons = w.available_monitors().ok()?;
    Some(
        mons.iter()
            .map(|m| format!("{:?}:{:?}:{}", m.position(), m.size(), m.scale_factor()))
            .collect::<Vec<_>>()
            .join("|"),
    )
}

/// `place_notch` only ever ran at startup or when the user asked for it (drag, "Reset position").
/// Unplugging a monitor, docking a laptop, or an RDP session resizing the desktop all change the
/// primary monitor's bounds without any of those, so the pill stayed wherever it had last been
/// placed — sometimes short of the true right edge, sometimes off the bottom of a monitor that
/// had since shrunk. Polling is simpler and just as reliable as subclassing the window for
/// WM_DISPLAYCHANGE, and 2 s is fast enough that nobody notices the lag after a change.
fn start_monitor_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last = monitor_fingerprint(&app);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let cur = monitor_fingerprint(&app);
            if cur.is_some() && cur != last {
                last = cur;
                applog("monitor layout changed -> re-placing the notch");
                place_notch(&app);
            }
        }
    });
}

/// Older entry point name still used by tray.rs
pub fn reset_bar(app: &AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        let edge = c.edge();
        c.set_notch_position(edge, 0.5);
        config::save(&c);
    }
    place_notch(app);
}

/// Drag along the selected edge. The page calls this once after a press on the pill moves more than
/// 4 px; from then on a Rust thread follows the system cursor (WebView mousemove is unreliable
/// once the window itself starts moving). Releasing the left button ends the drag and the per-edge
/// ratio is written back to the config.
static DRAGGING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}
#[cfg(not(windows))]
fn left_button_down() -> bool {
    false
}

#[tauri::command]
fn drag_begin(app: AppHandle) {
    if DRAGGING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let Some(w) = app.get_webview_window("notch") else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let Some(mon) = selected_monitor(&app) else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let (Ok(start_cur), Ok(start_pos), Ok(size)) =
            (app.cursor_position(), w.outer_position(), w.outer_size())
        else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let edge = {
            let st = app.state::<AppState>();
            st.cfg.lock().map(|c| c.edge()).unwrap_or(NotchEdge::Right)
        };
        let vertical = edge.is_vertical();
        let (monitor_start, monitor_length, window_length) = if vertical {
            (mon.position().y, mon.size().height as i32, size.height as i32)
        } else {
            (mon.position().x, mon.size().width as i32, size.width as i32)
        };
        let (lo, hi) = along_origin_range(
            monitor_start,
            monitor_length,
            window_length,
            pill_along_rect(edge),
        );
        let mut last = if vertical { start_pos.y } else { start_pos.x };
        let mut moved = false;
        loop {
            if !left_button_down() {
                break;
            }
            if let Ok(cur) = app.cursor_position() {
                let delta = if vertical { cur.y - start_cur.y } else { cur.x - start_cur.x };
                let start = if vertical { start_pos.y } else { start_pos.x };
                let next = (start as f64 + delta).round() as i32;
                let next = next.clamp(lo, hi);
                if next != last {
                    last = next;
                    moved = true;
                    let pos = if vertical {
                        tauri::PhysicalPosition::new(start_pos.x, next)
                    } else {
                        tauri::PhysicalPosition::new(next, start_pos.y)
                    };
                    let _ = w.set_position(pos);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        if moved {
            let ratio = ratio_for_origin(lo, hi, last);
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.set_notch_position(edge, ratio);
            config::save(&c);
            applog(&format!("notch drag: edge={} origin={last} ratio={ratio:.3}", edge.as_str()));
        }
        DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
        place_notch(&app);
        let _ = app.emit("drag_end", moved);
    });
}
pub fn place_bar(app: &AppHandle) {
    place_notch(app);
}
pub fn toggle_drag(app: &AppHandle) {
    // The notch stays welded to the edge; kept as a no-op for the tray menu code path
    let _ = app;
}

pub fn apply_lang(app: &AppHandle, lang: &str) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.lang = lang.to_string();
        config::save(&c);
    }
    // Through refresh_menu, which makes sure the swap happens on the main thread: doing it from the
    // settings window's thread left the tray with a menu that would never open again.
    tray::refresh_menu(app);
    broadcast(app);
    antigravity::emit_current(app);
}

// ---------------- commands ----------------

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> state::Snapshot {
    let store = state.store.lock().unwrap();
    let cfg = state.cfg.lock().unwrap();
    store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), i18n::clock_24h(), false)
}

#[tauri::command]
fn get_usage(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.usage.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_usage(app: AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut u = st.usage.lock().unwrap();
        u.backoff_until = 0;
    }
    usage::request_refresh();
    codex::request_refresh();
    cursor::request_refresh();
    antigravity::request_refresh();
    grok::request_refresh();
    opencode::request_refresh();
    glm::request_refresh();
    devin::request_refresh();
    command_code::request_refresh();
    kimi::request_refresh();
    copilot::request_refresh();
    kiro::request_refresh();
}

#[tauri::command]
fn get_antigravity(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    let lang = state.cfg.lock().unwrap().lang.clone();
    let snap = state.antigravity.lock().unwrap().clone();
    antigravity::localized_snapshot_for_lang(snap, &lang)
}

#[tauri::command]
fn get_activity(state: tauri::State<AppState>) -> Vec<activity::Activity> {
    state.activity.lock().unwrap().clone()
}

#[tauri::command]
fn get_glyphs(state: tauri::State<AppState>) -> std::collections::HashMap<String, glyphs::Glyph> {
    state.glyphs.lock().unwrap().clone()
}

/// Collects the glyphs again and pushes them to the page (tray refresh, or the user just dropped in an override)
pub fn reload_glyphs(app: &AppHandle) {
    let prev = {
        let st = app.state::<AppState>();
        let g = st.glyphs.lock().unwrap().clone();
        g
    };
    let m = glyphs::collect(&prev);
    let st = app.state::<AppState>();
    *st.glyphs.lock().unwrap() = m.clone();
    let _ = app.emit("glyphs", &m);
}

#[tauri::command]
fn open_data_dir() {
    let dir = config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let _ = std::fs::create_dir_all(glyphs::user_dir());
    let mut cmd = std::process::Command::new("explorer");
    cmd.arg(dir.as_os_str());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn get_cursor(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.cursor.lock().unwrap().clone()
}

#[tauri::command]
fn get_codex(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.codex.lock().unwrap().clone()
}

#[tauri::command]
fn get_grok(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.grok.lock().unwrap().clone()
}

#[tauri::command]
fn get_opencode(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.opencode.lock().unwrap().clone()
}

#[tauri::command]
fn get_glm(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.glm.lock().unwrap().clone()
}

#[tauri::command]
fn get_devin(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.devin.lock().unwrap().clone()
}

#[tauri::command]
fn get_commandcode(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.commandcode.lock().unwrap().clone()
}

#[tauri::command]
fn get_kimi(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.kimi.lock().unwrap().clone()
}

#[tauri::command]
fn get_copilot(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.copilot.lock().unwrap().clone()
}

#[tauri::command]
fn get_kiro(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.kiro.lock().unwrap().clone()
}

fn request_provider_refresh(app: &AppHandle, provider: &str) -> bool {
    match provider {
        "claude" => {
            if let Ok(mut snap) = app.state::<AppState>().usage.lock() {
                snap.backoff_until = 0;
            }
            usage::request_refresh();
        }
        "codex" => codex::request_refresh(),
        "cursor" => cursor::request_refresh(),
        "gemini" => antigravity::request_refresh(),
        "grok" => grok::request_refresh(),
        "opencode" => opencode::request_refresh(),
        "glm" => glm::request_refresh(),
        "devin" => devin::request_refresh(),
        "commandcode" => command_code::request_refresh(),
        "kimi" => kimi::request_refresh(),
        "copilot" => copilot::request_refresh(),
        "kiro" => kiro::request_refresh(),
        _ => return false,
    }
    true
}

/// A single click refreshes just the provider under the pointer, matching the macOS ring action.
#[tauri::command]
fn refresh_provider(app: AppHandle, provider: String) -> bool {
    request_provider_refresh(&app, &provider)
}

/// The single-click delay follows Windows' own double-click setting, so a user who deliberately
/// configured a slower double click never gets an unwanted refresh before the page opens.
#[tauri::command]
fn get_double_click_time_ms() -> u64 {
    #[cfg(windows)]
    unsafe {
        return windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() as u64;
    }
    #[cfg(not(windows))]
    500
}

fn provider_page(provider: &str) -> &'static str {
    match provider {
        "codex" => "https://chatgpt.com/codex/cloud/settings/analytics#usage",
        "cursor" => "https://cursor.com/dashboard",
        "gemini" => "https://gemini.google.com/app",
        // Kept unchanged at 떡배님's request.
        "grok" => "https://grok.com/?_s=usage",
        "opencode" => "https://opencode.ai/workspace/wrk_01M02ADN1RXR7S9P9S5BYPPAGT/go",
        "glm" => "https://bigmodel.cn/usercenter/proj-mgmt/apikeys",
        "devin" => "https://app.devin.ai",
        "commandcode" => "https://commandcode.ai",
        "kimi" => "https://www.kimi.com/code/console",
        "copilot" => "https://github.com/settings/copilot",
        "kiro" => "https://kiro.dev",
        // Claude's existing Windows target is intentionally retained.
        _ => "https://claude.ai/settings/usage",
    }
}

/// A double click on a cell opens that provider's official usage/account page.
#[tauri::command]
fn open_provider_page(provider: String) {
    let url = provider_page(&provider);
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

/// Hot rectangles in **physical pixels**, window-relative, as x,y,w,h: the pill, plus the card
/// while it is open. The page converts by its own devicePixelRatio before reporting, so no scale
/// conversion happens here — WebView2's DPR and the window's scale_factor can disagree (see
/// report_dpr).
///
/// Empty means click-through: before the page has reported, one lost click on the notch beats
/// eating every click aimed at the window behind it.
static HOT: Mutex<Vec<[f64; 4]>> = Mutex::new(Vec::new());

/// Read only by the collapse timer — the click gate goes by the rectangles, since the pill is
/// clickable whether or not the card is up.
static EXPANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
fn set_hot(app: AppHandle, rects: Vec<[f64; 4]>, expanded: bool) {
    let layout_changed = {
        let mut hot = HOT.lock().unwrap();
        let changed = match (hot.first(), rects.first()) {
            (None, Some(_)) | (Some(_), None) => true,
            (Some(old), Some(new)) => old
                .iter()
                .zip(new.iter())
                .any(|(left, right)| (left - right).abs() > 0.5),
            (None, None) => false,
        };
        *hot = rects;
        changed
    };
    EXPANDED.store(expanded, std::sync::atomic::Ordering::Relaxed);
    if expanded {
        antigravity::request_hover_refresh();
    }
    if layout_changed {
        // Startup and an edge rotation begin with no trustworthy pill geometry. Once the page has
        // laid the pill out, re-place from that visible rectangle so saved end positions are exact.
        // The same applies when provider count or size changes the pill's extent.
        place_notch(&app);
    }
}

/// Setting `WS_EX_TRANSPARENT` by hand instead looks like it should work, and does not: it applies
/// to the notch window, but WebView2 keeps child HWNDs that hit-testing descends into and they
/// never get the bit. `WS_EX_LAYERED` is what makes the window answer as one surface, so the helper
/// that sets both is the only route. Clearing it again is safe — the notch is not otherwise layered
/// (its transparency is DWM composition), so the window returns to the styles it had.
fn set_click_through(app: &AppHandle, on: bool) {
    let Some(w) = app.get_webview_window("notch") else { return };
    let _ = w.set_ignore_cursor_events(on);
}

/// The WebView zoom currently applied (1.0 = uncorrected)
static ZOOM: Mutex<f64> = Mutex::new(1.0);

pub fn applog(line: &str) {
    use std::io::Write;
    let log = config::config_path().with_file_name("run.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{line}");
    }
}

/// Root cause: with two monitors (150 % / 200 %) WebView2 picked a devicePixelRatio of 2.0 while
/// the window was sized for the primary monitor's 1.5, so the page was 255 CSS px wide instead of
/// the designed 340 and every coordinate conversion was off (the watchdog misfired and the card
/// flashed away). Fix: the page reports its DPR, and when it differs from the primary monitor's
/// scale, set_zoom pulls the effective DPR back to that scale, restoring the 360 px width.
#[tauri::command]
fn report_dpr(app: AppHandle, dpr: f64, w: f64, h: f64) {
    let Some(win) = app.get_webview_window("notch") else { return };
    let want = selected_monitor(&app)
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| win.scale_factor().unwrap_or(1.0));
    let mut z = ZOOM.lock().unwrap();
    let base = if *z > 0.0 { dpr / *z } else { dpr };
    let target = if base > 0.0 { want / base } else { 1.0 };
    applog(&format!(
        "dpr report: dpr={dpr:.3} viewport={w:.0}x{h:.0} monitor_scale={want:.3} zoom_applied={:.3} -> target_zoom={target:.3}",
        *z
    ));
    // Oscillation guard: at most three corrections per process (if the DPR does not follow the zoom, stop chasing it)
    static APPLIED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if (dpr - want).abs() > 0.02
        && (target - *z).abs() > 0.01
        && (0.25..=4.0).contains(&target)
        && APPLIED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3
    {
        match win.set_zoom(target) {
            Ok(()) => {
                *z = target;
                applog(&format!("dpr correction: set_zoom({target:.3}) ok"));
            }
            Err(e) => applog(&format!("dpr correction failed: {e}")),
        }
    }
}

/// Slack around every hot rectangle: this is sampled on a timer, so a cursor arriving at the pill
/// has to count as arrived slightly early, or a quick click lands between two polls while the
/// window is still click-through and goes to whatever is behind it.
const HOT_PAD: f64 = 10.0;

/// Is the cursor on something the window is there for? `window` is the outer size in physical
/// pixels, or None when it could not be read.
fn cursor_in_hot(rects: &[[f64; 4]], lx: f64, ly: f64, window: Option<(f64, f64)>) -> bool {
    if rects.is_empty() {
        return false;
    }
    let in_window = window
        .map(|(w, h)| lx >= 0.0 && ly >= 0.0 && lx < w && ly < h)
        .unwrap_or(true);
    if !in_window {
        return false;
    }
    if rects.iter().any(|r| {
        lx >= r[0] - HOT_PAD
            && ly >= r[1] - HOT_PAD
            && lx < r[0] + r[2] + HOT_PAD
            && ly < r[1] + r[3] + HOT_PAD
    }) {
        return true;
    }
    // The gap between hot rectangles (pill and card) counts as inside: use the bounding box of all of them
    if rects.len() > 1 {
        let x0 = rects.iter().map(|r| r[0]).fold(f64::MAX, f64::min);
        let y0 = rects.iter().map(|r| r[1]).fold(f64::MAX, f64::min);
        let x1 = rects.iter().map(|r| r[0] + r[2]).fold(f64::MIN, f64::max);
        let y1 = rects.iter().map(|r| r[1] + r[3]).fold(f64::MIN, f64::max);
        return lx >= x0 && ly >= y0 && lx < x1 && ly < y1;
    }
    false
}

/// Was 150 ms, when this only decided whether the card stayed up. It now also gates whether a click
/// reaches the notch, and at 150 ms a click arriving in the wrong sample went to the window behind.
const WATCHDOG_MS: u64 = 50;
/// Kept at the original 300 ms rather than falling out of the faster poll, which would make the
/// card twitchy.
const LEAVE_MS: u64 = 300;

/// WebView2's mouseleave is unreliable inside a NOACTIVATE transparent window — a cursor that
/// leaves quickly often produces no WM_MOUSELEAVE, and the card stays up. Rather than trust DOM
/// events, the Rust side watches the system cursor and emits pointer_left once it is outside; the
/// page collapses after its 250 ms grace period. "Outside the window" is not the test, though: the
/// window is mostly transparent, so the cursor is compared against the hot rectangles the page
/// reports (pill, card, and the gap between them).
///
/// It also gates click-through (#106), which is why it runs whether or not the card is open. That
/// ordering is load-bearing: the window ignores the cursor while it is click-through, so the page
/// gets no mousemove out there and cannot see the pointer arriving. This loop does, and hands the
/// window its input back in time for the page to open the card.
fn start_pointer_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        let need = (LEAVE_MS / WATCHDOG_MS).max(1) as u8;
        let mut miss = 0u8;
        // Last value pushed: this changes only when the cursor crosses an edge
        let mut click_through: Option<bool> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_MS));
            let Some(w) = app.get_webview_window("notch") else { continue };
            let (Ok(pos), Ok(cur)) = (w.outer_position(), app.cursor_position()) else { continue };
            let rects = HOT.lock().unwrap().clone();
            // Cursor position relative to the window's top-left, in physical pixels; the hot rectangles are physical too, so no scale conversion
            let lx = cur.x - pos.x as f64;
            let ly = cur.y - pos.y as f64;
            let size = w.outer_size().ok().map(|s| (s.width as f64, s.height as f64));
            let inside = cursor_in_hot(&rects, lx, ly, size);

            if click_through != Some(!inside) {
                set_click_through(&app, !inside);
                click_through = Some(!inside);
                applog(&format!(
                    "click-through {} at cursor_rel=({lx:.0},{ly:.0}) rects={rects:?}",
                    if inside { "off (cursor on the notch)" } else { "on (cursor elsewhere)" }
                ));
            }

            static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                applog(&format!(
                    "watchdog: cursor_rel=({lx:.0},{ly:.0}) inside={inside} rects={rects:?} winpos=({},{})",
                    pos.x, pos.y
                ));
            }

            if !EXPANDED.load(std::sync::atomic::Ordering::Relaxed) {
                miss = 0;
                continue;
            }
            if inside {
                miss = 0;
            } else {
                miss += 1;
                if miss >= need {
                    miss = 0;
                    EXPANDED.store(false, std::sync::atomic::Ordering::Relaxed);
                    let _ = app.emit("pointer_left", ());
                }
            }
        }
    });
}

/// Log channel for the page: JS writes key diagnostics into run.log (if invoke itself fails, the page reports on screen instead)
#[tauri::command]
fn log_js(msg: String) {
    applog(&format!("js: {}", msg.chars().take(600).collect::<String>()));
}

#[tauri::command]
fn open_usage_page() {
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", "https://claude.ai/settings/usage"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn focus_session(app: AppHandle, id: String) -> bool {
    let ppid = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.ppid_of(&id)
    };
    match ppid {
        Some(p) => focus::focus_terminal(p),
        None => focus::focus_claude_desktop(),
    }
}

#[tauri::command]
fn dismiss_session(app: AppHandle, id: String) {
    {
        let st = app.state::<AppState>();
        let mut store = st.store.lock().unwrap();
        store.dismiss(&id);
    }
    broadcast(&app);
}

#[tauri::command]
fn set_lang(app: AppHandle, lang: String) {
    apply_lang(&app, &lang);
}

// ---------------- notch size ----------------

#[tauri::command]
fn get_scale(app: AppHandle) -> f64 {
    ui_scale(&app)
}

/// Called by the slider on every move. Only the value is stored here: the page scales the pill
/// itself with a CSS zoom, so the window is never resized and the hover card holding the slider
/// keeps its size — otherwise the slider would shrink away from under the cursor mid-drag.
#[tauri::command]
fn set_scale(app: AppHandle, scale: f64) {
    let value = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.scale = config::snap_scale(scale);
        config::save(&c);
        c.scale
    };
    // The notch draws its own size, so it has to be told. Without this the slider in the settings
    // window saved the value but nothing changed on screen until the app was restarted.
    let _ = app.emit("scale", value);
}

#[tauri::command]
fn get_percent_basis(app: AppHandle) -> String {
    app.state::<AppState>()
        .cfg
        .lock()
        .map(|c| c.percent_basis.clone())
        .unwrap_or_else(|_| "remaining".into())
}

#[tauri::command]
fn set_percent_basis(app: AppHandle, basis: String) -> String {
    let value = if basis == "used" { "used" } else { "remaining" }.to_string();
    if let Ok(mut cfg) = app.state::<AppState>().cfg.lock() {
        cfg.percent_basis = value.clone();
        config::save(&cfg);
    }
    let _ = app.emit("percent_basis", &value);
    repaint_tray(&app);
    value
}

// ---------------- tray icon readings ----------------

/// The tightest metered window, ties going to the lower id so the choice never flickers. A `count`
/// window (Antigravity's requests today) has no published denominator, so it is never a candidate.
fn tightest<'a>(
    windows: impl Iterator<Item = &'a usage::LimitWindow>,
) -> Option<&'a usage::LimitWindow> {
    windows
        .filter(|w| w.count.is_none())
        .max_by(|a, b| a.used.total_cmp(&b.used).then_with(|| b.id.cmp(&a.id)))
}

/// The window a provider's ring shows, declared per provider as the macOS providers declare
/// `headlineID`: a window dropping out of a reply shows a dash instead of promoting another one
/// into its place. `headlineOf` in ui/notch.html is the same rule, so the ring and the tray agree.
///
/// A provider's own deterministic default, unrelated to any ring-limit choice — the priority chain
/// that decides between windows that are really aliases of the same thing (Claude's plan can report
/// `weekly_all`, `weekly_scoped` and `weekly_opus` in the same reply; only one of them is the number
/// that actually gates the plan, and it must never lose to whichever alias happens to read higher).
fn provider_default<'a>(provider: &str, windows: &'a [usage::LimitWindow]) -> Option<&'a usage::LimitWindow> {
    let by_id = |id: &str| windows.iter().find(|w| w.id == id);
    match provider {
        "claude" => by_id("weekly_all")
            .or_else(|| by_id("seven_day"))
            .or_else(|| by_id("weekly_scoped"))
            .or_else(|| by_id("weekly_opus"))
            .or_else(|| by_id("session")),
        "codex" => by_id("secondary").or_else(|| windows.first()),
        // Cursor has no weekly window (its allowance resets on the billing cycle instead)
        "cursor" => by_id("included").or_else(|| by_id("api")),
        "grok" => by_id("credits"), // Grok Build is already the weekly pool
        "opencode" => by_id("weekly").or_else(|| by_id("rolling")),
        "glm" => by_id("session").or_else(|| by_id("weekly")),
        "devin" => windows.first(),
        "commandcode" => by_id("monthly").or_else(|| windows.first()),
        "kimi" => by_id("rolling").or_else(|| by_id("weekly")),
        "copilot" => by_id("premium_interactions").or_else(|| windows.first()),
        "kiro" => by_id("credits").or_else(|| windows.first()),
        _ => None,
    }
}

/// `ring_limit` was Antigravity's own "Notch reads" choice; it now applies to every provider (떡배님's
/// call): "automatic" keeps each provider's own default above, and "5h" / "weekly" / "monthly" pick
/// whichever of that provider's own windows matches, falling back to the default when it has none.
///
/// "weekly" is also the overall default (every provider starts there), so it must never second-guess
/// `provider_default` by re-picking among a provider's own weekly-ish aliases by raw usage instead of
/// its declared priority — that is what let a barely-touched `weekly_all` lose to a near-exhausted
/// `weekly_opus` before this was caught in review. Only a limit the provider's own default does *not*
/// already sit in (asking Claude for "monthly", say) falls through to the keyword search below.
fn ring_window<'a>(
    provider: &str,
    windows: &'a [usage::LimitWindow],
    ring_limit: &str,
    antigravity_model: &str,
) -> Option<&'a usage::LimitWindow> {
    // Antigravity keeps its own model-family filtering; antigravity_lane already understands
    // "automatic" plus every explicit limit, so it takes the whole decision by itself.
    if provider == "gemini" {
        return antigravity_lane(windows, ring_limit, antigravity_model);
    }
    let default = provider_default(provider, windows);
    if ring_limit == "automatic" {
        return default;
    }
    if ring_limit == "weekly" {
        if let Some(w) = default {
            if lane_is(w, "weekly") {
                return Some(w);
            }
        }
    }
    tightest(windows.iter().filter(|w| lane_is(w, ring_limit))).or(default)
}

/// Antigravity's lane, chosen as the Mac app's "Notch reads" and "Model data" choose it: within the
/// model family (or every lane, if none belongs to it), the tightest lane of the chosen cadence; on
/// Automatic, the tightest lane that still has room, or the tightest of all once every one is spent.
fn antigravity_lane<'a>(
    windows: &'a [usage::LimitWindow],
    limit: &str,
    model: &str,
) -> Option<&'a usage::LimitWindow> {
    let family: Vec<_> = windows.iter().filter(|w| lane_family(w) == model).collect();
    let lanes = if family.is_empty() { windows.iter().collect() } else { family };
    if limit != "automatic" {
        if let Some(w) = tightest(lanes.iter().copied().filter(|w| lane_is(w, limit))) {
            return Some(w);
        }
    }
    tightest(lanes.iter().copied().filter(|w| w.used < 1.0))
        .or_else(|| tightest(lanes.iter().copied()))
        .or_else(|| lanes.first().copied())
}

/// "gemini" or "3p", from the language server's `gemini-5h` ids or the CLI's "Gemini Models …" ones
fn lane_family(w: &usage::LimitWindow) -> &'static str {
    let id = w.id.to_lowercase();
    if id.starts_with("gemini") {
        "gemini"
    } else if id.starts_with("3p") || id.starts_with("claude") {
        "3p"
    } else {
        ""
    }
}

/// Whether a lane is the 5-hour, weekly or monthly one, by the words the Mac app looks for
fn lane_is(w: &usage::LimitWindow, limit: &str) -> bool {
    let text = format!("{} {}", w.id, w.label).to_lowercase();
    match limit {
        "weekly" => text.contains("weekly") || text.contains("week") || text.contains("seven_day"),
        "monthly" => text.contains("month") || text.contains("30d"),
        _ => ["5h", "5-hour", "5 hour", "five hour", "five-hour", "hourly", "session", "rolling"]
            .iter()
            .any(|k| text.contains(k)),
    }
}

/// Ids match the ones the page uses, so the tray, the settings window and the notch all agree.
fn snapshot_of(app: &AppHandle, id: &str) -> usage::UsageSnapshot {
    let st = app.state::<AppState>();
    match id {
        "codex" => st.codex.lock().unwrap().clone(),
        "cursor" => st.cursor.lock().unwrap().clone(),
        "gemini" => st.antigravity.lock().unwrap().clone(),
        "grok" => st.grok.lock().unwrap().clone(),
        "opencode" => st.opencode.lock().unwrap().clone(),
        "glm" => st.glm.lock().unwrap().clone(),
        "devin" => st.devin.lock().unwrap().clone(),
        "commandcode" => st.commandcode.lock().unwrap().clone(),
        "kimi" => st.kimi.lock().unwrap().clone(),
        "copilot" => st.copilot.lock().unwrap().clone(),
        "kiro" => st.kiro.lock().unwrap().clone(),
        _ => st.usage.lock().unwrap().clone(),
    }
}

/// A provider's own ring metric — "weekly" unless it has its own entry in `ring_limits` (떡배님's
/// ask: each provider is switched independently, not all of them at once).
fn ring_limit_for(cfg: &config::Config, provider: &str) -> String {
    cfg.ring_limits.get(provider).cloned().unwrap_or_else(|| "weekly".into())
}

/// A provider's ring as a whole percentage, for the tray icon and the settings picker. A count
/// window has no percentage to draw, so it is a dash.
fn ring_pct(app: &AppHandle, provider: &str) -> Option<u32> {
    let snap = snapshot_of(app, provider);
    if snap.status == "absent" {
        return None;
    }
    let (limit, model, basis) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (ring_limit_for(&c, provider), c.antigravity_model.clone(), c.percent_basis.clone())
    };
    ring_window(provider, &snap.windows, &limit, &model)
        .filter(|w| w.count.is_none())
        .map(|w| displayed_percent(w.used, &basis))
}

fn displayed_percent(used: f64, basis: &str) -> u32 {
    let used = used.clamp(0.0, 1.0);
    let displayed = if basis == "used" { used } else { 1.0 - used };
    (displayed * 100.0).round().clamp(0.0, 100.0) as u32
}

/// What one half of the icon shows: that provider's ring.
fn reading_for_slot(app: &AppHandle, slot: &config::TraySlot) -> Option<u32> {
    ring_pct(app, &slot.provider)
}

/// One provider and its ring's current number, for the settings window's picker.
#[derive(serde::Serialize)]
struct TrayOption {
    id: String,
    label: String,
    status: String,
    used: Option<u32>,
}

#[tauri::command]
fn get_tray_options(app: AppHandle) -> Vec<TrayOption> {
    TRAY_PROVIDER_IDS
        .iter()
        .map(|id| TrayOption {
            id: (*id).to_string(),
            label: provider_label(id).to_string(),
            status: snapshot_of(&app, id).status,
            used: ring_pct(&app, id),
        })
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TrayConfig {
    mode: String,
    slots: Vec<config::TraySlot>,
}

#[tauri::command]
fn get_tray_config(app: AppHandle) -> TrayConfig {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    TrayConfig { mode: c.tray_mode.clone(), slots: c.tray_slots.clone() }
}

#[tauri::command]
fn set_tray_config(app: AppHandle, cfg: TrayConfig) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.tray_mode = cfg.mode;
        c.tray_slots = cfg.slots;
        // Kept in step so an older build reading this file still shows something sensible
        c.tray_providers = c.tray_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
    }
    repaint_tray(&app);
    tray::refresh_menu(&app);
}

/// The real icon, as a picture, for the settings preview — so what is being edited cannot drift
/// from what the taskbar actually draws.
#[tauri::command]
fn get_tray_preview(app: AppHandle, cfg: TrayConfig) -> Option<String> {
    let values: Vec<Option<u32>> = cfg.slots.iter().map(|s| reading_for_slot(&app, s)).collect();
    let rgba = match cfg.mode.as_str() {
        "bars" if !values.is_empty() => trayicon::bars_rgba(&values),
        "numbers" if !values.is_empty() => trayicon::numbers_rgba(&values),
        _ => return None, // "off" shows the app's own mark, which the page draws itself
    };
    trayicon::to_data_url(&rgba)
}

const RING_LIMIT_VALUES: [&str; 4] = ["automatic", "5h", "weekly", "monthly"];

/// Every provider's ring metric ("Notch reads" in the Mac app, Antigravity-only there — 떡배님's
/// ask: each provider now keeps its own choice rather than sharing one).
#[tauri::command]
fn get_ring_limits(app: AppHandle) -> std::collections::HashMap<String, String> {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    TRAY_PROVIDER_IDS.iter().map(|id| (id.to_string(), ring_limit_for(&c, id))).collect()
}

/// Sets one provider's ring metric and returns every provider's, so a caller can just replace its
/// whole local copy. Unknown values are refused rather than stored.
#[tauri::command]
fn set_ring_limit(app: AppHandle, provider: String, limit: String) -> std::collections::HashMap<String, String> {
    let all = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        if RING_LIMIT_VALUES.contains(&limit.as_str()) {
            c.ring_limits.insert(provider, limit);
        }
        config::save(&c);
        TRAY_PROVIDER_IDS.iter().map(|id| (id.to_string(), ring_limit_for(&c, id))).collect::<std::collections::HashMap<_, _>>()
    };
    let _ = app.emit("ring_limits", &all);
    repaint_tray(&app);
    all
}

/// Antigravity's own "Model data" — which model family its lanes come from. Unrelated to the ring
/// metric above; still Antigravity-only, as in the Mac app.
#[tauri::command]
fn get_antigravity_model(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.antigravity_model.clone()
}

#[tauri::command]
fn set_antigravity_model(app: AppHandle, model: String) -> String {
    let value = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        if ["gemini", "3p"].contains(&model.as_str()) {
            c.antigravity_model = model;
        }
        config::save(&c);
        c.antigravity_model.clone()
    };
    let _ = app.emit("antigravity_model", &value);
    repaint_tray(&app);
    value
}

/// Which providers get a ring on the notch. An empty list means every provider.
#[tauri::command]
fn get_notch_slots(app: AppHandle) -> Vec<config::TraySlot> {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.notch_slots.clone()
}

#[tauri::command]
fn set_notch_slots(app: AppHandle, slots: Vec<config::TraySlot>) {
    let list = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_slots = slots;
        // Kept in step so an older build reading this file still shows the right providers
        c.notch_providers = c.notch_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
        c.notch_slots.clone()
    };
    // The notch is a separate window and draws its own cells, so it has to be told.
    let _ = app.emit("notch_slots", list);
}

/// The application's own icon, so the settings window shows what the taskbar shows.
#[tauri::command]
fn get_app_icon() -> Option<String> {
    trayicon::app_mark_data_url()
}

// ---------------- what is on screen at all ----------------

#[derive(serde::Serialize)]
struct UiFlags {
    notch_visible: bool,
    tray_visible: bool,
}

#[tauri::command]
fn get_ui_flags(app: AppHandle) -> UiFlags {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
}

/// Hiding both would leave the app running with nothing to click, so the tray icon is kept
/// whenever the notch is off. The answer says what was actually stored, so the settings window can
/// show the corrected state rather than a lie.
#[tauri::command]
fn set_ui_flags(app: AppHandle, notch_visible: bool, tray_visible: bool) -> UiFlags {
    let flags = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_visible = notch_visible;
        c.tray_visible = if notch_visible { tray_visible } else { true };
        config::save(&c);
        UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
    };
    apply_visibility(&app);
    flags
}

/// Puts the two switches into effect.
pub fn apply_visibility(app: &AppHandle) {
    let (notch, tray_on) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.notch_visible, c.tray_visible)
    };
    if let Some(w) = app.get_webview_window("notch") {
        if notch {
            let _ = w.show();
            place_notch(app);
            apply_always_on_top(app);
        } else {
            let _ = w.hide();
        }
    }
    if let Some(t) = app.tray_by_id("main") {
        let _ = t.set_visible(tray_on);
    }
}

// ---------------- always on top ----------------

#[tauri::command]
fn get_always_on_top(app: AppHandle) -> bool {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.always_on_top
}

#[tauri::command]
fn set_always_on_top(app: AppHandle, on: bool) -> bool {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.always_on_top = on;
        config::save(&c);
    }
    apply_always_on_top(&app);
    let _ = app.emit("always_on_top", on);
    on
}

/// Applies both Tauri's remembered flag and the native Z-order operation. Repeating only
/// `set_always_on_top(true)` can be a no-op once Tauri already believes the flag is set, even after
/// another Windows topmost surface has moved above us. `SetWindowPos(HWND_TOPMOST, …)` always
/// reasserts the actual HWND ordering without activating or focusing the notch.
fn force_window_topmost(app: &AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("notch") {
        let _ = w.set_always_on_top(on);
        #[cfg(windows)]
        if let Ok(tauri_hwnd) = w.hwnd() {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
                SWP_NOMOVE, SWP_NOSIZE,
            };
            let hwnd = HWND(tauri_hwnd.0);
            let insert_after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            if let Err(error) = unsafe {
                SetWindowPos(
                    hwnd,
                    insert_after,
                    0,
                    0,
                    0,
                    0,
                    SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            } {
                applog(&format!("native topmost={on} failed: {error}"));
            }
        }
    }
}

/// Puts the saved switch into effect. `false` clears the window's own topmost flag; `true` sets it
/// once here, and `start_always_on_top_watchdog` keeps reapplying it.
pub fn apply_always_on_top(app: &AppHandle) {
    let on = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        c.always_on_top
    };
    force_window_topmost(app, on);
}

fn enforce_saved_topmost(app: &AppHandle) {
    let on = {
        let st = app.state::<AppState>();
        st.cfg.lock().map(|c| c.always_on_top).unwrap_or(false)
    };
    if on {
        force_window_topmost(app, true);
    }
}

/// Windows can silently drop a topmost window's z-order — another app also asking for topmost, an
/// exclusive-fullscreen game, sometimes just waking from sleep — which is exactly 떡배님's report
/// ("다른 앱 뒤로 가는 경우가 있다"). Re-asserting every few seconds costs nothing while nothing
/// contests it, and wins the ordering back within a few seconds when something does.
fn start_always_on_top_watchdog(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let on = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            c.always_on_top
        };
        if on {
            enforce_saved_topmost(&app);
        }
    });
}

// ---------------- settings that used to live in the tray menu ----------------

#[tauri::command]
fn get_lang(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.lang.clone()
}

/// The settings WebView must use the same Windows locale as the tray. WebView2's
/// navigator.language can describe the browser runtime rather than the user locale.
#[tauri::command]
fn get_lang_resolved(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    resolved_lang(&c.lang)
}

#[tauri::command]
fn get_autostart() -> bool {
    autostart::is_enabled()
}

#[tauri::command]
fn set_autostart(on: bool) -> Result<String, String> {
    if on {
        autostart::enable()
    } else {
        autostart::disable()
    }
}

#[tauri::command]
fn get_hooks_installed() -> bool {
    hooks_install::is_installed()
}

#[tauri::command]
fn set_hooks_installed(on: bool) -> Result<String, String> {
    if on {
        hooks_install::install()
    } else {
        hooks_install::uninstall()
    }
}

#[tauri::command]
fn reset_notch_position(app: AppHandle) {
    reset_bar(&app);
}

// ---------------- update check ----------------

#[tauri::command]
fn get_update_info() -> updater::UpdateInfo {
    updater::last()
}

#[tauri::command]
fn get_upstream_update_info() -> updater::UpstreamInfo {
    updater::upstream_last()
}

/// Runs the network check off the invoking thread — the settings window awaits the promise, but a
/// blocking Tauri command would stall the WebView's own message loop while it waits on ureq.
#[tauri::command]
async fn check_for_update(app: AppHandle) -> updater::UpdateInfo {
    tauri::async_runtime::spawn_blocking(move || updater::check(&app)).await.unwrap_or_default()
}

#[tauri::command]
async fn check_upstream_update(app: AppHandle) -> updater::UpstreamInfo {
    tauri::async_runtime::spawn_blocking(move || updater::check_upstream(&app)).await.unwrap_or_default()
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || updater::download_and_launch(&app))
        .await
        .map_err(|e| format!("{e}"))?
}

/// Unlike the other `open_*_page` commands, this URL comes from GitHub's API rather than a
/// hardcoded literal — a compromised account or a MITM'd response could hand back something
/// cmd.exe would treat specially (`%VAR%` expands inside `cmd /C start` even inside quotes). Two
/// guards: only an actual https URL is ever opened, and it goes to `explorer.exe` as a single
/// argument rather than through `cmd /C start`, which never runs a shell over the string at all.
#[tauri::command]
fn open_release_page(url: String) {
    if !url.starts_with("https://") {
        return;
    }
    let mut cmd = std::process::Command::new("explorer");
    cmd.arg(&url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn close_settings(window: tauri::WebviewWindow) {
    // The settings webview is created from tauri.conf at startup and reused. Hiding mirrors the
    // Mac close button and lets the tray's Settings item show the same window again; destroying it
    // here would leave open_settings with nothing to reopen.
    let _ = window.hide();
}

#[tauri::command]
fn minimize_settings(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "cursor" => "Cursor",
        "gemini" => "Antigravity",
        "grok" => "Grok",
        "opencode" => "OpenCode",
        "glm" => "GLM",
        "devin" => "Devin",
        "commandcode" => "Command Code",
        "kimi" => "Kimi Code",
        "copilot" => "GitHub Copilot",
        "kiro" => "Kiro",
        _ => "Claude",
    }
}

/// Every provider the tray menu can offer, in the order the notch shows them.
pub const TRAY_PROVIDER_IDS: [&str; 12] = [
    "claude",
    "codex",
    "cursor",
    "gemini",
    "grok",
    "opencode",
    "glm",
    "devin",
    "commandcode",
    "kimi",
    "copilot",
    "kiro",
];

/// Draws the icon and writes the tooltip. Shared by the polling thread and by the settings window,
/// so a change made in settings shows up at once rather than on the next poll.
fn paint_tray(app: &AppHandle, mode: &str, slots: &[config::TraySlot], values: &[Option<u32>]) {
    let Some(tray) = app.tray_by_id("main") else {
        applog("tray: no tray with id 'main' — icon not updated");
        return;
    };
    let outcome = match mode {
        "numbers" if !values.is_empty() => tray.set_icon(Some(trayicon::numbers(values))),
        "bars" if !values.is_empty() => tray.set_icon(Some(trayicon::bars(values))),
        // "Plain icon": the application's own icon
        _ => match trayicon::app_mark() {
            Some(img) => tray.set_icon(Some(img)),
            None => Ok(()), // leave whatever icon is there rather than clearing it to nothing
        },
    };
    if let Err(e) = outcome {
        applog(&format!("tray: set_icon FAILED mode={mode} values={values:?}: {e}"));
    }
    // The tooltip lists every slot, including any the digit layout could not fit, so nothing is
    // silently dropped.
    let parts: Vec<String> = slots
        .iter()
        .zip(values.iter())
        .map(|(slot, v)| {
            format!(
                "{} {}",
                provider_label(&slot.provider),
                v.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into())
            )
        })
        .collect();
    let tip = if parts.is_empty() {
        concat!("Provider Monitor v", env!("CARGO_PKG_VERSION")).to_string()
    } else {
        format!("Provider Monitor — {}", parts.join(" · "))
    };
    let _ = tray.set_tooltip(Some(&tip));
}

/// Reads the current settings and readings, and repaints immediately.
pub fn repaint_tray(app: &AppHandle) {
    let (mode, slots) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.tray_mode.clone(), c.tray_slots.clone())
    };
    let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(app, s)).collect();
    paint_tray(app, &mode, &slots, &values);
}

/// Repaints when a reading changes. Every 2 seconds, but it only touches the icon when something
/// actually moved, so it costs nothing while idle.
fn start_tray_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last: Option<(String, Vec<config::TraySlot>, Vec<Option<u32>>)> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let (mode, slots) = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                (c.tray_mode.clone(), c.tray_slots.clone())
            };
            let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(&app, s)).collect();
            let key = (mode.clone(), slots.clone(), values.clone());
            if last.as_ref() == Some(&key) {
                continue;
            }
            last = Some(key);
            paint_tray(&app, &mode, &slots, &values);
        }
    });
}

/// Seen-clears-it: looking at a session acknowledges it (engine behaviour, unchanged)
#[cfg(windows)]
fn ack_scan(app: &AppHandle) -> bool {
    let need = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.has_done()
    };
    if !need {
        return false;
    }
    let fg = focus::fg_pid();
    if fg == 0 {
        return false;
    }
    let maps = focus::proc_maps();
    let fg_name = maps.name.get(&fg).cloned().unwrap_or_default();
    let fg_is_claude_desktop = fg_name.contains("claude") && !fg_name.contains("codenotch");
    let st = app.state::<AppState>();
    let mut store = st.store.lock().unwrap();
    store.ack_done(|s| {
        if s.ppid == 0 {
            fg_is_claude_desktop
        } else {
            focus::pid_hits_chain(fg, &focus::chain_of(s.ppid, &maps.ppid), &maps)
        }
    })
}
#[cfg(not(windows))]
fn ack_scan(_app: &AppHandle) -> bool {
    false
}

// ---------------- main ----------------

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_console() {}

fn report(r: Result<String, String>) {
    let msg = match r {
        Ok(m) => format!("OK: {m}"),
        Err(e) => format!("FAILED: {e}"),
    };
    println!("{msg}");
    let log = config::config_path().with_file_name("install.log");
    let _ = std::fs::write(log, &msg);
}

fn main() {
    attach_console();
    let args: Vec<String> = std::env::args().collect();
    if let Some(cmd) = args.get(1) {
        match cmd.as_str() {
            "install-hooks" => {
                report(hooks_install::install());
                return;
            }
            "uninstall-hooks" => {
                report(hooks_install::uninstall());
                return;
            }
            "autostart" => {
                let r = match args.get(2).map(|s| s.as_str()) {
                    Some("on") => autostart::enable(),
                    Some("off") => autostart::disable(),
                    _ => Err("usage: \"Provider Monitor.exe\" autostart on|off".into()),
                };
                report(r);
                return;
            }
            "doctor" => {
                let out = if args.get(2).map(|s| s.as_str()) == Some("deep") { diag::run() } else { doctor::run() };
                println!("{out}");
                let log = config::config_path().with_file_name("doctor.log");
                let _ = std::fs::write(log, &out);
                return;
            }
            _ => {}
        }
    }

    let cfg = config::load();
    let port = cfg.port;

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Launching a freshly built exe while the old one is still running lands here: the new
            // instance is turned away and what stays on screen is the old process. Say so loudly.
            applog(&format!("single instance: another launch was refused; the running instance is build={BUILD} — quit it from the tray first if you just rebuilt"));
            let _ = app.emit("notice", format!("Provider Monitor is already running ({BUILD}) — quit it from the tray before starting a new build"));
        }))
        .manage(AppState {
            store: Mutex::new(Default::default()),
            cfg: Mutex::new(cfg),
            usage: Mutex::new(usage::load_persisted()),
            codex: Mutex::new(codex::load_persisted()),
            cursor: Mutex::new(cursor::load_persisted()),
            antigravity: Mutex::new(antigravity::load_persisted()),
            grok: Mutex::new(grok::load_persisted()),
            opencode: Mutex::new(opencode::load_persisted()),
            glm: Mutex::new(glm::load_persisted()),
            devin: Mutex::new(devin::load_persisted()),
            commandcode: Mutex::new(command_code::load_persisted()),
            kimi: Mutex::new(kimi::load_persisted()),
            copilot: Mutex::new(copilot::load_persisted()),
            kiro: Mutex::new(kiro::load_persisted()),
            glyphs: Mutex::new(Default::default()),
            activity: Mutex::new(Vec::new()),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_usage,
            get_codex,
            get_cursor,
            get_antigravity,
            get_grok,
            get_opencode,
            get_glm,
            get_devin,
            get_commandcode,
            get_kimi,
            get_copilot,
            get_kiro,
            get_glyphs,
            get_activity,
            open_data_dir,
            drag_begin,
            refresh_provider,
            get_double_click_time_ms,
            open_provider_page,
            refresh_usage,
            open_usage_page,
            set_hot,
            report_dpr,
            log_js,
            focus_session,
            dismiss_session,
            set_lang,
            get_scale,
            set_scale,
            get_percent_basis,
            set_percent_basis,
            get_displays,
            set_display,
            get_notch_edge,
            set_notch_edge,
            get_tray_options,
            get_tray_config,
            set_tray_config,
            get_tray_preview,
            get_notch_slots,
            set_notch_slots,
            get_ring_limits,
            set_ring_limit,
            get_antigravity_model,
            set_antigravity_model,
            get_app_icon,
            get_ui_flags,
            set_ui_flags,
            get_lang,
            get_lang_resolved,
            get_autostart,
            set_autostart,
            get_hooks_installed,
            set_hooks_installed,
            reset_notch_position,
            open_settings,
            get_update_info,
            get_upstream_update_info,
            check_for_update,
            check_upstream_update,
            install_update,
            open_release_page,
            get_always_on_top,
            set_always_on_top,
            close_settings,
            minimize_settings,
            quit_app
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            place_notch(&handle);
            if let Some(w) = handle.get_webview_window("notch") {
                let _ = w.show();
            }
            tray::setup(&handle)?;
            // Closing a Tauri window destroys it by default, and a destroyed window cannot be shown
            // again — which is why Settings opened once and then never again. Hide it instead.
            if let Some(w) = handle.get_webview_window("settings") {
                let hide_me = w.clone();
                w.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = hide_me.hide();
                    }
                });
            }
            start_tray_updater(handle.clone());
            // Honours the saved switches: a notch hidden last time stays hidden.
            apply_visibility(&handle);
            apply_always_on_top(&handle);
            start_always_on_top_watchdog(handle.clone());
            server::start(handle.clone(), port);
            watcher::start(handle.clone());
            usage::start(handle.clone());
            codex::start(handle.clone());
            cursor::start(handle.clone());
            antigravity::start(handle.clone());
            grok::start(handle.clone());
            opencode::start(handle.clone());
            glm::start(handle.clone());
            devin::start(handle.clone());
            command_code::start(handle.clone());
            kimi::start(handle.clone());
            copilot::start(handle.clone());
            kiro::start(handle.clone());
            activity::start(handle.clone());
            // Collecting glyphs may read icon resources out of a few executables; do it off the main thread and push when done
            let gh = handle.clone();
            std::thread::spawn(move || reload_glyphs(&gh));
            start_pointer_watchdog(handle.clone());
            start_monitor_watcher(handle.clone());
            updater::start(handle.clone());
            // Seen-clears-it scan
            let acker = handle.clone();
            std::thread::spawn(move || {
                activity::lower_thread_priority();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    if ack_scan(&acker) {
                        broadcast(&acker);
                    }
                }
            });
            // Stale session cleanup
            let sweeper = handle.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let changed = {
                    let st = sweeper.state::<AppState>();
                    let mut s = st.store.lock().unwrap();
                    s.sweep()
                };
                if changed {
                    broadcast(&sweeper);
                }
            });
            // Persist the config (codenotch-hook reads the port from it)
            {
                let st = handle.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                config::save(&c);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Provider Monitor failed to start");
}

#[cfg(test)]
mod tests {
    use super::{
        along_origin_range, cursor_in_hot, displayed_percent, notch_window_size,
        origin_for_ratio, provider_page, ratio_for_origin, ring_window, HOT_PAD,
    };
    use crate::config::NotchEdge;
    use crate::usage::LimitWindow;

    /// Real values from the run.log in #106: a 2560×1600 display at 150 %.
    const PILL: [f64; 4] = [405.0, 183.5, 105.0, 323.0];
    const CARD: [f64; 4] = [21.0, 142.5, 369.0, 262.0];
    const WINDOW: Option<(f64, f64)> = Some((510.0, 690.0));

    #[test]
    fn requested_provider_pages_are_exact_and_existing_claude_and_grok_links_stay_put() {
        assert_eq!(provider_page("codex"), "https://chatgpt.com/codex/cloud/settings/analytics#usage");
        assert_eq!(provider_page("claude"), "https://claude.ai/settings/usage");
        assert_eq!(provider_page("gemini"), "https://gemini.google.com/app");
        assert_eq!(provider_page("grok"), "https://grok.com/?_s=usage");
        assert_eq!(provider_page("opencode"), "https://opencode.ai/workspace/wrk_01M02ADN1RXR7S9P9S5BYPPAGT/go");
    }

    #[test]
    fn horizontal_edges_rotate_the_window_without_changing_its_area() {
        assert_eq!(notch_window_size(NotchEdge::Right, 1.5), tauri::PhysicalSize::new(540, 1050));
        assert_eq!(notch_window_size(NotchEdge::Left, 1.5), tauri::PhysicalSize::new(540, 1050));
        assert_eq!(notch_window_size(NotchEdge::Top, 1.5), tauri::PhysicalSize::new(1050, 1050));
        assert_eq!(notch_window_size(NotchEdge::Bottom, 1.5), tauri::PhysicalSize::new(1050, 1050));
    }

    #[test]
    fn transparent_slack_may_leave_the_screen_so_the_visible_pill_reaches_both_ends() {
        // 700px window with the visible pill from 100...600 on a 1080px display.
        let (lo, hi) = along_origin_range(0, 1080, 700, Some((100.0, 500.0)));
        assert_eq!((lo, hi), (-100, 480));
        assert_eq!(origin_for_ratio(lo, hi, 0.0), -100);
        assert_eq!(origin_for_ratio(lo, hi, 1.0), 480);
        assert_eq!(ratio_for_origin(lo, hi, -100), 0.0);
        assert_eq!(ratio_for_origin(lo, hi, 480), 1.0);
    }

    #[test]
    fn nothing_is_hot_before_the_page_reports() {
        assert!(!cursor_in_hot(&[], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_pill_is_hot() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_transparent_area_beside_the_pill_is_not() {
        assert!(!cursor_in_hot(&[PILL], 0.0, 297.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 100.0, 400.0, WINDOW));
    }

    #[test]
    fn the_card_is_hot_while_it_is_open() {
        assert!(!cursor_in_hot(&[PILL], 100.0, 250.0, WINDOW));
        assert!(cursor_in_hot(&[PILL, CARD], 100.0, 250.0, WINDOW));
    }

    #[test]
    fn the_tail_leaves_no_cold_strip_between_the_pill_and_the_card() {
        // The shipped layout at 150 %, from the CSS: the card stops 100 px from the edge and the
        // tail spans the rest, its tip under the pill's edge. A pointer crossing along the tail is
        // hot on one rectangle alone at every step, so it never leans on the bounding box.
        const WIDE: Option<(f64, f64)> = Some((540.0, 690.0));
        const WIDE_PILL: [f64; 4] = [435.0, 183.5, 105.0, 323.0];
        const TAIL: [f64; 4] = [388.5, 318.0, 48.0, 54.0];
        let y = TAIL[1] + TAIL[3] / 2.0;
        for x in (CARD[0] + CARD[2]) as i32..WIDE_PILL[0] as i32 {
            let x = x as f64;
            assert!(
                [WIDE_PILL, TAIL, CARD].iter().any(|r| cursor_in_hot(&[*r], x, y, WIDE)),
                "cold at x={x}"
            );
        }
    }

    /// Far enough apart that the pads do not meet — the case the bounding box exists for.
    const FAR_A: [f64; 4] = [0.0, 0.0, 50.0, 50.0];
    const FAR_B: [f64; 4] = [200.0, 0.0, 50.0, 50.0];

    #[test]
    fn a_wide_gap_is_bridged_by_the_bounding_box() {
        assert!(cursor_in_hot(&[FAR_A, FAR_B], 125.0, 25.0, None));
    }

    #[test]
    fn the_bounding_box_needs_two_rectangles_to_bridge_anything() {
        assert!(!cursor_in_hot(&[FAR_A], 125.0, 25.0, None));
    }

    #[test]
    fn the_pad_reaches_slightly_past_the_pill() {
        assert!(cursor_in_hot(&[PILL], PILL[0] - HOT_PAD + 1.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], PILL[0] - HOT_PAD - 1.0, 300.0, WINDOW));
    }

    #[test]
    fn a_cursor_off_the_window_is_never_hot() {
        assert!(!cursor_in_hot(&[PILL], 515.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 450.0, -5.0, WINDOW));
    }

    #[test]
    fn an_unreadable_window_size_falls_back_to_the_rectangles() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, None));
        assert!(!cursor_in_hot(&[PILL], 100.0, 300.0, None));
    }

    fn win(id: &str, used: f64) -> LimitWindow {
        LimitWindow { id: id.into(), used, ..Default::default() }
    }

    fn pick<'a>(provider: &str, windows: &'a [LimitWindow]) -> Option<&'a str> {
        ring_window(provider, windows, "automatic", "gemini").map(|w| w.id.as_str())
    }

    fn lane<'a>(windows: &'a [LimitWindow], limit: &str, model: &str) -> Option<&'a str> {
        ring_window("gemini", windows, limit, model).map(|w| w.id.as_str())
    }

    /// The four lanes Antigravity's language server reported on a real machine
    fn bridge() -> [LimitWindow; 4] {
        [win("gemini-weekly", 0.03), win("gemini-5h", 0.0), win("3p-weekly", 0.5), win("3p-5h", 0.9)]
    }

    #[test]
    fn claude_means_weekly_even_when_the_session_is_fuller() {
        assert_eq!(pick("claude", &[win("session", 0.60), win("weekly_all", 0.10)]), Some("weekly_all"));
    }

    #[test]
    fn claude_falls_back_through_the_weekly_aliases_then_the_session() {
        assert_eq!(pick("claude", &[win("session", 0.1), win("seven_day", 0.2)]), Some("seven_day"));
        assert_eq!(pick("claude", &[win("session", 0.1), win("weekly_scoped", 0.2)]), Some("weekly_scoped"));
        assert_eq!(pick("claude", &[win("session", 0.1)]), Some("session"));
    }

    #[test]
    fn a_missing_declared_window_is_a_dash_not_a_stand_in() {
        assert_eq!(pick("claude", &[win("something_else", 0.60)]), None);
    }

    #[test]
    fn codex_means_its_weekly_secondary_window_and_cursor_its_included_usage() {
        assert_eq!(pick("codex", &[win("primary", 0.2), win("secondary", 0.9)]), Some("secondary"));
        assert_eq!(pick("codex", &[win("primary", 0.2)]), Some("primary"));
        assert_eq!(pick("cursor", &[win("included", 0.3), win("api", 0.9)]), Some("included"));
        assert_eq!(pick("cursor", &[win("api", 0.9), win("on_demand", 0.95)]), Some("api"));
        assert_eq!(pick("opencode", &[win("rolling", 0.1), win("weekly", 0.4)]), Some("weekly"));
        assert_eq!(pick("opencode", &[win("rolling", 0.1)]), Some("rolling"));
    }

    #[test]
    fn antigravity_reads_only_gemini_lanes_unless_told_otherwise() {
        assert_eq!(lane(&bridge(), "automatic", "gemini"), Some("gemini-weekly"));
        assert_eq!(lane(&bridge(), "automatic", "3p"), Some("3p-5h"));
    }

    #[test]
    fn notch_reads_picks_the_five_hour_or_the_weekly_lane() {
        assert_eq!(lane(&bridge(), "5h", "gemini"), Some("gemini-5h"));
        assert_eq!(lane(&bridge(), "weekly", "3p"), Some("3p-weekly"));
    }

    #[test]
    fn the_cli_names_its_lanes_differently_and_still_matches() {
        let cli = [
            win("Gemini Models Weekly Limit", 0.2),
            win("Gemini Models Five Hour Limit", 0.1),
            win("Claude and GPT models Five Hour Limit", 0.7),
        ];
        assert_eq!(lane(&cli, "5h", "gemini"), Some("Gemini Models Five Hour Limit"));
        assert_eq!(lane(&cli, "automatic", "3p"), Some("Claude and GPT models Five Hour Limit"));
    }

    #[test]
    fn a_spent_lane_leads_only_once_every_lane_is_spent() {
        let one_spent = [win("gemini-5h", 1.0), win("gemini-weekly", 0.4)];
        assert_eq!(lane(&one_spent, "automatic", "gemini"), Some("gemini-weekly"));
        let all_spent = [win("gemini-weekly", 1.0), win("gemini-5h", 1.0)];
        assert_eq!(lane(&all_spent, "automatic", "gemini"), Some("gemini-5h"));
    }

    #[test]
    fn a_request_count_still_leads_when_it_is_all_there_is() {
        let requests = LimitWindow { id: "requests".into(), count: Some(79), ..Default::default() };
        assert_eq!(pick("gemini", std::slice::from_ref(&requests)), Some("requests"));
    }

    /// The ring-limit toggle used to be Antigravity-only; it now reaches every provider, picking
    /// whichever of that provider's own windows matches the chosen cadence.
    fn ring_for<'a>(provider: &str, windows: &'a [LimitWindow], limit: &str) -> Option<&'a str> {
        ring_window(provider, windows, limit, "gemini").map(|w| w.id.as_str())
    }

    #[test]
    fn an_explicit_limit_overrides_a_providers_own_default() {
        // Claude's own default is weekly_all; a window that actually matches "monthly" text must
        // win over it even though weekly_all is what "automatic" would have picked.
        let ws = [win("weekly_all", 0.1), LimitWindow { id: "monthly_extra".into(), label: "Monthly limit".into(), used: 0.9, ..Default::default() }];
        assert_eq!(ring_for("claude", &ws, "monthly"), Some("monthly_extra"));
    }

    /// Regression for a review finding: Claude can report weekly_all, weekly_scoped and
    /// weekly_opus in the same reply, and all three contain "weekly" — asking for the "weekly"
    /// ring (the overall default) must not let a heavily-used alias outrank weekly_all, the one
    /// that actually decides whether the plan survives the week.
    #[test]
    fn weekly_never_lets_a_fuller_alias_outrank_weekly_all() {
        let ws = [win("weekly_all", 0.05), win("weekly_opus", 0.99), win("session", 0.5)];
        assert_eq!(ring_for("claude", &ws, "weekly"), Some("weekly_all"));
    }

    #[test]
    fn a_provider_with_no_matching_lane_keeps_its_own_default() {
        // Cursor has no weekly window at all; asking for one anyway must not blank the ring out.
        let ws = [win("included", 0.3), win("api", 0.9)];
        assert_eq!(ring_for("cursor", &ws, "weekly"), Some("included"));
    }

    #[test]
    fn monthly_is_matched_by_id_or_label() {
        let ws = [win("weekly", 0.1), win("monthly", 0.4)];
        assert_eq!(ring_for("opencode", &ws, "monthly"), Some("monthly"));
        let codex_ws = [
            LimitWindow { id: "primary".into(), label: "5h limit".into(), used: 0.1, ..Default::default() },
            LimitWindow { id: "secondary".into(), label: "Monthly limit".into(), used: 0.6, ..Default::default() },
        ];
        assert_eq!(ring_for("codex", &codex_ws, "monthly"), Some("secondary"));
    }

    #[test]
    fn tray_graph_and_number_follow_the_selected_percent_basis() {
        assert_eq!(displayed_percent(0.2, "used"), 20);
        assert_eq!(displayed_percent(0.2, "remaining"), 80);
        assert_eq!(displayed_percent(1.4, "remaining"), 0);
        assert_eq!(displayed_percent(-0.4, "remaining"), 100);
    }
}
