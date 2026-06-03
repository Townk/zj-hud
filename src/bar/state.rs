use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zellij_tile::prelude::*;

use crate::bar::click_map::ClickMap;

#[derive(Debug, Clone)]
pub struct CachedValue<T> {
    pub value: Option<T>,
    pub last_updated: Option<Instant>,
    pub ttl: Duration,
    pub in_flight: bool,
}

impl<T> CachedValue<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            value: None,
            last_updated: None,
            ttl,
            in_flight: false,
        }
    }

    pub fn is_expired(&self) -> bool {
        if self.in_flight {
            return false;
        }
        match self.last_updated {
            None => true,
            Some(t) => t.elapsed() >= self.ttl,
        }
    }

    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.last_updated = Some(Instant::now());
        self.in_flight = false;
    }

    pub fn mark_refreshed(&mut self) {
        self.last_updated = Some(Instant::now());
        self.in_flight = false;
    }
}

#[derive(Debug, Clone)]
pub struct ScrollState {
    pub left: usize,
    pub right: usize,
    pub has_left: bool,
    pub has_right: bool,
}

#[derive(Default)]
pub struct PaneCache {
    pub cwd: HashMap<u32, PathBuf>,
    pub cmd: HashMap<u32, Vec<String>>,
}

#[derive(Default)]
pub struct ProjectRootCache {
    pub roots: HashMap<PathBuf, Option<PathBuf>>,
    pub in_flight: HashSet<PathBuf>,
}

/// Most recent sample for a user-defined `info` widget.
#[derive(Debug, Clone)]
pub enum WidgetSample {
    /// Successfully captured, trimmed-first-line stdout.
    Value(String),
    /// Command exited zero with empty stdout. Distinct from `Error` so the
    /// renderer can skip the widget entirely (instead of substituting "ERR")
    /// — useful for "show only when condition X holds" widgets.
    Empty,
    /// Command exited non-zero, or `type=number` widget produced
    /// non-numeric output.
    Error,
}

#[derive(Debug, Default)]
pub struct WidgetState {
    pub sample: Option<WidgetSample>,
    pub last_updated: Option<Instant>,
    pub in_flight: bool,
}

impl WidgetState {
    /// Should we re-fire the command for this widget?
    ///
    /// `interval == ZERO` means "fire once and cache forever": we only run
    /// it while we have no observation yet (sample is `None` AND we are
    /// not currently waiting on a result).
    pub fn should_refresh(&self, interval: Duration) -> bool {
        if self.in_flight {
            return false;
        }
        if interval.is_zero() {
            return self.last_updated.is_none();
        }
        match self.last_updated {
            None => true,
            Some(t) => t.elapsed() >= interval,
        }
    }
}

