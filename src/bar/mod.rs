//! The status-bar role: the default plugin instance. Renders the bar itself —
//! tabs on the left ([`tabs`]) and status segments on the right ([`status`]) —
//! and owns the session-scoped mode/backstack/palette in `SharedState`,
//! broadcasting it to the other roles via `shared::state`.

pub mod click_map;
pub mod config;
pub mod render;
pub mod state;
pub mod status;
pub mod tabs;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use zellij_tile::prelude::*;

use crate::shared::alarms::{self, AlarmEntry, AlarmKind};
use crate::shared::state as shared_state;
use crate::shared::state::SharedState;
use crate::PLUGIN_PERMISSIONS;
use click_map::ClickAction;
use config::Config;
use state::AppState;
use status::system;

/// How often the background timer fires. Kept fast (1 s) because Zellij does
/// not emit `PaneUpdate` when only a terminal's OSC window title changes, so
/// `system::refresh_all_tab_titles` has to poll. Other timer-driven work
/// (widget refresh, Ghostty fullscreen probe) gates itself behind its own TTL,
/// so faster ticks don't increase the rate of those underlying commands.
const TIMER_INTERVAL: f64 = 1.0;

/// Delay non-Normal mode publication long enough for tab-switch transients to
/// settle. `Normal` updates bypass and cancel the delay.
const MODE_INDICATOR_DEBOUNCE: Duration = Duration::from_millis(32);

/// Minimum delay between two `RunCommand` invocations of the same `on_click`
/// shell command. Prevents accidental double-fires from a single click.
const CLICK_RUN_DEBOUNCE: Duration = Duration::from_millis(100);

// Context keys for RunCommandResult routing.
const CTX_PROJECT_ROOT: &str = "project_root";
const CTX_PROJECT_CWD: &str = "project_cwd";

/// Pipe a user keybind targets (via the `MessagePlugin` action) to arm or
/// clear a background-tab alarm on the active tab's focused terminal pane. The
/// payload selects the action: `idle`, `activity`, or `clear`.
pub const ALARM_PIPE: &str = "zj_hud_alarm";

#[derive(Default)]
pub(crate) struct State {
    app: AppState,
    config: Config,
    hud_peers: Vec<u32>,
    shared_generation: u64,
    zellij_visible: bool,
    saw_visible_event: bool,
    active_tab: usize,
    active_tab_id: Option<usize>,
    my_tab: Option<usize>,
    my_tab_id: Option<usize>,
    pending_mode: Option<InputMode>,
    pending_mode_started: Option<Instant>,
    /// Non-`Normal` modes the user chained through faster than the debounce
    /// window, awaiting commit. Kept local (not published) so a transient mode
    /// never reaches the live indicator; folded into the shared `backstack` in
    /// order only when the chain settles (see [`Self::commit_pending_trail`]).
    /// A chain that resolves to `Normal` clears this, so tab-switch transients
    /// (`Alt+w t N`) never flash the bar.
    pending_trail: Vec<InputMode>,
    /// Latest `base_mode` reported by `ModeUpdate`; used to maintain the
    /// shared `backstack` mode-trail (consumed by the WhichKey role).
    base_mode: Option<InputMode>,
    /// Most recent full `SharedState` this instance has seen or written. Lets
    /// the Bar preserve fields it does not own (`search_active`, `suppressed`,
    /// `page`) when it republishes a mode change.
    last_shared: SharedState,
}

