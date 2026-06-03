use std::collections::BTreeMap;
use std::time::Instant;
use zellij_tile::prelude::*;

use crate::bar::config::{Config, InfoWidget, Visibility};
use crate::bar::state::{AppState, WidgetSample};
use crate::shared::alarms::{self, AlarmKind, AlarmOutcome};

pub const CTX_KEY: &str = "source";
pub const CTX_GHOSTTY_FULLSCREEN: &str = "ghostty_fullscreen";
pub const CTX_TZ_OFFSET: &str = "tz_offset";
pub const CTX_WEZTERM_FULLSCREEN: &str = "wezterm_fullscreen";
pub const CTX_WIDGET: &str = "widget";
pub const CTX_WIDGET_ID: &str = "widget_id";

const GHOSTTY_FULLSCREEN_SCRIPT: &str = r#"
tell application "System Events"
    if not (exists process "Ghostty") then return "NOT_RUNNING"
    set p to first process whose name is "Ghostty"
    if (count of windows of p) > 0 then
        tell window 1 of p
            if value of attribute "AXFullScreen" is true then return "NATIVE_FULLSCREEN"
            set sz to size
            set wW to item 1 of sz
            set wH to item 2 of sz
        end tell
        tell application "Finder" to set scr to bounds of window of desktop
        set sW to item 3 of scr
        set sH to item 4 of scr
        if wW >= sW and wH >= sH then
            return "NON_NATIVE_FULLSCREEN"
        else
            return "WINDOWED"
        end if
    else
        return "NO_WINDOWS"
    end if
end tell
"#;

const WEZTERM_FULLSCREEN_SCRIPT: &str = r#"
state_home=${XDG_STATE_HOME:-$HOME/.local/state}
for path in "$state_home/wezterm/fullscreen_state" "$HOME/.local/state/wezterm/fullscreen_state"; do
    if [ -r "$path" ]; then
        IFS= read -r line < "$path" || true
        printf '%s\n' "$line"
        exit 0
    fi
done
exit 1
"#;

pub fn should_show_system_segments(
    state: &AppState,
    fullscreen_min_cols: usize,
    cols: usize,
) -> bool {
    if !state.got_permissions {
        return false;
    }

    let active_tab_idx = state.active_tab_index().unwrap_or(0);
    let is_zellij_pane_fullscreen = state.any_pane_zoomed(active_tab_idx);
    let is_non_graphical = std::env::var("DISPLAY").is_err()
        && std::env::var("WAYLAND_DISPLAY").is_err()
        && std::env::var("TERM_PROGRAM").is_err();

    if is_zellij_pane_fullscreen || is_non_graphical {
        return true;
    }

    if let Some(is_fullscreen) = state.terminal_fullscreen.value {
        return is_fullscreen;
    }

    if is_running_in_ghostty() {
        return state.ghostty_fullscreen.value.unwrap_or(false);
    }
    if is_running_in_wezterm() {
        return state.wezterm_fullscreen.value.unwrap_or(false);
    }

    cols >= fullscreen_min_cols
}

/// Evaluate a `Visibility` choice against the current display state. The
/// `min_cols_override` lets per-segment / per-widget configuration tighten or
/// relax the global `fullscreen_min_cols` threshold.
pub fn is_visible(
    visibility: Visibility,
    min_cols_override: Option<usize>,
    state: &AppState,
    config: &Config,
    cols: usize,
) -> bool {
    match visibility {
        Visibility::Always => true,
        Visibility::Never => false,
        Visibility::Fullscreen => {
            let threshold = min_cols_override.unwrap_or(config.fullscreen_min_cols);
            should_show_system_segments(state, threshold, cols)
        }
    }
}

pub fn maybe_refresh_ghostty_fullscreen(state: &mut AppState) {
    request_ghostty_fullscreen(state, false);
}

pub fn refresh_ghostty_fullscreen_now(state: &mut AppState) {
    request_ghostty_fullscreen(state, true);
}

pub fn maybe_refresh_wezterm_fullscreen(state: &mut AppState) {
    request_wezterm_fullscreen(state, false);
}

pub fn refresh_wezterm_fullscreen_now(state: &mut AppState) {
    request_wezterm_fullscreen(state, true);
}