pub struct AppState {
    pub mode: InputMode,
    /// Mirror of the shared `search_active` flag, written by the floating
    /// search pane while its dialog is open and delivered to the bar through
    /// the `SharedState` broadcast. The dialog keeps the *client* in `Normal`
    /// (the only mode in which `intercept_key_presses` delivers keys to the
    /// plugin), so we cannot read "search is active" from the client mode and
    /// read this field instead. When set, the bar renders the Search indicator
    /// regardless of `mode`.
    pub search_active: bool,
    /// Mirror of the shared search-option toggles (written by the Search role),
    /// used to render the per-option glyphs in the Search-mode hint segment.
    /// `search_wrap` mirrors the dialog's wrap toggle (default on).
    pub search_case_sensitive: bool,
    pub search_whole_word: bool,
    pub search_wrap: bool,
    pub tabs: Vec<TabInfo>,
    pub panes: HashMap<usize, Vec<PaneInfo>>,
    /// Terminal pane IDs we actually care about for `PaneRenderReport` events
    /// — the focused terminal pane in each tab. Rebuilt on `TabUpdate` /
    /// `PaneUpdate`. Render reports for any other pane (e.g. an unfocused pane
    /// in another tab that's spitting output) are dropped without an IPC
    /// round-trip, bounding our worst-case `get_pane_info` calls at one per
    /// open tab.
    pub interesting_panes: HashSet<u32>,
    pub session_name: String,
    pub ghostty_fullscreen: CachedValue<bool>,
    /// Local-time UTC offset in seconds, sampled by shelling out to
    /// `date +%z` (see `system::maybe_refresh_tz_offset`). Zellij plugins
    /// run inside a WASI sandbox with no access to the host timezone
    /// database, so `chrono::Local::now()` silently falls back to UTC.
    /// We instead format `chrono::Utc::now()` with this cached offset.
    /// The 30-minute TTL is short enough to catch a DST transition within
    /// half an hour of it happening.
    pub tz_offset: CachedValue<i32>,
    pub cols: usize,
    pub dirty: bool,
    pub got_permissions: bool,
    pub pending_events: Vec<Event>,
    pub pane_cache: PaneCache,
    pub project_roots: ProjectRootCache,
    pub home: PathBuf,
    /// Latest click regions produced by `render`. Mouse handlers look up
    /// the clicked column here to decide which action to take.
    pub click_map: ClickMap,
    /// User-defined widget samples, keyed by widget id.
    pub widgets: HashMap<String, WidgetState>,
    /// Per-`on_click` debounce: timestamp of the last fire for each
    /// `ClickAction::RunCommand(idx)`. Clicks within 100ms are dropped.
    pub last_click_run: HashMap<usize, Instant>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            mode: InputMode::Normal,
            search_active: false,
            search_case_sensitive: false,
            search_whole_word: false,
            search_wrap: true,
            tabs: Vec::new(),
            panes: HashMap::new(),
            interesting_panes: HashSet::new(),
            session_name: String::new(),
            ghostty_fullscreen: CachedValue::new(Duration::from_secs(2)),
            tz_offset: CachedValue::new(Duration::from_secs(30 * 60)),
            cols: 0,
            dirty: true,
            got_permissions: false,
            pending_events: Vec::new(),
            pane_cache: PaneCache::default(),
            project_roots: ProjectRootCache::default(),
            home: std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/")),
            click_map: ClickMap::default(),
            widgets: HashMap::new(),
            last_click_run: HashMap::new(),
        }
    }
}

impl AppState {
    pub fn active_tab_index(&self) -> Option<usize> {
        self.tabs.iter().position(|t| t.active)
    }

