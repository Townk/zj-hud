mod click_map;
mod color;
mod config;
mod icons;
mod layout;
mod render;
mod search;
mod segments;
mod shared_state;
mod state;
mod system;
mod tabs;
mod truncation;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zellij_tile::prelude::*;

use click_map::ClickAction;
use config::Config;
use search::SearchPane;
use shared_state::SharedState;
use state::AppState;

register_plugin!(Plugin);

/// Host-import stub for non-wasm builds. `zellij-tile` declares
/// `host_run_plugin_command` as a wasm import resolved by the zellij runtime;
/// on the host target (i.e. `cargo test`) that symbol is undefined and linking
/// fails. The unit tests only exercise pure logic and never issue host
/// commands, so a no-op lets the test binary link. It is compiled out of the
/// real wasm plugin, which keeps the genuine import.
#[cfg(not(target_arch = "wasm32"))]
#[no_mangle]
pub extern "C" fn host_run_plugin_command() {}

/// The plugin binary serves two roles, selected at load time by the `role`
/// configuration key:
///
/// - the status bar (default), and
/// - a floating "visual search" input pane (`role = "search"`).
///
/// Both are the same `.wasm`; Zellij keys plugin instances by (url,
/// configuration), so the differing config makes the search pane a distinct
/// instance from the bar.
enum Plugin {
    Bar(Box<State>),
    Search(SearchPane),
}

impl Default for Plugin {
    fn default() -> Self {
        Plugin::Bar(Box::default())
    }
}

impl ZellijPlugin for Plugin {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        if configuration.get("role").map(String::as_str) == Some("search") {
            *self = Plugin::Search(SearchPane::default());
        }
        match self {
            Plugin::Bar(state) => state.load(configuration),
            Plugin::Search(search) => search.load(configuration),
        }
    }

    fn update(&mut self, event: Event) -> bool {
        match self {
            Plugin::Bar(state) => state.update(event),
            Plugin::Search(search) => search.update(event),
        }
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        match self {
            Plugin::Bar(state) => state.pipe(pipe_message),
            Plugin::Search(_) => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        match self {
            Plugin::Bar(state) => state.render(rows, cols),
            Plugin::Search(search) => search.render(rows, cols),
        }
    }
}

/// Permissions requested by **both** plugin roles (bar and search).
///
/// The two roles share a single wasm URL, and Zellij caches granted
/// permissions *per URL*, overwriting the cache with the exact set last
/// requested. If the roles asked for different sets they'd ping-pong that
/// cache and re-prompt on every session. Requesting one identical (union) set
/// keeps the cached grant stable: the user approves once and never again.
///
/// `ReadApplicationState`/`ReadPaneContents` are the bar's; `RunActionsAsUser`
/// (drive native search) and `InterceptInput` (grab keystrokes) are the search
/// pane's; `RunCommands` and `ChangeApplicationState` are used by both.
/// `MessageAndLaunchOtherPlugins` is required by `pipe_message_to_plugin` —
/// used both for the bar's cross-instance state sync and for the search pane to
/// tell the bar when its dialog is open (the `__zj_statusbar_search` indicator).
/// Without it those messages are silently dropped by the host.
pub const PLUGIN_PERMISSIONS: &[PermissionType] = &[
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::ReadPaneContents,
    PermissionType::RunActionsAsUser,
    PermissionType::InterceptInput,
    PermissionType::MessageAndLaunchOtherPlugins,
];

/// How often the background timer fires. Kept fast (1 s) because Zellij does
/// not emit `PaneUpdate` when only a terminal's OSC window title changes, so
/// `system::refresh_focused_pane_title` has to poll. Other timer-driven work
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

#[derive(Default)]
struct State {
    app: AppState,
    config: Config,
    statusbar_peers: Vec<u32>,
    shared_generation: u64,
    zellij_visible: bool,
    saw_visible_event: bool,
    active_tab: usize,
    active_tab_id: Option<usize>,
    my_tab: Option<usize>,
    my_tab_id: Option<usize>,
    pending_mode: Option<InputMode>,
    pending_mode_started: Option<Instant>,
}