// ─── Plugin ───────────────────────────────────────────────────────────────────

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.config = Config::from_map(configuration);
        // Identical set for both roles — see `PLUGIN_PERMISSIONS`. (The bar
        // itself only uses Read*/RunCommands/ChangeApplicationState; the extra
        // search-role permissions are requested here solely to keep the
        // per-URL permission cache stable across roles.)
        request_permission(PLUGIN_PERMISSIONS);
        subscribe(&[
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::SessionUpdate,
            EventType::PaneUpdate,
            EventType::Visible,
            EventType::PaneRenderReport,
            EventType::CwdChanged,
            EventType::CommandChanged,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
            EventType::Mouse,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        if !self.app.got_permissions {
            if let Event::PermissionRequestResult(PermissionStatus::Granted) = event {
                self.app.got_permissions = true;
                set_selectable(false);
                set_timeout(1.0);
                self.load_alarms();
                system::maybe_refresh_ghostty_fullscreen(&mut self.app);
                system::maybe_refresh_tz_offset(&mut self.app);
                system::maybe_refresh_widgets(&mut self.app, &self.config);

                if let Ok((_tab_pos, pane_id)) = get_focused_pane_info() {
                    if let PaneId::Terminal(id) = pane_id {
                        if let Ok(cwd) = get_pane_cwd(pane_id) {
                            self.app.pane_cache.cwd.insert(id, cwd.clone());
                            self.start_project_root_lookup(cwd);
                        }
                        if let Ok(cmd) = get_pane_running_command(pane_id) {
                            self.app.pane_cache.cmd.insert(id, cmd);
                        }
                    }
                }

                let pending: Vec<Event> = self.app.pending_events.drain(..).collect();
                for ev in pending {
                    self.process_event(ev);
                }
                self.app.dirty = true;
            } else {
                self.app.pending_events.push(event);
            }
            return self.app.dirty;
        }
        self.process_event(event);
        self.app.dirty
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        let cols_changed = self.app.cols != cols;
        self.app.cols = cols;
        if self.app.got_permissions && cols_changed {
            system::refresh_ghostty_fullscreen_now(&mut self.app);
        }
        let bar = render::render_bar(&self.app, &self.config, cols);
        print!("{}", bar.text);
        self.app.click_map = bar.click_map;
        self.app.dirty = false;
    }
}

// ─── Event processing ─────────────────────────────────────────────────────────

impl State {
    pub(crate) fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name == ALARM_PIPE {
            return self.handle_alarm_pipe(pipe_message.payload.as_deref());
        }
        if pipe_message.name != shared_state::SYNC_PIPE {
            return false;
        }

        let Some(payload) = pipe_message.payload.as_deref() else {
            return false;
        };
        let Ok(shared) = serde_json::from_str::<SharedState>(payload) else {
            return false;
        };