fn request_ghostty_fullscreen(state: &mut AppState, force: bool) {
    if !is_running_in_ghostty() || state.ghostty_fullscreen.in_flight {
        return;
    }
    if !force && !state.ghostty_fullscreen.is_expired() {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_GHOSTTY_FULLSCREEN.to_string());
    run_command(&["osascript", "-e", GHOSTTY_FULLSCREEN_SCRIPT], ctx);
    state.ghostty_fullscreen.in_flight = true;
}

fn is_running_in_ghostty() -> bool {
    term_program_is("ghostty")
}

fn is_running_in_wezterm() -> bool {
    term_program_is("wezterm")
}

fn term_program_is(name: &str) -> bool {
    std::env::var("TERM_PROGRAM")
        .map(|term| term.eq_ignore_ascii_case(name))
        .unwrap_or(false)
}

fn request_wezterm_fullscreen(state: &mut AppState, force: bool) {
    if !is_running_in_wezterm() || state.wezterm_fullscreen.in_flight {
        return;
    }
    if !force && !state.wezterm_fullscreen.is_expired() {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_WEZTERM_FULLSCREEN.to_string());
    run_command(&["sh", "-c", WEZTERM_FULLSCREEN_SCRIPT], ctx);
    state.wezterm_fullscreen.in_flight = true;
}

// ─── Timezone offset ──────────────────────────────────────────────────────────

/// Refresh the cached UTC offset by shelling out to `date +%z` if the cache
/// has expired (or has never been populated). Idempotent — repeated calls
/// while a previous probe is in flight are no-ops.
///
/// Zellij plugins are WASI guests and only see `/host`, `/data`, and `/tmp`
/// — `/etc/localtime` is not reachable, and `iana-time-zone` (used by
/// `chrono::Local`) returns an error for `wasm32-wasip1`, so chrono silently
/// falls back to UTC. Sampling `date +%z` on the host gives us the offset
/// the OS would use, including DST. The 30-minute cache TTL is short enough
/// that a DST transition is corrected on its own within half an hour.
pub fn maybe_refresh_tz_offset(state: &mut AppState) {
    if state.tz_offset.in_flight || !state.tz_offset.is_expired() {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_TZ_OFFSET.to_string());
    run_command(&["date", "+%z"], ctx);
    state.tz_offset.in_flight = true;
}