    pub fn panes_for_tab(&self, tab_position: usize) -> &[PaneInfo] {
        self.panes
            .get(&tab_position)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn focused_pane_for_tab(&self, tab_position: usize) -> Option<&PaneInfo> {
        self.panes_for_tab(tab_position)
            .iter()
            .find(|p| p.is_focused && !p.is_plugin)
    }

    pub fn any_pane_zoomed(&self, tab_position: usize) -> bool {
        self.panes_for_tab(tab_position)
            .iter()
            .any(|p| p.is_fullscreen && !p.is_plugin)
    }

    /// Whether input sent to this tab is synced to all of its panes.
    pub fn tab_sync_active(&self, tab_position: usize) -> bool {
        self.tabs
            .iter()
            .find(|t| t.position == tab_position)
            .map(|t| t.is_sync_panes_active)
            .unwrap_or(false)
    }

    /// Recompute `interesting_panes` to contain the focused terminal pane of
    /// each tab. Called whenever pane/tab membership might have changed so the
    /// `PaneRenderReport` filter stays in sync with the current layout.
    pub fn rebuild_interesting_panes(&mut self) {
        self.interesting_panes.clear();
        for panes in self.panes.values() {
            if let Some(focused) = panes.iter().find(|p| p.is_focused && !p.is_plugin) {
                self.interesting_panes.insert(focused.id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_value_starts_expired() {
        let cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        assert!(cv.is_expired());
        assert!(cv.value.is_none());
    }

    #[test]
    fn cached_value_not_expired_after_set() {
        let mut cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        cv.set(true);
        assert!(!cv.is_expired());
        assert_eq!(cv.value, Some(true));
    }

    #[test]
    fn cached_value_in_flight_prevents_expiry() {
        let mut cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        cv.in_flight = true;
        assert!(!cv.is_expired());
    }

    #[test]
    fn cached_value_mark_refreshed_preserves_value() {
        let mut cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        cv.set(true);
        cv.mark_refreshed();
        assert_eq!(cv.value, Some(true));
        assert!(!cv.is_expired());
    }

    #[test]
    fn default_app_state() {
        let state = AppState::default();
        assert_eq!(state.mode, InputMode::Normal);
        assert!(state.tabs.is_empty());
        assert!(state.dirty);
        assert!(!state.got_permissions);
        assert!(state.interesting_panes.is_empty());
    }

    #[test]
    fn rebuild_interesting_panes_picks_focused_terminal_per_tab() {
        let mut state = AppState::default();
        state.panes.insert(
            0,
            vec![
                PaneInfo {
                    id: 1,
                    is_focused: false,
                    is_plugin: false,
                    ..Default::default()
                },
                PaneInfo {
                    id: 2,
                    is_focused: true,
                    is_plugin: false,
                    ..Default::default()
                },
            ],
        );
        // Tab 1: focused pane is a plugin (e.g., our own status bar) — should
        // be ignored. Only the unfocused terminal pane sits in that tab, so no
        // entry is added for tab 1.
        state.panes.insert(
            1,
            vec![
                PaneInfo {
                    id: 99,
                    is_focused: true,
                    is_plugin: true,
                    ..Default::default()
                },
                PaneInfo {
                    id: 3,
                    is_focused: false,
                    is_plugin: false,
                    ..Default::default()
                },
            ],
        );

        state.rebuild_interesting_panes();

        assert!(state.interesting_panes.contains(&2));
        assert!(!state.interesting_panes.contains(&1));
        assert!(!state.interesting_panes.contains(&3));
        assert!(!state.interesting_panes.contains(&99));
        assert_eq!(state.interesting_panes.len(), 1);
    }

    #[test]
    fn rebuild_interesting_panes_clears_stale_entries() {
        let mut state = AppState::default();
        state.interesting_panes.insert(42);
        state.panes.insert(
            0,
            vec![PaneInfo {
                id: 7,
                is_focused: true,
                is_plugin: false,
                ..Default::default()
            }],
        );

        state.rebuild_interesting_panes();

        assert!(state.interesting_panes.contains(&7));
        assert!(!state.interesting_panes.contains(&42));
    }

    // ── WidgetState ────────────────────────────────────────────────────────

    #[test]
    fn widget_state_zero_interval_fires_only_once() {
        let mut w = WidgetState::default();
        // First call: no observation yet → should refresh.
        assert!(w.should_refresh(Duration::ZERO));

        // Simulate fire: in_flight flag.
        w = WidgetState {
            in_flight: true,
            ..WidgetState::default()
        };
        assert!(!w.should_refresh(Duration::ZERO));

        // Result lands.
        w = WidgetState {
            in_flight: false,
            last_updated: Some(Instant::now()),
            sample: Some(WidgetSample::Value("x".into())),
        };

        // With interval=0, never refresh again.
        assert!(!w.should_refresh(Duration::ZERO));
    }

    #[test]
    fn widget_state_in_flight_blocks_refresh() {
        let w = WidgetState {
            in_flight: true,
            ..WidgetState::default()
        };
        assert!(!w.should_refresh(Duration::from_secs(0)));
        assert!(!w.should_refresh(Duration::from_secs(60)));
    }

    #[test]
    fn widget_state_interval_respects_elapsed() {
        let w = WidgetState {
            last_updated: Some(Instant::now()),
            ..WidgetState::default()
        };
        // Just-now observation, long interval → not yet.
        assert!(!w.should_refresh(Duration::from_secs(60)));
        // Trivially-tiny interval → already due.
        assert!(w.should_refresh(Duration::from_nanos(1)));
    }
}