        let incoming_generation = shared.generation;
        let local_generation = self.shared_generation;
        let changed = self.apply_shared_state(&shared);
        if incoming_generation >= local_generation {
            self.cache_shared_state(&shared);
        }
        changed
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::ModeUpdate(mode_info) => {
                let mode = mode_info.mode;

                // Update session name first so the file path is correct.
                if let Some(name) = mode_info.session_name {
                    self.app.session_name = name;
                }
                if let Some(base) = mode_info.base_mode {
                    self.base_mode = Some(base);
                }

                self.handle_mode_update(mode);
                self.app.dirty = true;
            }
            Event::TabUpdate(tabs) => {
                self.app.tabs = tabs;
                if let Some(active) = self.app.tabs.iter().find(|tab| tab.active) {
                    self.active_tab = active.position;
                    self.active_tab_id = Some(active.tab_id);
                }
                self.sync_from_shared_state();
                if self.is_active_instance() {
                    self.clear_fired_in_active_tab();
                }
                self.app.dirty = true;
            }
            Event::SessionUpdate(sessions, _) => {
                if let Some(session) = sessions.iter().find(|s| s.is_current_session) {
                    if session.name != self.app.session_name {
                        self.app.session_name = session.name.clone();
                        self.sync_from_shared_state();
                        self.app.dirty = true;
                    }
                }
            }
            Event::PaneUpdate(manifest) => {
                self.detect_my_tab(&manifest);
                let peers_changed = self.detect_hud_peers(&manifest);
                if peers_changed && self.shared_generation > 0 {
                    self.broadcast_shared_state(&self.snapshot_shared_state());
                }

                // CwdChanged / CommandChanged only fire on changes; proactively
                // query new panes so inactive tabs can still render cached state.
                for panes in manifest.panes.values() {
                    for pane in panes {
                        if pane.is_plugin {
                            continue;
                        }
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            self.app.pane_cache.cwd.entry(pane.id)
                        {
                            if let Ok(cwd) = get_pane_cwd(PaneId::Terminal(pane.id)) {
                                entry.insert(cwd.clone());
                                self.start_project_root_lookup(cwd);
                            }
                        }
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            self.app.pane_cache.cmd.entry(pane.id)
                        {
                            if let Ok(cmd) = get_pane_running_command(PaneId::Terminal(pane.id)) {
                                entry.insert(cmd);
                            }
                        }
                    }
                }
                self.app.panes = manifest.panes;
                self.app.rebuild_interesting_panes();
                if self.is_active_instance() {
                    self.prune_alarms();
                }
                self.sync_from_shared_state();
                self.app.dirty = true;
            }
            Event::Visible(is_visible) => {
                self.saw_visible_event = true;
                self.zellij_visible = is_visible;
                if is_visible {
                    self.sync_from_shared_state();
                    // This instance just became active: adopt the latest alarm
                    // store (left by the previously-active instance) and clear
                    // any fired alarm on the tab the user just switched into.
                    self.load_alarms();
                    self.clear_fired_in_active_tab();
                } else {
                    self.clear_local_mode_indicator();
                }
                self.app.dirty = true;
            }
            Event::PaneRenderReport(panes) => {
                // Zellij delivers the *changed* viewports of all subscribed
                // panes here; we only act on the focused terminal pane of each
                // tab (tracked in `interesting_panes`). The viewport payload
                // itself is ignored — we just use the event as a trigger to
                // re-read each pane's OSC title via `get_pane_info`. Worst case
                // this does one IPC per open tab per render tick.
                for pane_id in panes.keys() {
                    if let PaneId::Terminal(id) = pane_id {
                        if self.app.interesting_panes.contains(id) {
                            system::refresh_pane_title_by_id(&mut self.app, *id);
                        }
                    }
                }
            }
            Event::CwdChanged(pane_id, path, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    self.app.pane_cache.cwd.insert(id, path.clone());
                    self.start_project_root_lookup(path);
                }
                self.app.dirty = true;
            }
            Event::CommandChanged(pane_id, cmd, _is_foreground, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    self.app.pane_cache.cmd.insert(id, cmd);
                }
                self.app.dirty = true;
            }
            Event::Timer(_secs) => {
                if !self.flush_pending_mode_if_ready() {
                    return;
                }
                system::maybe_refresh_ghostty_fullscreen(&mut self.app);
                system::maybe_refresh_tz_offset(&mut self.app);
                system::maybe_refresh_widgets(&mut self.app, &self.config);
                system::refresh_all_tab_titles(&mut self.app);
                // Only the active instance monitors alarms: it holds every
                // tab's panes and is the store's sole writer, so a background
                // instance can never double-fire a notification.
                if self.is_active_instance() && system::monitor_alarms(&mut self.app, &self.config)
                {
                    self.persist_alarms();
                }
                set_timeout(TIMER_INTERVAL);
            }
            Event::Mouse(mouse_event) => {
                self.handle_mouse_event(mouse_event);
            }
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                match context.get(system::CTX_KEY).map(|s| s.as_str()) {
                    Some(CTX_PROJECT_ROOT) => {
                        if let Some(cwd) = context.get(CTX_PROJECT_CWD).map(PathBuf::from) {
                            self.app.project_roots.in_flight.remove(&cwd);
                            if exit_code == Some(0) {
                                let root = String::from_utf8_lossy(&stdout).trim().to_string();
                                if root.is_empty() {
                                    self.app.project_roots.roots.insert(cwd, None);
                                } else {
                                    self.app
                                        .project_roots
                                        .roots
                                        .insert(cwd, Some(PathBuf::from(root)));
                                }
                            } else {
                                self.app.project_roots.roots.insert(cwd, None);
                            }
                            self.app.dirty = true;
                        }
                    }
                    _ => {
                        // Battery / wifi results.
                        system::handle_command_result(
                            exit_code,
                            &stdout,
                            &stderr,
                            &context,
                            &mut self.app,
                        );
                        self.app.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Route a mouse event to the appropriate action.
    ///
    /// Left-click looks up the column in the click map produced by the
    /// most recent render. Scroll wheel cycles through tabs regardless of
    /// the column (Zellij does not report a position for scroll events).
    /// `Mouse::Hover` is intentionally not handled: Zellij 0.44 does not
    /// deliver hover events to non-selectable plugin panes, so subscribing
    /// to them would only add dead code paths.
    fn handle_mouse_event(&mut self, event: Mouse) {
        match event {
            Mouse::LeftClick(_line, col) => {
                if let Some(action) = self.app.click_map.lookup(col) {
                    self.apply_click_action(action);
                }
            }
            Mouse::ScrollUp(_) => self.cycle_tab(1),
            Mouse::ScrollDown(_) => self.cycle_tab(-1),
            _ => {}
        }
    }

    fn apply_click_action(&mut self, action: ClickAction) {
        match action {
            ClickAction::SwitchToTab(idx) => {
                if idx > 0 && (idx as usize) <= self.app.tabs.len() {
                    switch_tab_to(idx);
                }
            }
            ClickAction::SwitchToMode(mode) => {
                switch_to_input_mode(&mode);
            }
            ClickAction::RunCommand(idx) => {
                self.fire_click_command(idx);
            }
        }
    }

    /// Fire a configured `on_click` shell command via `sh -c`. Debounced per
    /// command index — repeated clicks within `CLICK_RUN_DEBOUNCE` are
    /// dropped to avoid accidental double-fires.
    fn fire_click_command(&mut self, idx: usize) {
        let Some(cmd) = self.config.click_commands.get(idx).cloned() else {
            return;
        };
        let now = Instant::now();
        if let Some(last) = self.app.last_click_run.get(&idx) {
            if now.duration_since(*last) < CLICK_RUN_DEBOUNCE {
                return;
            }
        }
        self.app.last_click_run.insert(idx, now);

        // Fire-and-forget. We don't need the result, so we route it through
        // a "no-op" context that `RunCommandResult` matchers ignore.
        run_command(&["sh", "-c", cmd.as_str()], BTreeMap::new());
    }

    /// Move tab focus by `delta` positions, clamped to the existing tabs.
    fn cycle_tab(&mut self, delta: isize) {
        let tab_count = self.app.tabs.len();
        if tab_count == 0 {
            return;
        }
        let current = self.app.active_tab_index().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, (tab_count - 1) as isize);
        if next == current {
            return;
        }
        switch_tab_to((next as u32) + 1);
    }

    fn handle_mode_update(&mut self, mode: InputMode) {
        // While the floating search dialog is up it parks the client in `Normal`
        // to grab keys (and `SearchInput` re-asserts `Normal`); none of that is a
        // real mode change. Freeze the mode-trail (publish nothing) so the
        // which-key breadcrumb survives the excursion and is correct once the
        // user submits into `Search`. The Search role writes `search_active` to
        // the state file *before* flipping the mode, so this fresh disk read is
        // race-free — the cached/pipe-synced copy can lag behind the ModeUpdate.
        // The Search indicator still lights via the synced `search_active` flag.
        if self.search_dialog_active() {
            // A genuine mode the user passed through on the way into search
            // (e.g. `Scroll` from `Alt+w l /`) may still be sitting in
            // `pending_mode`, debounced and not yet published. Commit it to the
            // trail before freezing so it survives into the breadcrumb once the
            // user submits into `Search`; drop the search-entry modes
            // themselves (`EnterSearch`/`Search`), which collapse into the
            // dialog excursion rather than belonging in the trail.
            self.flush_pending_mode_skipping_search();
            return;
        }
        if mode == InputMode::Normal {
            self.pending_mode = None;
            self.pending_mode_started = None;
            self.pending_trail.clear();
            self.publish_or_sync_mode(mode);
            return;
        }

        // A *different* non-Normal mode supersedes the one still inside the
        // debounce window: the user chained modes faster than the window (e.g.
        // `l` then `/`). Stash the superseded mode in the local trail so the
        // which-key breadcrumb keeps it, but do NOT publish it — publishing
        // promotes it to the live mode and flashes the bar indicator for a mode
        // the user only passed through (the `Alt+w t N` tab-switch transient).
        // The trail is committed in one shot when the chain settles (see
        // [`Self::commit_pending_trail`]); a chain that resolves to `Normal`
        // clears it above, so the transient never reaches the screen.
        if matches!(self.pending_mode, Some(pending) if pending != mode) {
            if let Some(prev) = self.pending_mode.take() {
                self.pending_trail.push(prev);
            }
        }

        if self.can_publish_shared_state() {
            self.pending_mode = Some(mode);
            self.pending_mode_started = Some(Instant::now());
            set_timeout(MODE_INDICATOR_DEBOUNCE.as_secs_f64());
        } else {
            self.sync_from_shared_state();
        }
    }

    /// Publish the stashed transient trail (modes the user chained through
    /// during the debounce window) in order, building the shared `backstack`.
    /// Every publish happens inside the single `update()` call that settled the
    /// chain, so the bar renders only the final mode — the intermediates reach
    /// the breadcrumb's `backstack` without ever flashing the live indicator.
    fn commit_pending_trail(&mut self) {
        for mode in std::mem::take(&mut self.pending_trail) {
            self.publish_or_sync_mode(mode);
        }
    }

    /// Commit the stashed trail plus the pending mode for the search-dialog
    /// excursion, discarding search-entry modes (`EnterSearch`/`Search`): those
    /// collapse into the dialog rather than belonging in the breadcrumb. Lets a
    /// real pre-search mode (e.g. `Scroll`) survive into the trail while the
    /// search entry itself is dropped.
    fn flush_pending_mode_skipping_search(&mut self) {
        let pending = self.pending_mode.take();
        self.pending_mode_started = None;
        self.commit_pending_trail();
        if let Some(mode) = pending {
            if !matches!(mode, InputMode::EnterSearch | InputMode::Search) {
                self.publish_or_sync_mode(mode);
            }
        }
    }

    fn flush_pending_mode_if_ready(&mut self) -> bool {
        let Some(mode) = self.pending_mode else {
            return true;
        };
        let Some(started) = self.pending_mode_started else {
            self.pending_mode = None;
            return true;
        };

        let elapsed = started.elapsed();
        if elapsed < MODE_INDICATOR_DEBOUNCE {
            set_timeout((MODE_INDICATOR_DEBOUNCE - elapsed).as_secs_f64());
            return false;
        }

        self.pending_mode = None;
        self.pending_mode_started = None;
        self.commit_pending_trail();
        self.publish_or_sync_mode(mode);
        true
    }

    fn publish_or_sync_mode(&mut self, mode: InputMode) {
        if self.can_publish_shared_state() {
            // Fall back to the new mode itself as base when none was reported,
            // matching native behaviour for sessions with the default base.
            let base_mode = self.base_mode.unwrap_or(InputMode::Normal);
            self.with_active_shared_state(|shared, _| {
                shared.publish_mode_update(mode, base_mode, get_plugin_ids().plugin_id)
            });
        } else {
            self.sync_from_shared_state();
        }
    }

    fn clear_local_mode_indicator(&mut self) {
        self.pending_mode = None;
        self.pending_mode_started = None;
        self.pending_trail.clear();
        if self.app.mode != InputMode::Normal {
            self.app.mode = InputMode::Normal;
            self.app.dirty = true;
        }
    }

    fn shared_state_path(&self) -> String {
        shared_state::state_path(get_plugin_ids().zellij_pid, &self.app.session_name)
    }

    // ─── Alarms ─────────────────────────────────────────────────────────────

    fn alarm_state_path(&self) -> String {
        alarms::path(get_plugin_ids().zellij_pid, &self.app.session_name)
    }

    /// Load the alarm store from disk into memory. Called when this instance
    /// becomes the active one, so it picks up arms/baselines left by whichever
    /// instance was active before.
    fn load_alarms(&mut self) {
        self.app.alarms = alarms::read_from(self.alarm_state_path()).unwrap_or_default();
    }

    fn persist_alarms(&self) {
        let _ = alarms::write_to(self.alarm_state_path(), &self.app.alarms);
    }

    /// Arm/clear an alarm on the active tab's focused terminal pane. Only the
    /// active instance acts (its `active_tab` is the tab the user is looking
    /// at, and its in-memory store is authoritative).
    fn handle_alarm_pipe(&mut self, payload: Option<&str>) -> bool {
        if !self.is_active_instance() {
            return false;
        }
        let action = payload.map(str::trim).unwrap_or("");
        let Some(pane_id) = self
            .app
            .focused_pane_for_tab(self.active_tab)
            .map(|pane| pane.id)
        else {
            return false;
        };
        match action {
            "idle" | "activity" => {
                let kind = if action == "idle" {
                    AlarmKind::Idle
                } else {
                    AlarmKind::Activity
                };
                let now = system::now_epoch();
                let hash = system::pane_content_hash(pane_id).unwrap_or(0);
                self.app
                    .alarms
                    .entries
                    .insert(pane_id, AlarmEntry::armed(kind, now, hash));
            }
            "clear" => {
                self.app.alarms.entries.remove(&pane_id);
            }
            _ => return false,
        }
        self.persist_alarms();
        self.app.dirty = true;
        true
    }

    /// Drop alarm entries whose pane no longer exists (closed panes). Active
    /// instance only — it owns the store writes.
    fn prune_alarms(&mut self) {
        if self.app.alarms.entries.is_empty() {
            return;
        }
        let live: HashSet<u32> = self
            .app
            .panes
            .values()
            .flatten()
            .filter(|pane| !pane.is_plugin)
            .map(|pane| pane.id)
            .collect();
        let before = self.app.alarms.entries.len();
        self.app.alarms.entries.retain(|id, _| live.contains(id));
        if self.app.alarms.entries.len() != before {
            self.persist_alarms();
            self.app.dirty = true;
        }
    }

    /// Acknowledge fired alarms on the now-active tab: switching into the tab
    /// is the "visit" that clears its bell.
    fn clear_fired_in_active_tab(&mut self) {
        if self.app.alarms.entries.is_empty() {
            return;
        }
        let pane_ids: HashSet<u32> = self
            .app
            .panes_for_tab(self.active_tab)
            .iter()
            .filter(|pane| !pane.is_plugin)
            .map(|pane| pane.id)
            .collect();
        let before = self.app.alarms.entries.len();
        self.app
            .alarms
            .entries
            .retain(|id, entry| !(entry.fired && pane_ids.contains(id)));
        if self.app.alarms.entries.len() != before {
            self.persist_alarms();
            self.app.dirty = true;
        }
    }

    /// The most recent full state with this instance's identity stamped on it.
    /// Built from `last_shared` so fields the Bar doesn't own (`search_active`,
    /// `suppressed`, `page`) are preserved across re-broadcasts.
    fn snapshot_shared_state(&self) -> SharedState {
        let mut snapshot = self.last_shared.clone();
        snapshot.schema_version = shared_state::SCHEMA_VERSION;
        snapshot.generation = self.shared_generation;
        snapshot.writer = get_plugin_ids().plugin_id;
        // The active Bar owns the palette + which_key block: stamp them on every
        // publish so the WhichKey panel inherits the bar's configured
        // glyphs/colors/labels and its display options (authored once).
        snapshot.palette = self.config.mode_palette();
        snapshot.which_key_config = self.config.which_key_config.clone();
        snapshot.search_config = self.config.search_config.clone();
        snapshot
    }

    fn persist_shared_state(&self, state: &SharedState) {
        self.cache_shared_state(state);
        self.broadcast_shared_state(state);
    }

    fn cache_shared_state(&self, state: &SharedState) {
        let _ = shared_state::write_state_to(self.shared_state_path(), state);
    }

    fn broadcast_shared_state(&self, state: &SharedState) {
        let Ok(payload) = serde_json::to_string(state) else {
            return;
        };
        pipe_message_to_plugin(MessageToPlugin::new(shared_state::SYNC_PIPE).with_payload(payload));
    }

    fn sync_from_shared_state(&mut self) -> bool {
        let shared = shared_state::read_state_from(self.shared_state_path()).unwrap_or_default();
        self.apply_shared_state(&shared)
    }

    /// Freshest `search_active` straight from disk. Used to gate mode-trail
    /// updates: the Search role writes this flag before flipping the client
    /// mode, so reading it here (rather than the lagging pipe-synced copy)
    /// reliably tells us a `Normal`/`Search` ModeUpdate is a dialog excursion.
    fn search_dialog_active(&self) -> bool {
        shared_state::read_state_from(self.shared_state_path())
            .map(|shared| shared.search_active)
            .unwrap_or(false)
    }

    fn apply_shared_state(&mut self, shared: &SharedState) -> bool {
        if shared.generation < self.shared_generation {
            return false;
        }

        let mode = if self.is_active_instance() {
            shared.mode()
        } else {
            // Passive per-tab instances should track the latest generation but
            // not keep a stale non-Normal mode ready to flash on next reveal.
            InputMode::Normal
        };
        let mut changed = self.app.mode != mode;
        // The search indicator is driven by the shared field (written by the
        // Search role) rather than a dedicated pipe, so it reflects on whatever
        // bar instance is visible.
        if self.app.search_active != shared.search_active {
            self.app.search_active = shared.search_active;
            changed = true;
        }
        // Search-option toggles drive the per-glyph highlight in the Search-mode
        // hint segment; like `search_active` they ride the shared field.
        if self.app.search_case_sensitive != shared.search_case_sensitive
            || self.app.search_whole_word != shared.search_whole_word
            || self.app.search_wrap != shared.search_wrap
        {
            self.app.search_case_sensitive = shared.search_case_sensitive;
            self.app.search_whole_word = shared.search_whole_word;
            self.app.search_wrap = shared.search_wrap;
            changed = true;
        }
        self.shared_generation = shared.generation;
        self.last_shared = shared.clone();
        if changed {
            self.app.mode = mode;
            self.app.dirty = true;
        }
        changed
    }

    fn with_active_shared_state(
        &mut self,
        update: impl FnOnce(SharedState, &Self) -> SharedState,
    ) -> bool {
        if !self.can_publish_shared_state() {
            self.sync_from_shared_state();
            return false;
        }

        let from_disk = shared_state::read_state_from(self.shared_state_path()).unwrap_or_default();
        self.apply_shared_state(&from_disk);
        // Start from the freshest full state we hold so the closure only
        // rewrites the Bar-owned fields and preserves everyone else's.
        let before = self.last_shared.clone();
        let mut after = update(before.clone(), self);
        // Bar owns the palette + which_key block; keep them stamped (the first
        // publish populates them for the WhichKey panel to adopt).
        after.palette = self.config.mode_palette();
        after.which_key_config = self.config.which_key_config.clone();
        after.search_config = self.config.search_config.clone();
        let changed = after != before;
        if changed {
            self.persist_shared_state(&after);
        }
        self.apply_shared_state(&after);
        changed
    }

    fn is_active_instance(&self) -> bool {
        if self.saw_visible_event {
            return self.zellij_visible;
        }
        self.my_tab == Some(self.active_tab)
    }

    fn can_publish_shared_state(&self) -> bool {
        self.is_active_instance()
    }

    fn panes_for_manifest_tab<'a>(
        &self,
        manifest: &'a PaneManifest,
        tab: &TabInfo,
    ) -> Option<&'a Vec<PaneInfo>> {
        manifest
            .panes
            .get(&tab.position)
            .or_else(|| manifest.panes.get(&tab.tab_id))
    }

    fn detect_my_tab(&mut self, manifest: &PaneManifest) {
        let my_id = get_plugin_ids().plugin_id;
        for tab in &self.app.tabs {
            let Some(panes) = self.panes_for_manifest_tab(manifest, tab) else {
                continue;
            };
            if panes.iter().any(|pane| pane.is_plugin && pane.id == my_id) {
                self.my_tab = Some(tab.position);
                self.my_tab_id = Some(tab.tab_id);
                return;
            }
        }

        for (tab, panes) in &manifest.panes {
            if panes.iter().any(|pane| pane.is_plugin && pane.id == my_id) {
                self.my_tab = Some(*tab);
                self.my_tab_id = None;
                return;
            }
        }
    }

    fn detect_hud_peers(&mut self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        let mut peers: Vec<u32> = manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| {
                pane.is_plugin
                    && pane.id != my_id
                    && pane.title != crate::search::PANE_TITLE
                    && pane.title != crate::whichkey::PANE_TITLE
                    && pane
                        .plugin_url
                        .as_deref()
                        .map(|url| url.contains("zj-hud"))
                        .unwrap_or(false)
            })
            .map(|pane| pane.id)
            .collect();
        peers.sort_unstable();
        peers.dedup();

        if self.hud_peers == peers {
            return false;
        }

        self.hud_peers = peers;
        true
    }

    fn start_project_root_lookup(&mut self, cwd: PathBuf) {
        if self.app.project_roots.roots.contains_key(&cwd)
            || self.app.project_roots.in_flight.contains(&cwd)
            || self.config.project_markers.is_empty()
        {
            return;
        }

        self.app.project_roots.in_flight.insert(cwd.clone());

        let script = r#"dir=$1; shift; while [ -n "$dir" ]; do for marker in "$@"; do if [ -e "$dir/$marker" ]; then printf '%s\n' "$dir"; exit 0; fi; done; parent=$(dirname "$dir"); if [ "$parent" = "$dir" ]; then break; fi; dir=$parent; done; exit 1"#;
        let cwd_str = cwd.to_string_lossy().to_string();
        let mut args = vec![
            "sh".to_string(),
            "-c".to_string(),
            script.to_string(),
            "zj-hud-project-root".to_string(),
            cwd_str.clone(),
        ];
        args.extend(self.config.project_markers.iter().cloned());

        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let mut ctx = BTreeMap::new();
        ctx.insert(system::CTX_KEY.to_string(), CTX_PROJECT_ROOT.to_string());
        ctx.insert(CTX_PROJECT_CWD.to_string(), cwd_str);
        run_command(&arg_refs, ctx);
    }
}
