mod click_map;
mod color;
mod config;
mod icons;
mod layout;
mod render;
mod search;
mod segments;
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
use state::AppState;

register_plugin!(Plugin);

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
pub const PLUGIN_PERMISSIONS: &[PermissionType] = &[
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::ReadPaneContents,
    PermissionType::RunActionsAsUser,
    PermissionType::InterceptInput,
];

/// How often the background timer fires. Kept fast (1 s) because Zellij does
/// not emit `PaneUpdate` when only a terminal's OSC window title changes, so
/// `system::refresh_focused_pane_title` has to poll. Other timer-driven work
/// (widget refresh, Ghostty fullscreen probe) gates itself behind its own TTL,
/// so faster ticks don't increase the rate of those underlying commands.
const TIMER_INTERVAL: f64 = 1.0;

/// Minimum delay between two `RunCommand` invocations of the same `on_click`
/// shell command. Prevents accidental double-fires from a single click.
const CLICK_RUN_DEBOUNCE: Duration = Duration::from_millis(100);

// Context keys for RunCommandResult routing.
const CTX_MODE_READ: &str = "mode_read";
const CTX_MODE_WRITE: &str = "mode_write";
// Context key carrying the read-generation ID so stale responses are dropped.
const CTX_MODE_READ_ID: &str = "mode_read_id";
const CTX_PROJECT_ROOT: &str = "project_root";
const CTX_PROJECT_CWD: &str = "project_cwd";

#[derive(Default)]
struct State {
    app: AppState,
    config: Config,
}

// ─── Mode serialisation ───────────────────────────────────────────────────────

fn mode_to_str(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Normal => "Normal",
        InputMode::Locked => "Locked",
        InputMode::Resize => "Resize",
        InputMode::Pane => "Pane",
        InputMode::Tab => "Tab",
        InputMode::Scroll => "Scroll",
        InputMode::EnterSearch => "EnterSearch",
        InputMode::Search => "Search",
        InputMode::RenameTab => "RenameTab",
        InputMode::RenamePane => "RenamePane",
        InputMode::Session => "Session",
        InputMode::Move => "Move",
        InputMode::Prompt => "Prompt",
        InputMode::Tmux => "Tmux",
    }
}

fn str_to_mode(s: &str) -> Option<InputMode> {
    match s {
        "Normal" => Some(InputMode::Normal),
        "Locked" => Some(InputMode::Locked),
        "Resize" => Some(InputMode::Resize),
        "Pane" => Some(InputMode::Pane),
        "Tab" => Some(InputMode::Tab),
        "Scroll" => Some(InputMode::Scroll),
        "EnterSearch" => Some(InputMode::EnterSearch),
        "Search" => Some(InputMode::Search),
        "RenameTab" => Some(InputMode::RenameTab),
        "RenamePane" => Some(InputMode::RenamePane),
        "Session" => Some(InputMode::Session),
        "Move" => Some(InputMode::Move),
        "Prompt" => Some(InputMode::Prompt),
        "Tmux" => Some(InputMode::Tmux),
        _ => None,
    }
}

/// Session-scoped path for the shared mode state file.
fn mode_file_path(session_name: &str) -> String {
    session_scoped_tmp("zj-statusbar-mode", session_name)
}

/// Build `/tmp/<prefix>[-<sanitized session>]`.
fn session_scoped_tmp(prefix: &str, session_name: &str) -> String {
    if session_name.is_empty() {
        format!("/tmp/{prefix}")
    } else {
        let safe: String = session_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("/tmp/{prefix}-{safe}")
    }
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
    fn process_event(&mut self, event: Event) {
        match event {
            Event::ModeUpdate(mode_info) => {
                let mode = mode_info.mode;

                // Update session name first so the file path is correct.
                if let Some(name) = mode_info.session_name {
                    self.app.session_name = name;
                }

                if self.app.mode_reset_pending && mode != InputMode::Normal {
                    self.app.dirty = true;
                    return;
                }

                // Mode is either Normal or non-reset-pending: it is
                // authoritative.  Persist it to the shared file so other
                // instances (that haven't received this ModeUpdate yet) can
                // sync on their next tab-focus change.
                let path = mode_file_path(&self.app.session_name);
                let cmd = format!("printf '{}' > {}", mode_to_str(mode), path);
                let mut ctx = BTreeMap::new();
                ctx.insert(system::CTX_KEY.to_string(), CTX_MODE_WRITE.to_string());
                run_command(&["sh", "-c", cmd.as_str()], ctx);

                self.app.mode = mode;
                self.app.mode_confirmed = true;
                self.app.mode_reset_pending = false;
                self.app.dirty = true;
            }
            Event::TabUpdate(tabs) => {
                let old_count = self.app.tabs.len();
                let old_active = self.app.active_tab_index();
                self.app.tabs = tabs;
                let new_count = self.app.tabs.len();
                let new_active = self.app.active_tab_index();

                // Zellij marks each plugin's *own* tab as active=true in TabUpdate,
                // so old_active==new_active for P1 even when the user switches tabs.
                // Instead, trigger on tab count change (tab added or closed), which
                // always changes the count when the user's context has shifted.
                if old_count != new_count || old_active != new_active {
                    self.app.mode = InputMode::Normal;
                    self.app.mode_confirmed = false;
                    self.app.mode_reset_pending = true;
                    self.start_mode_read();
                }
                self.app.dirty = true;
            }
            Event::SessionUpdate(sessions, _) => {
                if let Some(session) = sessions.iter().find(|s| s.is_current_session) {
                    if session.name != self.app.session_name {
                        self.app.session_name = session.name.clone();
                        self.app.dirty = true;
                    }
                }
            }
            Event::PaneUpdate(manifest) => {
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
                    Some(CTX_MODE_READ) => {
                        let expected = self.app.mode_read_id.to_string();
                        let file_content = String::from_utf8_lossy(&stdout);
                        if context.get(CTX_MODE_READ_ID).map(|s| s.as_str())
                            != Some(expected.as_str())
                        {
                            return;
                        }

                        self.app.mode_reset_pending = false;

                        if exit_code == Some(0) {
                            if let Some(mode) = str_to_mode(file_content.trim()) {
                                self.app.mode = mode;
                                self.app.mode_confirmed = true;
                                self.app.dirty = true;
                            }
                        }
                    }
                    Some(CTX_MODE_WRITE) => {
                        // Fire-and-forget write; ignore result entirely.
                    }
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

    /// Kick off a generation-stamped async read of the shared mode file.
    fn start_mode_read(&mut self) {
        self.app.mode_read_id += 1;
        let id = self.app.mode_read_id;
        let path = mode_file_path(&self.app.session_name);
        let mut ctx = BTreeMap::new();
        ctx.insert(system::CTX_KEY.to_string(), CTX_MODE_READ.to_string());
        ctx.insert(CTX_MODE_READ_ID.to_string(), id.to_string());
        run_command(&["cat", path.as_str()], ctx);
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