// ─── Mode serialisation ───────────────────────────────────────────────────────

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
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
    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        // The search pane toggles this while its dialog is open/closed. The
        // client mode stays `Normal` throughout (intercept requirement), so the
        // bar can only learn "search is active" from this message.
        if pipe_message.name == search::SEARCH_INDICATOR_PIPE {
            let active = pipe_message.payload.as_deref() == Some("1");
            if self.app.search_active != active {
                self.app.search_active = active;
                self.app.dirty = true;
            }
            return self.app.dirty;
        }

        if pipe_message.name != "__zj_statusbar_sync_state" {
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
                let peers_changed = self.detect_statusbar_peers(&manifest);
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
                self.sync_from_shared_state();
                self.app.dirty = true;
            }
            Event::Visible(is_visible) => {
                self.saw_visible_event = true;
                self.zellij_visible = is_visible;
                if is_visible {
                    self.sync_from_shared_state();
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
                system::refresh_focused_pane_title(&mut self.app);
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
        if mode == InputMode::Normal {
            self.pending_mode = None;
            self.pending_mode_started = None;
            self.publish_or_sync_mode(mode);
            return;
        }

        if self.can_publish_shared_state() {
            self.pending_mode = Some(mode);
            self.pending_mode_started = Some(Instant::now());
            set_timeout(MODE_INDICATOR_DEBOUNCE.as_secs_f64());
        } else {
            self.sync_from_shared_state();
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
        self.publish_or_sync_mode(mode);
        true
    }

    fn publish_or_sync_mode(&mut self, mode: InputMode) {
        if self.can_publish_shared_state() {
            self.with_active_shared_state(|shared, _| {
                shared.publish_mode_update(mode, get_plugin_ids().plugin_id)
            });
        } else {
            self.sync_from_shared_state();
        }
    }

    fn clear_local_mode_indicator(&mut self) {
        self.pending_mode = None;
        self.pending_mode_started = None;
        if self.app.mode != InputMode::Normal {
            self.app.mode = InputMode::Normal;
            self.app.dirty = true;
        }
    }

    fn shared_state_path(&self) -> String {
        let session = if self.app.session_name.is_empty() {
            "unknown".to_string()
        } else {
            sanitize_path_component(&self.app.session_name)
        };
        format!(
            "/tmp/zj-statusbar-state-{}-{}.json",
            get_plugin_ids().zellij_pid,
            session
        )
    }

    fn snapshot_shared_state(&self) -> SharedState {
        SharedState {
            schema_version: shared_state::SCHEMA_VERSION,
            generation: self.shared_generation,
            writer: get_plugin_ids().plugin_id,
            mode: shared_state::mode_name(self.app.mode).to_string(),
        }
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
        pipe_message_to_plugin(
            MessageToPlugin::new("__zj_statusbar_sync_state").with_payload(payload),
        );
    }

    fn sync_from_shared_state(&mut self) -> bool {
        let shared = shared_state::read_state_from(self.shared_state_path()).unwrap_or_default();
        self.apply_shared_state(&shared)
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
        let changed = self.app.mode != mode;
        self.shared_generation = shared.generation;
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
        let before = self.snapshot_shared_state();
        let after = update(before.clone(), self);
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

    fn detect_statusbar_peers(&mut self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        let mut peers: Vec<u32> = manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| {
                pane.is_plugin
                    && pane.id != my_id
                    && pane.title != search::PANE_TITLE
                    && pane
                        .plugin_url
                        .as_deref()
                        .map(|url| url.contains("zj-statusbar") || url.contains("statusbar"))
                        .unwrap_or(false)
            })
            .map(|pane| pane.id)
            .collect();
        peers.sort_unstable();
        peers.dedup();

        if self.statusbar_peers == peers {
            return false;
        }

        self.statusbar_peers = peers;
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
            "zj-statusbar-project-root".to_string(),
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