/// Parse the `date +%z` output (`±HHMM`, possibly with a trailing newline)
/// into a signed offset in seconds. Returns `None` on any deviation from
/// that shape so callers can keep the previously cached value instead of
/// snapping to a garbage offset.
fn parse_tz_offset(output: &str) -> Option<i32> {
    let s = output.trim();
    let bytes = s.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign: i32 = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if !bytes[1..].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = s[1..3].parse().ok()?;
    let minutes: i32 = s[3..5].parse().ok()?;
    if minutes >= 60 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Refresh the cached title for a single terminal pane by ID.
///
/// Called from the `PaneRenderReport` event handler whenever Zellij tells us
/// a pane we care about just rendered. We re-query `get_pane_info` to read the
/// pane's current OSC title (which lives in `grid.title`, separate from the
/// viewport contents the report payload carries) and patch our cached
/// `PaneInfo` if it changed.
pub fn refresh_pane_title_by_id(state: &mut AppState, pane_id: u32) {
    let Some(fresh) = get_pane_info(PaneId::Terminal(pane_id)) else {
        return;
    };
    for panes in state.panes.values_mut() {
        if let Some(slot) = panes.iter_mut().find(|p| p.id == pane_id && !p.is_plugin) {
            if slot.title != fresh.title {
                slot.title = fresh.title;
                state.dirty = true;
            }
            return;
        }
    }
}

/// Backstop poll of every tab's focused-pane title.
///
/// `PaneRenderReport` only fires when a pane's *viewport* changes, but
/// Zellij's OSC 0/2 handler in `grid.rs:osc_dispatch` updates `grid.title`
/// without calling `mark_for_rerender`. A script that only emits
/// `\e]2;newtitle\a` and no visible output therefore never triggers a render
/// report. On top of that, the render report only ever covers the active
/// tab's panes, so a background tab whose title changes (e.g. a Cursor agent
/// updating its window title while you work in another tab) is never
/// re-read. We therefore poll the focused terminal pane of *every* tab here,
/// patching cached titles that drifted. `get_pane_info` is keyed by pane id
/// and works cross-tab, so this needs no per-tab focus. Worst case it does one
/// `get_pane_info` per open tab per slow tick.
pub fn refresh_all_tab_titles(state: &mut AppState) {
    let tab_positions: Vec<usize> = state.panes.keys().copied().collect();
    for tab_position in tab_positions {
        let Some(pane_id) = state.focused_pane_for_tab(tab_position).map(|pane| pane.id) else {
            continue;
        };
        let Some(fresh) = get_pane_info(PaneId::Terminal(pane_id)) else {
            continue;
        };
        let Some(panes) = state.panes.get_mut(&tab_position) else {
            continue;
        };
        let Some(slot) = panes.iter_mut().find(|p| p.id == fresh.id && !p.is_plugin) else {
            continue;
        };
        if slot.title != fresh.title {
            slot.title = fresh.title;
            state.dirty = true;
        }
    }
}

// ─── Alarms ───────────────────────────────────────────────────────────────────

/// Current wall-clock time in epoch seconds.
///
/// Zellij plugins run in a WASI sandbox where `chrono::Local` falls back to
/// UTC, but `chrono::Utc::now()` works — and for a monotonic-ish epoch delta
/// that is all the idle countdown needs.
pub fn now_epoch() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

/// Deterministic fingerprint of a pane's visible viewport. Uses
/// `DefaultHasher` (fixed keys, no per-process randomization) so the value is
/// stable across bar instances — essential for the idle baseline to survive a
/// tab switch handing monitoring to a different instance.
fn hash_viewport(viewport: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in viewport {
        line.hash(&mut hasher);
        0u8.hash(&mut hasher); // line boundary, so [\"ab\"] and [\"a\",\"b\"] differ
    }
    hasher.finish()
}

/// Sample a terminal pane's visible viewport and fingerprint it. `None` when
/// the pane could not be read (e.g. it just closed). Works cross-tab — the
/// query is keyed by pane id, not by which tab is active.
pub fn pane_content_hash(pane_id: u32) -> Option<u64> {
    let contents = get_pane_scrollback(PaneId::Terminal(pane_id), false).ok()?;
    Some(hash_viewport(&contents.viewport))
}

/// Sample every armed pane, evaluate its alarm, and fire notifications for any
/// that tripped. Returns `true` if the in-memory store changed (so the caller
/// persists it). Run only by the active instance — it holds the full manifest
/// for every tab and is the single writer of the store.
pub fn monitor_alarms(state: &mut AppState, config: &Config) -> bool {
    if state.alarms.entries.is_empty() {
        return false;
    }
    let now = now_epoch();
    let idle_timeout = config.alarms.idle_timeout.as_secs();
    let pane_ids: Vec<u32> = state.alarms.entries.keys().copied().collect();
    let mut changed = false;

    for pane_id in pane_ids {
        let Some(hash) = pane_content_hash(pane_id) else {
            continue;
        };
        let outcome = match state.alarms.entries.get(&pane_id) {
            Some(entry) => alarms::evaluate(entry, now, hash, idle_timeout),
            None => continue,
        };
        match outcome {
            AlarmOutcome::None => {}
            AlarmOutcome::UpdateBaseline => {
                if let Some(entry) = state.alarms.entries.get_mut(&pane_id) {
                    entry.content_hash = hash;
                    entry.last_change_epoch = now;
                    changed = true;
                }
            }
            AlarmOutcome::Fire => {
                fire_alarm_notification(state, config, pane_id);
                if let Some(entry) = state.alarms.entries.get_mut(&pane_id) {
                    entry.fired = true;
                    entry.content_hash = hash;
                    entry.last_change_epoch = now;
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Resolve a pane to a `(tab label, pane title)` pair for the notification text.
fn locate_pane(state: &AppState, pane_id: u32) -> (String, String) {
    for (tab_position, panes) in &state.panes {
        if let Some(pane) = panes.iter().find(|p| p.id == pane_id && !p.is_plugin) {
            let tab_label = state
                .tabs
                .iter()
                .find(|t| t.position == *tab_position)
                .map(|t| t.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("Tab {}", tab_position + 1));
            return (tab_label, pane.title.clone());
        }
    }
    (format!("pane {pane_id}"), String::new())
}

fn fire_alarm_notification(state: &AppState, config: &Config, pane_id: u32) {
    let kind = state.alarms.entries.get(&pane_id).map(|entry| entry.kind);
    let reason = match kind {
        Some(AlarmKind::Idle) => "output stopped",
        Some(AlarmKind::Activity) => "new output",
        None => "alarm",
    };
    let (tab_label, pane_title) = locate_pane(state, pane_id);
    let subject = if pane_title.trim().is_empty() {
        tab_label.clone()
    } else {
        pane_title
    };
    let title = format!("zj-hud: {tab_label}");
    let message = format!("{subject} - {reason}");
    let group = format!("zj-hud-{pane_id}");

    let argv: Vec<String> = config
        .alarms
        .notify_command
        .iter()
        .map(|token| {
            token
                .replace("{title}", &title)
                .replace("{message}", &message)
                .replace("{group}", &group)
        })
        .collect();
    if argv.is_empty() {
        return;
    }
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_command(&refs, BTreeMap::new());
}

// ─── User-defined widgets ─────────────────────────────────────────────────────

/// Fire pending command refreshes for every configured widget. This is
/// idempotent — widgets that are still in-flight or whose interval hasn't
/// elapsed are skipped.
pub fn maybe_refresh_widgets(state: &mut AppState, config: &Config) {
    let Some(block) = config.status.system_info.as_ref() else {
        return;
    };
    for widget in &block.widgets {
        maybe_refresh_widget(state, widget);
    }
}

fn maybe_refresh_widget(state: &mut AppState, widget: &InfoWidget) {
    let entry = state.widgets.entry(widget.id.clone()).or_default();

    if !entry.should_refresh(widget.interval) {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_WIDGET.to_string());
    ctx.insert(CTX_WIDGET_ID.to_string(), widget.id.clone());

    // Shell out so users can pipe and redirect freely.
    run_command(&["sh", "-c", widget.command.as_str()], ctx);

    entry.in_flight = true;
}

pub fn handle_command_result(
    exit_code: Option<i32>,
    stdout: &[u8],
    _stderr: &[u8],
    context: &BTreeMap<String, String>,
    state: &mut AppState,
) {
    let output = String::from_utf8_lossy(stdout);

    match context.get(CTX_KEY).map(|s| s.as_str()) {
        Some(CTX_GHOSTTY_FULLSCREEN) => {
            state.ghostty_fullscreen.in_flight = false;
            if exit_code == Some(0) {
                if let Some(is_fullscreen) = parse_ghostty_fullscreen_output(&output) {
                    state.ghostty_fullscreen.set(is_fullscreen);
                } else {
                    state.ghostty_fullscreen.mark_refreshed();
                }
            } else {
                state.ghostty_fullscreen.mark_refreshed();
            }
        }
        Some(CTX_TZ_OFFSET) => {
            state.tz_offset.in_flight = false;
            if exit_code == Some(0) {
                if let Some(offset) = parse_tz_offset(&output) {
                    state.tz_offset.set(offset);
                } else {
                    state.tz_offset.mark_refreshed();
                }
            } else {
                state.tz_offset.mark_refreshed();
            }
        }
        Some(CTX_WEZTERM_FULLSCREEN) => {
            state.wezterm_fullscreen.in_flight = false;
            if exit_code == Some(0) {
                if let Some(is_fullscreen) = parse_wezterm_fullscreen_output(&output) {
                    state.wezterm_fullscreen.set(is_fullscreen);
                } else {
                    state.wezterm_fullscreen.mark_refreshed();
                }
            } else {
                state.wezterm_fullscreen.mark_refreshed();
            }
        }
        Some(CTX_WIDGET) => {
            let Some(id) = context.get(CTX_WIDGET_ID) else {
                return;
            };
            let entry = state.widgets.entry(id.clone()).or_default();
            entry.in_flight = false;
            entry.last_updated = Some(Instant::now());
            if exit_code == Some(0) {
                let trimmed = output.lines().next().unwrap_or("").trim().to_string();
                entry.sample = Some(if trimmed.is_empty() {
                    WidgetSample::Empty
                } else {
                    WidgetSample::Value(trimmed)
                });
            } else {
                entry.sample = Some(WidgetSample::Error);
            }
        }
        _ => {}
    }
}

fn parse_ghostty_fullscreen_output(output: &str) -> Option<bool> {
    match output.trim() {
        "NATIVE_FULLSCREEN" | "NON_NATIVE_FULLSCREEN" => Some(true),
        "WINDOWED" | "NO_WINDOWS" | "NOT_RUNNING" => Some(false),
        _ => None,
    }
}

fn parse_wezterm_fullscreen_output(output: &str) -> Option<bool> {
    match output.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_term_program(value: &str, f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("TERM_PROGRAM");
        std::env::set_var("TERM_PROGRAM", value);
        f();
        match previous {
            Some(previous) => std::env::set_var("TERM_PROGRAM", previous),
            None => std::env::remove_var("TERM_PROGRAM"),
        }
    }

    #[test]
    fn parse_ghostty_fullscreen_states() {
        assert_eq!(
            parse_ghostty_fullscreen_output("NATIVE_FULLSCREEN\n"),
            Some(true)
        );
        assert_eq!(
            parse_ghostty_fullscreen_output("NON_NATIVE_FULLSCREEN\n"),
            Some(true)
        );
        assert_eq!(parse_ghostty_fullscreen_output("WINDOWED\n"), Some(false));
        assert_eq!(parse_ghostty_fullscreen_output("NO_WINDOWS\n"), Some(false));
        assert_eq!(
            parse_ghostty_fullscreen_output("NOT_RUNNING\n"),
            Some(false)
        );
        assert_eq!(parse_ghostty_fullscreen_output("bad"), None);
    }

    #[test]
    fn known_ghostty_windowed_state_overrides_column_fallback() {
        with_term_program("ghostty", || {
            let mut state = AppState {
                got_permissions: true,
                ..Default::default()
            };
            state.ghostty_fullscreen.set(false);

            assert!(!should_show_system_segments(&state, 100, 120));
        });
    }

    #[test]
    fn unknown_ghostty_state_stays_hidden() {
        with_term_program("ghostty", || {
            let state = AppState {
                got_permissions: true,
                ..Default::default()
            };

            assert!(!should_show_system_segments(&state, 100, 120));
        });
    }

    #[test]
    fn parse_wezterm_fullscreen_states() {
        assert_eq!(parse_wezterm_fullscreen_output("true\n"), Some(true));
        assert_eq!(parse_wezterm_fullscreen_output("false"), Some(false));
        assert_eq!(parse_wezterm_fullscreen_output("TRUE"), None);
        assert_eq!(parse_wezterm_fullscreen_output("bad"), None);
    }

    #[test]
    fn known_wezterm_state_overrides_column_fallback() {
        with_term_program("WezTerm", || {
            let mut state = AppState {
                got_permissions: true,
                ..Default::default()
            };
            state.wezterm_fullscreen.set(false);
            assert!(!should_show_system_segments(&state, 100, 120));

            state.wezterm_fullscreen.set(true);
            assert!(should_show_system_segments(&state, 100, 80));
        });
    }

    #[test]
    fn configured_terminal_state_overrides_column_fallback() {
        let mut state = AppState {
            got_permissions: true,
            ..Default::default()
        };

        state.terminal_fullscreen.set(false);
        assert!(!should_show_system_segments(&state, 100, 120));

        state.terminal_fullscreen.set(true);
        assert!(should_show_system_segments(&state, 100, 80));
    }

    #[test]
    fn wezterm_fullscreen_result_updates_cache() {
        let mut state = AppState::default();
        state.wezterm_fullscreen.in_flight = true;
        let mut context = BTreeMap::new();
        context.insert(CTX_KEY.to_string(), CTX_WEZTERM_FULLSCREEN.to_string());

        handle_command_result(Some(0), b"true\n", &[], &context, &mut state);

        assert_eq!(state.wezterm_fullscreen.value, Some(true));
        assert!(!state.wezterm_fullscreen.in_flight);
    }

    // ── parse_tz_offset ────────────────────────────────────────────────────

    #[test]
    fn parse_tz_offset_handles_common_shapes() {
        assert_eq!(parse_tz_offset("-0700\n"), Some(-7 * 3600));
        assert_eq!(parse_tz_offset("+0000"), Some(0));
        assert_eq!(parse_tz_offset("+0530\n"), Some(5 * 3600 + 30 * 60));
        assert_eq!(parse_tz_offset("-0930"), Some(-(9 * 3600 + 30 * 60)));
        assert_eq!(parse_tz_offset("  +0100  "), Some(3600));
    }

    #[test]
    fn parse_tz_offset_rejects_garbage() {
        assert_eq!(parse_tz_offset(""), None);
        assert_eq!(parse_tz_offset("-07"), None);
        assert_eq!(parse_tz_offset("PDT"), None);
        assert_eq!(parse_tz_offset("-07:00"), None);
        assert_eq!(parse_tz_offset("+07ab"), None);
        // Minutes must be a real subhour quantity.
        assert_eq!(parse_tz_offset("+0099"), None);
    }
}
