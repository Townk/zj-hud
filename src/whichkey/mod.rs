//! zj-which-key — a which-key style floating panel of mode keybindings.
//!
//! Spawned **per tab** by the layout's `default_tab_template` as a tiny,
//! non-selectable floating pane that only paints when resized into the panel
//! (see `layouts/default.kdl` and `load`). The active tab's instance reduces
//! mode trail, manual suppression, and page into a `SharedState`, writes it to a
//! session-scoped file, and other per-tab instances apply it when they become
//! active. An instance only reveals its panel while **its own tab is the active
//! one**; on the other tabs it stays parked. This is what makes the panel follow
//! you across tabs (a *single* float is per-tab in Zellij and cannot span tabs).
//!
//! On every `ModeUpdate` it reads the live keymap (`get_keybinds_for_mode`) and
//! the base mode, then:
//!
//!   * in a *dismiss* mode (configurable; always includes `base_mode`) it parks
//!     the float at 1x1 and paints nothing;
//!   * otherwise it resizes itself into a **pinned, non-selectable** floating
//!     pane, titles the frame with the mode glyph, positions it from the
//!     anchor/padding config against the screen size (learned from `TabUpdate`),
//!     and hands focus back to the originating pane.
//!
//! Pinning keeps the panel visible while you move focus between tiled panes;
//! non-selectable + focus-return means it never steals focus and is never the
//! target of mode actions (close / fullscreen / move-focus, etc.).
//!
//! ## Permissions
//! This role shares the union [`crate::PLUGIN_PERMISSIONS`] requested by every
//! role (so Zellij's per-URL permission cache stays stable). Of those it uses:
//!   * `ReadApplicationState` — `ModeUpdate` / `TabUpdate` / pane manifest.
//!   * `ChangeApplicationState` — rename / float / pin / show / hide / focus.
//!   * `MessageAndLaunchOtherPlugins` — broadcast the unified shared state.
//!
//! The host-testable config/geometry/grid/labels/render logic lives in the
//! sibling submodules (`config`, `geometry`, `grid`, `labels`, `render`, …).

pub mod config;
pub mod footer;
pub mod grid;
pub mod labels;
pub mod modes;
pub mod render;
pub mod tab_lookup;
pub mod theme;

use std::collections::BTreeMap;

use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::*;

use crate::shared::geometry::{self, Rect, WidthMode};
use crate::shared::state::{self as shared_state, SharedState, SCHEMA_VERSION};
use config::Config;
use grid::{diagnostics, lay_out, Columns, Layout};
use labels::merge_keybinds;
use render::{page_lines, paint, Frame, NO_BINDINGS};
use tab_lookup::{resolve_tab_key, TabRef};
use theme::Theme;

/// Stable pane title for the which-key float. Used both to name the pane and to
/// recognise sibling which-key instances in the manifest (peer detection) and to
/// exclude them from the bar's peer set — all three roles share one wasm URL, so
/// the title is what distinguishes them. The user-facing breadcrumb is drawn in
/// the panel frame (see [`WhichKeyPane::breadcrumb`]), not in this pane name.
pub const PANE_TITLE: &str = "zj-which-key";

/// Best-effort append to a debug log inside the plugin's WASI sandbox. Silently
/// ignores any error (the path's root must be a Zellij-preopened dir, e.g.
/// `/host`). Debug instrumentation only — gated by the `debug_log` config.
fn append_log(path: &str, content: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(content.as_bytes());
    }
}

pub struct WhichKeyPane {
    config: Config,
    /// `ChangeApplicationState` has been granted. Until then we can't make the
    /// pane non-selectable or move it to its hidden parking coordinates.
    granted: bool,
    /// Plugin-level setup has run (idempotent; works on fresh or cached grant).
    ready: bool,
    /// `set_floating_pane_pinned` applied once (a background plugin has no
    /// floating pane to pin until the first reveal).
    pinned: bool,
    /// Whether the panel is currently revealed.
    visible: bool,
    /// A reveal has been staged: coordinates were sent to Zellij, but rendering
    /// stays disabled until a short timer gives the move a chance to land.
    reveal_pending: bool,
    /// Whether the pane has completed its first reveal. The first reveal uses
    /// `show_pane_with_id` to materialize the float reliably; later reveals can
    /// just move the parked pane and show the floating layer, avoiding a brief
    /// Zellij restore at a default location.
    revealed_once: bool,
    /// We explicitly hide the tab floating layer on real dismissals to avoid
    /// stale cross-tab panes. Manual hide does not touch the layer, so repeat
    /// manual reveals can skip `ShowFloatingPanes`.
    floating_layer_hidden: bool,
    /// Manual hide override toggled by the `wk_toggle_pane` pipe. When set, the
    /// panel stays hidden even in modes that would otherwise show it; it
    /// persists across mode changes (in-memory, session-only) until toggled
    /// again. Set when the user dismisses a *visible* panel, cleared otherwise.
    suppressed: bool,
    /// Screen size `(cols, rows)` from the active tab's display area.
    screen: (usize, usize),
    /// Current mode (drives content + visibility + frame title).
    mode: InputMode,
    /// Session base/default mode — always treated as a dismiss mode.
    base_mode: InputMode,
    /// Synthetic mode history (Zellij keeps none). Holds the trail of visible
    /// modes entered to reach `mode`, oldest first. Entering a non-dismiss mode
    /// pushes the mode we came from; entering the mode currently on top pops it
    /// (a "back" step). Cleared whenever we land in a dismiss mode. Drives the
    /// breadcrumb title and `wk_go_back`.
    backstack: Vec<InputMode>,
    /// Live keybindings for `mode`, straight from the user's config.
    keybinds: Vec<(KeyWithModifier, Vec<Action>)>,
    /// Grid pages computed for the current mode + screen.
    layout: Layout,
    /// Current page index into `layout`.
    page: usize,
    /// Footer for the current mode + page, if any.
    footer: Option<footer::Footer>,
    /// Latest raw `ModeUpdate` payload from Zellij. Its `mode` can flap through
    /// transient modes, but its keymap/theme data is still the freshest source
    /// for rendering whichever stable mode the Bar publishes.
    mode_info: Option<ModeInfo>,
    /// The terminal pane to hand focus back to, captured from the manifest.
    origin: Option<PaneId>,
    /// Zellij frame overhead in rows (outer height − interior rows), self-
    /// calibrated in `render` from the interior Zellij actually reports. Starts
    /// at 2 (a top + bottom border) and converges to the real value — 0 once
    /// `set_pane_borderless` takes effect, 2 while Zellij still frames us — so
    /// the pane is sized to show every packed body row.
    frame_rows: usize,
    /// Zellij frame overhead in columns (outer width − interior cols), self-
    /// calibrated alongside `frame_rows`. Without this, a Zellij-drawn frame
    /// steals 2 columns and the content overflows onto the right border.
    frame_cols: usize,
    /// The outer height we last asked Zellij for, used to derive `frame_rows`.
    req_height: usize,
    /// The outer width we last asked Zellij for, used to derive `frame_cols`.
    req_width: usize,
    /// Interior colors derived from the live palette (`ModeInfo::style`).
    theme: Theme,
    /// `set_pane_borderless` applied once — we draw our own frame instead of
    /// Zellij's (which would also stamp `SCROLL`/`PIN` indicators on it).
    borderless: bool,
    /// Whether the active tab has another (non-self) floating pane shown
    /// (tracked from `PaneUpdate`). When true we stay hidden so we don't fight
    /// it for focus or clutter it (e.g. the visual-search dialog, which sits in
    /// `Search` mode just like our nav panel would).
    other_float: bool,
    /// Whether the active tab has *any* other floating pane shown — a foreign
    /// plugin (session-manager, configuration, about, …), a toggled floating
    /// terminal, or our own search dialog (tracked from `PaneUpdate`). Unlike
    /// [`Self::other_float`] (which only yields *visibility* to our sibling
    /// search dialog), this drives the floating-*layer* and focus decisions:
    /// while another float shares the tab we must never hide/show the shared
    /// layer (that would drag their pane along) nor pull focus back to our
    /// origin terminal (that would steal focus from their pane). It keeps
    /// which-key fully decoupled from any other float.
    any_other_float: bool,
    /// Latest tab metadata from `TabUpdate`. Used to resolve `PaneManifest`
    /// entries whether Zellij keys them by position (documented) or stable
    /// `tab_id` (observed after some resurrection/reorder paths).
    tabs: Vec<TabInfo>,
    /// Session name from `ModeUpdate`, used to scope the shared host-state file.
    session_name: Option<String>,
    /// Peer which-key plugin ids observed in the pane manifest. Shared state is
    /// broadcast to these ids because `/data` is per instance in this layout.
    which_key_peers: Vec<u32>,
    /// Last shared-state generation applied locally.
    shared_generation: u64,
    /// Most recent full `SharedState` seen, so writes preserve fields this role
    /// does not own (`mode`/`base_mode`/`backstack`/`search_active`).
    last_shared: SharedState,
    /// Last generation for which this instance hid all tab floating layers.
    last_hide_everywhere_generation: Option<u64>,
    /// Whether Zellij currently considers this plugin pane visible. This is the
    /// most direct active-tab signal for per-tab plugin instances.
    zellij_visible: bool,
    /// Until the first `Visible` event arrives, fall back to tab metadata.
    saw_visible_event: bool,
    /// Active tab position (from `TabUpdate`). Used to scope origin detection
    /// and float-presence checks to the tab the user is actually looking at, so
    /// switching tabs can't leave us refocusing a stale origin from another tab.
    active_tab: usize,
    /// Stable id for the active tab, paired with [`Self::active_tab`].
    active_tab_id: Option<usize>,
    /// This instance's home tab position, learned from the manifest (the tab
    /// whose panes contain our own plugin pane). `None` until first seen. We are
    /// one of several per-tab instances spawned by the layout; each reveals its
    /// panel only while its home tab is the active tab (`my_tab == active_tab`).
    my_tab: Option<usize>,
    /// Stable id for this instance's home tab, paired with [`Self::my_tab`].
    my_tab_id: Option<usize>,
    /// Per-instance debug log path: the configured `debug_log` with our plugin
    /// id spliced before the extension, so the concurrent per-tab instances each
    /// write their own file instead of garbling one. Resolved in `ensure_ready`.
    log_path: Option<String>,
    /// Per-instance lightweight state trace path, derived from `state_log`.
    state_log_path: Option<String>,
    /// The breadcrumb mode trail this instance last actually painted while
    /// visible (see [`Self::breadcrumb_modes`]). The title is drawn from
    /// `mode` (live, from `ModeUpdate`) + `backstack` (adopted from the Bar's
    /// shared state, which lands a beat later because the Bar debounces its
    /// publish). Those two arrive on separate events, so the backstack can be
    /// updated by a shared-state adoption that reports no field-level change
    /// (e.g. a redundant re-delivery after a silent sync) and would otherwise
    /// leave the previously painted, now-stale title frozen on screen. We
    /// compare against this to force a repaint whenever the visible trail drifts.
    painted_trail: Vec<InputMode>,
}

impl Default for WhichKeyPane {
    fn default() -> Self {
        Self {
            config: Config::default(),
            granted: false,
            ready: false,
            pinned: false,
            visible: false,
            reveal_pending: false,
            revealed_once: false,
            floating_layer_hidden: false,
            suppressed: false,
            screen: (0, 0),
            mode: InputMode::Normal,
            base_mode: InputMode::Normal,
            backstack: Vec::new(),
            keybinds: Vec::new(),
            layout: Layout {
                pages: Vec::new(),
                content_width: 0,
                page_widths: Vec::new(),
            },
            page: 0,
            footer: None,
            mode_info: None,
            origin: None,
            frame_rows: 2,
            frame_cols: 2,
            req_height: 0,
            req_width: 0,
            theme: Theme::default(),
            borderless: false,
            other_float: false,
            any_other_float: false,
            tabs: Vec::new(),
            session_name: None,
            which_key_peers: Vec::new(),
            shared_generation: 0,
            last_shared: SharedState::default(),
            last_hide_everywhere_generation: None,
            zellij_visible: false,
            saw_visible_event: false,
            active_tab: 0,
            active_tab_id: None,
            my_tab: None,
            my_tab_id: None,
            log_path: None,
            state_log_path: None,
            painted_trail: Vec::new(),
        }
    }
}

/// Splice a plugin-instance id into a log path before its extension, so the
/// per-tab instances write distinct files: `.../foo.log` -> `.../foo.<id>.log`.
fn insert_instance_tag(path: &str, id: u32) -> String {
    let name_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    match path[name_start..].rfind('.') {
        Some(rel) => {
            let dot = name_start + rel;
            format!("{}.{}{}", &path[..dot], id, &path[dot..])
        }
        None => format!("{path}.{id}"),
    }
}

fn format_shared_state(state: &SharedState) -> String {
    format!(
        "gen={} writer={} mode={} base={} back={:?} suppressed={} page={}",
        state.generation,
        state.writer,
        state.mode,
        state.base_mode,
        state.backstack,
        state.suppressed,
        state.page,
    )
}

impl ZellijPlugin for WhichKeyPane {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.config = Config::from_map(&configuration);
        // Request the identical union set for every role so Zellij's per-URL
        // permission cache stays stable (see `crate::PLUGIN_PERMISSIONS`).
        // WhichKey itself only uses `ReadApplicationState`/`ChangeApplicationState`
        // plus `MessageAndLaunchOtherPlugins` (state broadcast); the rest are
        // harmless here.
        request_permission(crate::PLUGIN_PERMISSIONS);
        subscribe(&[
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::Visible,
            EventType::Timer,
        ]);
        // NB: setup that changes pane state is gated on the permission grant.
        // `render` paints nothing until the panel is explicitly visible.
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                self.ensure_ready();
                self.state_log(&format!("[event:permission:granted] {}\n", self.context()));
                self.sync_from_shared_state();
                self.apply_visibility();
                true
            }
            Event::PermissionRequestResult(_) => true,
            Event::TabUpdate(tabs) => {
                self.tabs = tabs.clone();
                if let Some(active) = tabs.iter().find(|t| t.active) {
                    self.active_tab = active.position;
                    self.active_tab_id = Some(active.tab_id);
                    self.state_log(&format!(
                        "[event:tab] active_pos={} active_id={} screen={}x{} before={}\n",
                        active.position,
                        active.tab_id,
                        active.display_area_columns,
                        active.display_area_rows,
                        self.context()
                    ));
                    let next = (active.display_area_columns, active.display_area_rows);
                    if next != self.screen {
                        self.screen = next;
                        self.rebuild();
                        if self.visible {
                            self.reposition();
                        }
                    }
                    // The active tab gates our per-tab visibility: when it
                    // changes we may need to reveal (our tab just became active)
                    // or hide (it stopped being active). If `my_tab` isn't known
                    // yet the following `PaneUpdate` reconciles instead.
                    self.sync_from_shared_state();
                    if self.should_show() != self.visible {
                        self.apply_visibility();
                    }
                }
                false
            }
            Event::PaneUpdate(manifest) => {
                self.ensure_ready();
                let prev_origin = self.origin;
                self.detect_my_tab(&manifest);
                let peers_changed = self.detect_which_key_peers(&manifest);
                if peers_changed && self.shared_generation > 0 {
                    self.state_log(&format!(
                        "[peers:broadcast-current] peers={:?} {}\n",
                        self.which_key_peers,
                        self.context()
                    ));
                    self.broadcast_shared_state(&self.snapshot_shared_state());
                }
                self.state_log(&format!(
                    "[event:pane] after_detect_my_tab panes_tabs={} {}\n",
                    manifest.panes.len(),
                    self.context()
                ));
                self.detect_origin(&manifest);
                let origin_in_tab = self.origin_in_active_tab(&manifest);
                self.sync_from_shared_state();
                // Recompute float-yield (search dialog etc.) so it folds into the
                // visibility decision below.
                self.other_float = self.other_float_present(&manifest);
                // Recompute the broader "any other float shares the tab" signal
                // that gates the floating-layer + focus actions (so we never
                // disturb a foreign plugin/dialog/terminal float the user opened).
                self.any_other_float = self.any_other_float_present(&manifest);
                // Reconcile against the desired state. With per-tab instances the
                // trigger is usually `my_tab`/`active_tab` changing (the previous
                // tab's panel must hide, the new tab's must show); also covers
                // mode/float changes that landed via this manifest.
                if self.should_show() != self.visible {
                    self.apply_visibility();
                }
                // Backstop: if Zellij still has our pane shown but we no longer
                // want it (eg. active tab changed, or a search dialog appeared
                // while local state already thought we were hidden), park it.
                if self.self_float_is_shown(&manifest) && !self.should_show() {
                    self.log("[pane] reconcile: force-hide rogue float\n");
                    self.begin_hide();
                }
                let self_focused = self.self_is_focused(&manifest);
                let terminal_focused = self.active_terminal_is_focused(&manifest);
                if self_focused {
                    set_selectable(false);
                }
                // We are a non-focus-stealing overlay: while *shown* we must
                // never hold focus, so if we end up focused we hand it straight
                // back to the active tab's terminal. Gate on `origin_in_tab` so
                // we only ever refocus a same-tab origin (never a stale one from
                // another tab). (`origin_in_tab` computed above, pre-reconcile.)
                self.log(&format!(
                    "[pane] mode={:?} my_tab={:?} active_tab={} visible={} other_float={} self_focused={} terminal_focused={} origin_in_tab={} origin {:?}->{:?}\n",
                    self.mode, self.my_tab, self.active_tab, self.visible, self.other_float, self_focused, terminal_focused, origin_in_tab, prev_origin, self.origin,
                ));
                // Diagnostic: dump every *plugin* pane's topology (which tab it
                // sits on, floating/suppressed/focused), plus any focused pane.
                // When active_tab oscillates with us passive, this reveals where
                // our (and search's) lingering floating pane actually lives.
                for (tab, panes) in &manifest.panes {
                    for p in panes {
                        if p.is_plugin {
                            self.log(&format!(
                                "    plugin: tab={tab} id={} floating={} suppressed={} selectable={} focused={} title={:?}\n",
                                p.id, p.is_floating, p.is_suppressed, p.is_selectable, p.is_focused, p.title,
                            ));
                        } else if p.is_focused {
                            self.log(&format!(
                                "    term: tab={tab} id={} floating={} focused=true title={:?}\n",
                                p.id, p.is_floating, p.title,
                            ));
                        }
                    }
                }
                // Only touch focus if we are the only focused pane. Zellij can
                // report both the tiled terminal and our float as focused; in
                // that case refocusing the terminal is redundant and can race
                // with another floating dialog that is trying to take focus.
                if self.visible
                    && self_focused
                    && !terminal_focused
                    && !self.any_other_float
                    && origin_in_tab
                {
                    self.log("[pane] refocus_origin\n");
                    self.refocus_origin();
                }
                false
            }
            Event::ModeUpdate(mode_info) => {
                self.ensure_ready();
                if self.session_name != mode_info.session_name {
                    self.session_name = mode_info.session_name.clone();
                    self.state_log(&format!("[event:mode:session] {}\n", self.context()));
                }
                let incoming_mode = mode_info.mode;
                let incoming_base = mode_info.base_mode.unwrap_or(InputMode::Normal);
                let incoming_keybinds = mode_info.get_keybinds_for_mode(incoming_mode).len();
                self.mode_info = Some(mode_info);
                self.state_log(&format!(
                    "[event:mode] incoming={:?} base={:?} keybinds={} before={}\n",
                    incoming_mode,
                    incoming_base,
                    incoming_keybinds,
                    self.context()
                ));
                self.sync_from_shared_state();
                // Raw Zellij mode updates can briefly flap through intermediate
                // modes (`Tmux -> Tab -> Normal`) while pane activity is racing
                // in another tab. The Bar already debounces and publishes the
                // stable mode; use that for visibility/content, and use this raw
                // event only as the freshest keymap/theme source.
                self.apply_shared_display_mode();
                self.rebuild();
                self.set_title();
                self.apply_visibility();
                true
            }
            Event::Visible(is_visible) => {
                self.saw_visible_event = true;
                self.zellij_visible = is_visible;
                self.ensure_ready();
                self.state_log(&format!(
                    "[event:visible] is_visible={} before_sync={}\n",
                    is_visible,
                    self.context()
                ));
                if is_visible {
                    self.sync_from_shared_state();
                    self.apply_visibility();
                    true
                } else {
                    if self.visible {
                        self.begin_hide();
                        true
                    } else {
                        false
                    }
                }
            }
            Event::Timer(_) => {
                if self.reveal_pending {
                    self.reveal_pending = false;
                    if self.should_show() {
                        self.finish_reveal();
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name == shared_state::SYNC_PIPE {
            if let Some(payload) = pipe_message.payload.as_deref() {
                match serde_json::from_str::<SharedState>(payload) {
                    Ok(shared) => {
                        self.state_log(&format!(
                            "[pipe:sync] incoming={} before={}\n",
                            format_shared_state(&shared),
                            self.context()
                        ));
                        let incoming_generation = shared.generation;
                        let local_generation = self.shared_generation;
                        let changed = self.apply_shared_state(&shared);
                        if incoming_generation >= local_generation {
                            self.cache_shared_state(&shared);
                        }
                        self.apply_visibility();
                        // The Bar debounces its mode publish, so the corrected
                        // backstack (e.g. `Scroll` pushed when entering `Search`)
                        // can arrive a beat after we already painted the title
                        // from the live `ModeUpdate`. `apply_shared_state` may
                        // report `changed=false` here (the backstack field was
                        // already updated by an earlier silent sync), so also
                        // repaint whenever the visible title has drifted.
                        let repaint = changed || self.breadcrumb_stale();
                        self.state_log(&format!(
                            "[pipe:sync:done] changed={} repaint={} after={}\n",
                            changed,
                            repaint,
                            self.context()
                        ));
                        return repaint;
                    }
                    Err(err) => {
                        self.state_log(&format!("[pipe:sync:error] invalid payload: {err}\n"))
                    }
                }
            }
            return false;
        }
        if pipe_message.name == "wk_toggle_pane" {
            // Force-toggle visibility (shared across per-tab instances).
            // The live pipe carries its name even though the host strips it from
            // the displayed keymap, so this fires regardless of how it's bound.
            return self.with_active_shared_state(|shared, this| {
                shared.toggle(this.visible, get_plugin_ids().plugin_id)
            });
        }
        if pipe_message.name == "wk_go_back" {
            // Step back along the synthetic mode trail: switch Zellij to the
            // mode on top of the stack (or the base mode when empty, which
            // dismisses the panel). The resulting `ModeUpdate` updates the
            // shared trail and repaints — nothing to render here.
            if self.is_active_instance() {
                self.sync_from_shared_state();
                let target = self.backstack.last().copied().unwrap_or(self.base_mode);
                switch_to_input_mode(&target);
            }
            return false;
        }
        let page_count = self.layout.page_count();
        let changed = match pipe_message.name.as_str() {
            "wk_next_page" => self.with_active_shared_state(|shared, _| {
                shared.next_page(page_count, get_plugin_ids().plugin_id)
            }),
            "wk_prev_page" => self
                .with_active_shared_state(|shared, _| shared.prev_page(get_plugin_ids().plugin_id)),
            _ => return false,
        };
        if changed && self.visible {
            self.reposition();
        }
        changed
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.log(&format!(
            "[render] mode={:?} visible={} zellij_rows={rows} cols={cols} req_height={} frame_rows={} page={}\n",
            self.mode, self.visible, self.req_height, self.frame_rows, self.page,
        ));
        // Hidden instances stay alive as tiny parked floats to keep receiving
        // mode updates. Paint nothing until this instance is the visible panel.
        if !self.visible {
            print!("");
            return;
        }
        self.calibrate_frame(rows, cols);
        let body = page_lines(&self.layout, self.page);
        let (title, title_w) = self.breadcrumb();
        // Record the trail we are about to paint so a later shared-state
        // adoption that doesn't register a field-level change can still detect
        // that the on-screen title drifted and force a repaint.
        self.painted_trail = self.breadcrumb_modes();
        let frame = Frame {
            theme: &self.theme,
            title: &title,
            title_w,
            pad: self.config.padding,
        };
        print!("{}", paint(&body, self.footer.as_ref(), rows, cols, &frame));
    }
}

impl WhichKeyPane {
    /// Rows our self-drawn frame occupies (top + bottom border).
    const BORDER_ROWS: usize = 2;
    /// Columns our self-drawn frame occupies (left + right border).
    const BORDER_COLS: usize = 2;

    /// Append a line to the debug log when `debug_log` is configured. Writes to
    /// the per-instance path (resolved in [`Self::ensure_ready`]) so concurrent
    /// per-tab instances don't interleave; falls back to the raw configured path
    /// before setup has run.
    fn log(&self, msg: &str) {
        if let Some(path) = self.log_path.as_ref().or(self.config.debug_log.as_ref()) {
            append_log(path, msg);
        }
    }

    fn state_log(&self, msg: &str) {
        if let Some(path) = self
            .state_log_path
            .as_ref()
            .or(self.config.state_log.as_ref())
        {
            append_log(path, msg);
        }
    }

    fn context(&self) -> String {
        format!(
            "pid={} session={:?} state_path={} peers={:?} my_tab={:?}/{:?} active={}/{} zvis={} saw_zvis={} visible={} want={} suppress={} other_float={} any_other_float={} mode={:?} base={:?} back={:?} gen={}",
            get_plugin_ids().plugin_id,
            self.session_name,
            self.shared_state_path(),
            self.which_key_peers,
            self.my_tab,
            self.my_tab_id,
            self.active_tab,
            self.active_tab_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.zellij_visible,
            self.saw_visible_event,
            self.visible,
            self.want_visible(),
            self.suppressed,
            self.other_float,
            self.any_other_float,
            self.mode,
            self.base_mode,
            self.backstack,
            self.shared_generation,
        )
    }

    fn persist_shared_state(&self, state: &SharedState) {
        self.cache_shared_state(state);
        self.broadcast_shared_state(state);
    }

    fn cache_shared_state(&self, state: &SharedState) {
        let path = self.shared_state_path();
        self.state_log(&format!(
            "[state:cache] path={} {}\n",
            path,
            format_shared_state(state)
        ));
        if let Err(err) = shared_state::write_state_to(&path, state) {
            self.state_log(&format!("[state:cache:error] {err}\n"));
        } else {
            self.state_log("[state:cache:ok]\n");
        }
    }

    fn broadcast_shared_state(&self, state: &SharedState) {
        let Ok(payload) = serde_json::to_string(state) else {
            self.state_log("[pipe:broadcast:error] failed to serialize shared state\n");
            return;
        };
        self.state_log(&format!(
            "[pipe:broadcast] destination=all peers={:?} {}\n",
            self.which_key_peers,
            format_shared_state(state)
        ));
        pipe_message_to_plugin(MessageToPlugin::new(shared_state::SYNC_PIPE).with_payload(payload));
    }

    /// The single session-scoped state file shared with the Bar and Search
    /// roles (same wasm URL ⇒ same `pipe_message_to_plugin` broadcast).
    fn shared_state_path(&self) -> String {
        let session = self.session_name.as_deref().unwrap_or("");
        shared_state::state_path(get_plugin_ids().zellij_pid, session)
    }

    /// A full snapshot for broadcasting on peer change. We own only
    /// `suppressed`/`page`; the Bar-owned `mode`/`base_mode`/`backstack` and the
    /// Search-owned `search_active` are carried verbatim from the last applied
    /// state so we never clobber them.
    fn snapshot_shared_state(&self) -> SharedState {
        let mut snapshot = self.last_shared.clone();
        snapshot.schema_version = SCHEMA_VERSION;
        snapshot.generation = self.shared_generation;
        snapshot.writer = get_plugin_ids().plugin_id;
        snapshot.suppressed = self.suppressed;
        snapshot.page = self.page;
        snapshot
    }

    fn sync_from_shared_state(&mut self) -> bool {
        let path = self.shared_state_path();
        let shared = shared_state::read_state_from(&path).unwrap_or_default();
        self.state_log(&format!(
            "[state:read] path={} {} local_before={}\n",
            path,
            format_shared_state(&shared),
            self.context()
        ));
        self.apply_shared_state(&shared)
    }

    fn apply_shared_display_mode(&mut self) {
        self.mode = self.last_shared.mode();
        self.base_mode = self.last_shared.base_mode();
        if let Some(mode_info) = &self.mode_info {
            self.theme = Theme::from_style_and_colors(&mode_info.style, &self.config.chrome);
            self.keybinds = mode_info.get_keybinds_for_mode(self.mode);
        }
    }

    /// Adopt the Bar-owned stable mode/trail and the shared suppression/page
    /// from a fresh `SharedState`. Raw Zellij `ModeUpdate`s can oscillate
    /// through transient modes under cross-tab pane activity, so visibility is
    /// keyed to the Bar's debounced shared `mode`; raw mode events only refresh
    /// the keymap/theme cache used to render that stable mode.
    fn apply_shared_state(&mut self, shared: &SharedState) -> bool {
        if shared.generation < self.shared_generation {
            self.state_log(&format!(
                "[state:apply:stale] incoming={} local={}\n",
                format_shared_state(shared),
                self.context()
            ));
            return false;
        }
        let backstack = shared.backstack();
        let mode = shared.mode();
        let base_mode = shared.base_mode();
        let palette_changed =
            !shared.palette.is_empty() && shared.palette != self.last_shared.palette;
        let config_changed = !shared.which_key_config.is_empty()
            && shared.which_key_config != self.last_shared.which_key_config;
        let changed = self.backstack != backstack
            || self.mode != mode
            || self.base_mode != base_mode
            || self.suppressed != shared.suppressed
            || self.page != shared.page
            || palette_changed
            || config_changed;

        self.shared_generation = shared.generation;
        self.last_shared = shared.clone();

        if !changed {
            self.state_log(&format!(
                "[state:apply:unchanged] incoming={} local={}\n",
                format_shared_state(shared),
                self.context()
            ));
            return false;
        }

        self.state_log(&format!(
            "[state:apply:changed] incoming={} local_before={}\n",
            format_shared_state(shared),
            self.context()
        ));
        self.backstack = backstack;
        self.suppressed = shared.suppressed;
        self.page = shared.page;
        self.mode = mode;
        self.base_mode = base_mode;
        if config_changed {
            // Adopt the bar-authored `which_key { … }` block (single source of
            // truth). Preserve the operational debug/state log paths resolved at
            // load, which are not part of the display block.
            let mut cfg = Config::from_block(&shared.which_key_config);
            if cfg.debug_log.is_none() {
                cfg.debug_log = self.config.debug_log.clone();
            }
            if cfg.state_log.is_none() {
                cfg.state_log = self.config.state_log.clone();
            }
            self.config = cfg;
        }
        if config_changed || palette_changed {
            // (Re)apply the Bar's palette on top of the (possibly fresh) config
            // so the breadcrumb/labels match the status bar.
            if !shared.palette.is_empty() {
                self.config.apply_palette(&shared.palette);
            }
        }
        self.apply_shared_display_mode();
        self.rebuild();
        self.set_title();
        if config_changed && self.visible {
            self.reposition();
        }
        self.state_log(&format!(
            "[state:apply:done] local_after={}\n",
            self.context()
        ));
        changed
    }

    fn with_active_shared_state(
        &mut self,
        update: impl FnOnce(SharedState, &Self) -> SharedState,
    ) -> bool {
        if !self.can_publish_shared_state() {
            self.state_log(&format!("[state:update:denied] {}\n", self.context()));
            self.sync_from_shared_state();
            self.apply_visibility();
            return false;
        }
        let path = self.shared_state_path();
        let from_disk = shared_state::read_state_from(&path).unwrap_or_default();
        self.state_log(&format!(
            "[state:update:disk-before] path={} {}\n",
            path,
            format_shared_state(&from_disk)
        ));
        self.apply_shared_state(&from_disk);
        // Start from the freshest full state so the closure rewrites only the
        // WhichKey-owned fields (`suppressed`/`page`) and preserves the rest.
        let before = self.last_shared.clone();
        let after = update(before.clone(), self);
        let changed = after != before;
        self.state_log(&format!(
            "[state:update:reduced] before={} after={} changed={}\n",
            format_shared_state(&before),
            format_shared_state(&after),
            changed
        ));
        if changed {
            self.persist_shared_state(&after);
        }
        self.apply_shared_state(&after);
        self.apply_visibility();
        changed
    }

    /// Idempotent plugin-level setup, run once after the permission grant:
    /// resolve our per-instance log path, title ourselves, and become
    /// display-only. Gated on `granted` because `set_selectable` /
    /// `rename_plugin_pane` need `ChangeApplicationState`; calling them pre-grant
    /// is silently dropped. We stay alive as a tiny floating pane so Zellij keeps
    /// delivering mode updates, but render nothing until resized on demand.
    fn ensure_ready(&mut self) {
        if self.ready || !self.granted {
            return;
        }
        self.ready = true;
        if let Some(base) = &self.config.debug_log {
            let plugin_ids = get_plugin_ids();
            self.log_path = Some(insert_instance_tag(base, plugin_ids.plugin_id));
        }
        if let Some(base) = &self.config.state_log {
            let plugin_ids = get_plugin_ids();
            self.state_log_path = Some(insert_instance_tag(base, plugin_ids.plugin_id));
        }
        self.state_log(&format!("[ready] {}\n", self.context()));
        self.set_title();
        set_pane_borderless(self.me(), true);
        self.borderless = true;
        self.frame_rows = 0;
        self.frame_cols = 0;
        set_selectable(false);
        show_pane_with_id(self.me(), false, false);
        self.park_hidden();
    }

    fn me(&self) -> PaneId {
        PaneId::Plugin(get_plugin_ids().plugin_id)
    }

    /// Name the pane with a stable sentinel ([`PANE_TITLE`]) so sibling
    /// which-key instances and the bar can recognise it among the three roles
    /// sharing one wasm URL. The user-facing breadcrumb is drawn in the panel
    /// frame (see [`Self::breadcrumb`]), not in this pane name.
    fn set_title(&self) {
        if self.ready {
            rename_plugin_pane(get_plugin_ids().plugin_id, PANE_TITLE);
        }
    }

    /// The ordered mode trail shown in the title: the backstack (oldest first)
    /// followed by the current mode.
    fn breadcrumb_modes(&self) -> Vec<InputMode> {
        let mut modes = self.backstack.clone();
        modes.push(self.mode);
        modes
    }

    /// True when the visible panel's last-painted breadcrumb ([`Self::painted_trail`])
    /// no longer matches the trail we would draw now. Used to force a repaint
    /// after a delayed shared-state adoption updates the backstack without the
    /// adopting call itself reporting a change (see [`Self::painted_trail`]).
    fn breadcrumb_stale(&self) -> bool {
        self.visible && self.breadcrumb_modes() != self.painted_trail
    }

    /// Colored breadcrumb for the self-drawn frame title: each mode glyph in its
    /// configured color, joined by a dim `»`. Returns the SGR-laden string and
    /// its **visible** width (so the frame can size the title run correctly).
    fn breadcrumb(&self) -> (String, usize) {
        const SEP: &str = "\u{00BB}"; // »
        let (dim, reset) = (&self.theme.dim, &self.theme.reset);
        let mut text = String::new();
        let mut width = 0usize;
        for (i, mode) in self.breadcrumb_modes().iter().enumerate() {
            if i > 0 {
                text.push_str(&format!(" {dim}{SEP}{reset} "));
                width += 2 + SEP.chars().count(); // " » "
            }
            let sym = self.config.symbol(*mode);
            let color = self.config.symbol_color(*mode);
            width += sym.chars().count();
            text.push_str(&format!("{color}{sym}{reset}"));
        }
        (text, width)
    }

    /// Recompute the grid pages + footer for the current mode + screen width.
    /// `width = "single"` forces one binding column; every other width gives
    /// the grid a text budget and lets it auto-pack columns within it.
    fn rebuild(&mut self) {
        let entries = merge_keybinds(
            &self.keybinds,
            self.mode,
            self.base_mode,
            &self.config.labels,
            &self.config.mode_labels,
            &self.config.groups,
        );
        let (screen_w, screen_h) = self.screen;
        let margin = self.config.margin;
        let pad = self.config.padding;
        // Text budget = screen − outer margin − frame border − inner padding.
        let chrome_w = margin.left + margin.right + Self::BORDER_COLS + pad.left + pad.right;
        let avail_inner = screen_w.saturating_sub(chrome_w).max(1);

        let avail_outer = screen_w.saturating_sub(margin.left + margin.right).max(1);
        let fixed_inner_budget = |outer: usize| {
            outer
                .min(avail_outer)
                .saturating_sub(Self::BORDER_COLS + pad.left + pad.right)
                .max(1)
        };
        let (budget, columns) = match self.config.width {
            WidthMode::Single => (avail_inner, Columns::Fixed(1)),
            WidthMode::Fill => (avail_inner, Columns::Auto),
            WidthMode::Percent(percent) => {
                let outer = screen_w.saturating_mul(percent as usize) / 100;
                (fixed_inner_budget(outer), Columns::Auto)
            }
            WidthMode::Fixed(w) => (fixed_inner_budget(w), Columns::Auto),
        };
        // Never pack more body rows than fit on screen: a clipped row stays
        // invisible but still pads the snug key column of the visible rows. The
        // reserved vertical chrome is the outer margin + inner padding; `frame`
        // is our own border plus any residual Zellij overhead.
        let max_rows = geometry::body_row_budget(
            screen_h,
            margin.top + margin.bottom + pad.top + pad.bottom,
            self.frame_rows + Self::BORDER_ROWS,
            self.config.max_height,
            entries.len(),
        );

        self.layout = lay_out(
            &entries,
            budget,
            columns,
            self.config.sort_by,
            max_rows,
            &self.config.binding_separator,
            Some(&self.theme),
        );

        if self.config.debug_log.is_some() {
            let mut report = format!(
                "=== rebuild mode={:?} base={:?} screen={:?} budget={budget} width={:?} columns={columns:?} max_rows={max_rows} pages={} ===\n",
                self.mode,
                self.base_mode,
                self.screen,
                self.config.width,
                self.layout.page_count(),
            );
            report.push_str(&diagnostics(
                &entries,
                budget,
                columns,
                self.config.sort_by,
                max_rows,
                &self.config.binding_separator,
            ));
            self.log(&report);
        }

        if self.page >= self.layout.page_count() {
            self.page = 0;
        }
        self.footer = footer::build(
            &self.keybinds,
            self.base_mode,
            self.page,
            self.layout.page_count(),
            !self.backstack.is_empty(),
            &self.config,
            &self.theme,
        );
    }

    /// Whether the panel should be visible in the current mode. The resting
    /// modes ([`shared_state::RESTING_MODES`]) plus the session base mode are
    /// always hidden.
    fn want_visible(&self) -> bool {
        self.mode != self.base_mode && !shared_state::RESTING_MODES.contains(&self.mode)
    }

    fn is_active_instance(&self) -> bool {
        if self.saw_visible_event {
            return self.zellij_visible;
        }
        self.my_tab == Some(self.active_tab)
    }

    fn can_publish_shared_state(&self) -> bool {
        let can_publish = self.is_active_instance();
        self.state_log(&format!(
            "[state:can-publish] result={} {}\n",
            can_publish,
            self.context()
        ));
        can_publish
    }

    /// Whether the panel should actually be shown right now: the mode wants it,
    /// this instance belongs to the active tab, and the user hasn't manually
    /// toggled it off in shared state.
    fn should_show(&self) -> bool {
        self.want_visible() && self.is_active_instance() && !self.suppressed && !self.other_float
    }

    fn hide_floating_layers_everywhere(&mut self, reason: &str) {
        if self.last_hide_everywhere_generation == Some(self.shared_generation) {
            return;
        }
        self.last_hide_everywhere_generation = Some(self.shared_generation);
        self.floating_layer_hidden = true;

        self.park_which_key_panes(reason);

        if self.tabs.is_empty() {
            self.state_log(&format!(
                "[floating-layer:hide-all] reason={} tab=active-only\n",
                reason
            ));
            self.log_hide_floating_panes_result(None, hide_floating_panes(None));
            return;
        }

        for tab in &self.tabs {
            self.state_log(&format!(
                "[floating-layer:hide-all] reason={} tab_id={} position={}\n",
                reason, tab.tab_id, tab.position
            ));
            self.log_hide_floating_panes_result(
                Some(tab.tab_id),
                hide_floating_panes(Some(tab.tab_id)),
            );
        }
    }

    fn log_hide_floating_panes_result(&self, tab_id: Option<usize>, result: Result<bool, String>) {
        match result {
            Ok(changed) => self.state_log(&format!(
                "[floating-layer:hide-result] tab_id={:?} changed={}\n",
                tab_id, changed
            )),
            Err(err) => self.state_log(&format!(
                "[floating-layer:hide-result] tab_id={:?} error={}\n",
                tab_id, err
            )),
        }
    }

    fn hidden_coordinates() -> FloatingPaneCoordinates {
        FloatingPaneCoordinates::default()
            .with_x_fixed(9999)
            .with_y_fixed(9999)
            .with_width_fixed(1)
            .with_height_fixed(1)
    }

    fn park_which_key_panes(&mut self, reason: &str) {
        let mut panes = Vec::with_capacity(self.which_key_peers.len() + 1);
        panes.push(self.me());
        panes.extend(self.which_key_peers.iter().copied().map(PaneId::Plugin));
        panes.sort_unstable();
        panes.dedup();

        self.state_log(&format!(
            "[floating-layer:park-peers] reason={} panes={:?}\n",
            reason, panes
        ));
        for pane in &panes {
            set_floating_pane_pinned(*pane, false);
        }
        change_floating_panes_coordinates(
            panes
                .into_iter()
                .map(|pane| (pane, Self::hidden_coordinates()))
                .collect(),
        );
    }

    fn apply_visibility(&mut self) {
        if !self.ready {
            self.state_log("[visibility:skip] not ready\n");
            return;
        }
        let active_instance = self.is_active_instance();
        if !self.want_visible() {
            if !active_instance {
                self.state_log("[visibility:dismiss:skip] passive instance\n");
            } else if self.any_other_float {
                // Another floating pane (a plugin/dialog the user opened, a
                // toggled floating terminal, or the search dialog) shares the
                // tab's floating layer. Hiding the layer would hide *their*
                // pane too, so only park our own pane and leave the layer — and
                // their float — untouched. This keeps which-key decoupled.
                self.park_which_key_panes("dismissed-other-float");
            } else {
                self.hide_floating_layers_everywhere("dismissed");
            }
        } else if self.suppressed && active_instance {
            // Manual hide only needs to park which-key itself. Hiding the whole
            // floating layer makes the next manual unhide call ShowFloatingPanes,
            // which can visibly restore the layer before our coordinates settle.
            self.park_which_key_panes("suppressed");
        }
        let should_show = self.should_show();
        self.state_log(&format!(
            "[visibility] should_show={} before={}\n",
            should_show,
            self.context()
        ));
        if should_show {
            let me = self.me();
            if !self.visible {
                set_selectable(false);
            }
            // Re-assert non-selectable on every reveal. The pane stays alive as
            // an off-screen float, so revealing it should only resize/reposition
            // it, never make it a focus target.
            set_selectable(false);
            if !self.borderless {
                // Drop Zellij's frame (and its SCROLL/PIN indicators); we draw
                // our own rounded frame with just the mode symbol as title.
                set_pane_borderless(me, true);
                self.borderless = true;
                self.frame_rows = 0;
                self.frame_cols = 0;
            }
            self.reposition();
            if !self.visible {
                if !self.reveal_pending {
                    self.reveal_pending = true;
                    self.state_log(&format!(
                        "[visibility:show:pending] pane={me:?} {}\n",
                        self.context()
                    ));
                    set_timeout(0.016);
                }
                return;
            }
            // Pin within our own tab so the panel stays visible while focus moves
            // between this tab's tiled panes. Pinning is per-tab in Zellij, which
            // is fine now that each tab has its own instance — the pin never
            // leaks onto another tab. (Pinning does *not* cause the tab-focus
            // snap; our own focus-stealing did, hence the show path no longer
            // refocuses.) Cross-tab visibility comes from the per-tab instances,
            // not from one pinned float spanning tabs.
            if !self.pinned {
                set_floating_pane_pinned(me, true);
                self.pinned = true;
            }
            // Do NOT refocus here. Unconditionally focusing our origin on every
            // reveal is what broke tab switching: when a reveal lands during tab
            // navigation, `focus_pane_with_id` yanks the active tab back to our
            // home tab. Focus return is now handled *reactively* in the
            // `PaneUpdate` handler, gated on us actually holding focus *and* our
            // origin being on the currently active tab — so we never pull focus
            // to a pane on another tab.
        } else if self.visible {
            self.state_log("[visibility:hide] begin_hide\n");
            self.begin_hide();
        } else {
            self.reveal_pending = false;
            self.state_log("[visibility:no-op] already hidden\n");
        }
    }

    fn finish_reveal(&mut self) {
        let me = self.me();
        self.state_log(&format!(
            "[visibility:show:finish] pane={me:?} {}\n",
            self.context()
        ));
        // Only ever *show* the shared floating layer when we are the sole float.
        // If another float already shares the tab the layer is necessarily
        // visible, so toggling it is both redundant and harmful — it could pull
        // focus onto the layer and away from the user's pane. We just reposition
        // our own parked pane into view (it's a live float already).
        if !self.any_other_float && (!self.revealed_once || self.floating_layer_hidden) {
            let _ = show_floating_panes(self.my_tab_id.or(self.active_tab_id));
        }
        self.floating_layer_hidden = false;
        if !self.revealed_once {
            show_pane_with_id(me, true, false);
            self.revealed_once = true;
        }
        self.visible = true;
        if !self.pinned {
            set_floating_pane_pinned(me, true);
            self.pinned = true;
        }
    }

    /// Tear down the floating panel by parking it as a 1x1 float. We do not
    /// suppress it: suppressed panes do not reliably receive future mode updates,
    /// which is exactly what this plugin is driven by.
    fn begin_hide(&mut self) {
        self.state_log(&format!("[hide] before={}\n", self.context()));
        set_selectable(false);
        set_pane_borderless(self.me(), true);
        set_floating_pane_pinned(self.me(), false);
        self.pinned = false;
        self.visible = false;
        self.reveal_pending = false;
        self.park_hidden();
        self.state_log(&format!("[hide] after={}\n", self.context()));
    }

    fn park_hidden(&mut self) {
        self.state_log("[park] x=9999 y=9999 w=1 h=1\n");
        self.req_height = 1;
        self.req_width = 1;
        change_floating_panes_coordinates(vec![(self.me(), Self::hidden_coordinates())]);
    }

    /// Recompute and apply the floating rectangle from the anchor/padding config
    /// and the current screen + content size.
    fn reposition(&mut self) {
        let (sw, sh) = self.screen;
        if sw == 0 || sh == 0 {
            return;
        }
        let cs = self.content_size();
        let Rect {
            x,
            y,
            width,
            height,
        } = geometry::place(
            self.screen,
            cs,
            self.config.width,
            self.config.anchor,
            self.config.margin,
        );
        self.log(&format!(
            "[reposition] mode={:?} screen={:?} content_size={:?} margin={:?} pad={:?} frame_rows={} anchor={:?} => x={x} y={y} w={width} h={height} (bottom_row={})\n",
            self.mode,
            self.screen,
            cs,
            self.config.margin,
            self.config.padding,
            self.frame_rows,
            self.config.anchor,
            y + height,
        ));
        // Remember the outer size requested so `calibrate_frame` can derive the
        // real Zellij frame overhead (rows + cols) from the interior it reports.
        self.req_height = height;
        self.req_width = width;
        change_floating_panes_coordinates(vec![(
            self.me(),
            FloatingPaneCoordinates::default()
                .with_x_fixed(x)
                .with_y_fixed(y)
                .with_width_fixed(width)
                .with_height_fixed(height),
        )]);
    }

    /// Natural *outer* panel size `(width, height)` in cells, including the
    /// Zellij frame, interior side padding, and (when present) the separator +
    /// footer rows.
    ///
    /// Snugs to the **current** page: its row count and (for `width = "single"`)
    /// its own content width, so a page of single-key bindings isn't padded out
    /// to some other page's long chord. The width never drops below the footer
    /// (`Footer::width`) so the close/scroll hints always draw in full. Paging
    /// therefore resizes the pane — see the `pipe` handler, which repositions.
    fn content_size(&self) -> (usize, usize) {
        let pad = self.config.padding;
        let footer_rows = if self.footer.is_some() { 2 } else { 0 };
        let footer_w = self.footer.as_ref().map(|f| f.width()).unwrap_or(0);

        let (body_rows, body_w) = if self.layout.pages.is_empty() {
            (1, NO_BINDINGS.chars().count())
        } else {
            let page = self.page.min(self.layout.pages.len() - 1);
            let rows = self.layout.pages[page].len().max(1);
            let w = self
                .layout
                .page_widths
                .get(page)
                .copied()
                .unwrap_or(self.layout.content_width);
            (rows, w)
        };
        // The footer floors the width; for `Fill`/`Percent`/`Fixed` width,
        // `geometry::place` overrides this anyway, so the per-page width only
        // bites under `Single`.
        let inner = body_w.max(footer_w);

        (
            inner + pad.left + pad.right + Self::BORDER_COLS + self.frame_cols,
            body_rows + footer_rows + pad.top + pad.bottom + Self::BORDER_ROWS + self.frame_rows,
        )
    }

    /// Learn Zellij's real frame overhead from the interior size it hands to
    /// `render`: `overhead = requested_outer − interior`, in both rows and
    /// columns. We size the pane to `content + overhead`, so the interior ends
    /// up exactly fitting our content regardless of whether Zellij frames us.
    /// While Zellij still draws a frame the overhead is 2 (a border each side);
    /// once `set_pane_borderless` lands the interior equals what we asked for,
    /// so the overhead converges to 0.
    ///
    /// Calibrating columns (not just rows) is what stops a Zellij-drawn frame
    /// from stealing 2 columns and pushing the description onto the right
    /// border. While any frame remains we keep re-asserting borderless, since
    /// the one-shot call in `apply_visibility` can be dropped before the pane
    /// exists. Converges in one extra frame.
    fn calibrate_frame(&mut self, interior_rows: usize, interior_cols: usize) {
        self.log(&format!(
            "[calibrate] visible={} interior=({interior_cols}, {interior_rows}) req=({}, {}) frame=({}, {})\n",
            self.visible, self.req_width, self.req_height, self.frame_cols, self.frame_rows,
        ));
        if !self.visible
            || interior_rows == 0
            || interior_cols == 0
            || self.req_height == 0
            || self.req_width == 0
        {
            return;
        }
        let over_rows = self.req_height.saturating_sub(interior_rows);
        let over_cols = self.req_width.saturating_sub(interior_cols);
        // Zellij is still framing us — its frame ate rows/cols. Keep nudging the
        // pane borderless until the overhead drops to zero.
        if over_rows > 0 || over_cols > 0 {
            set_pane_borderless(self.me(), true);
        }
        if over_rows != self.frame_rows || over_cols != self.frame_cols {
            self.log(&format!(
                "[calibrate] -> frame ({}, {}) -> ({over_cols}, {over_rows}); rebuilding\n",
                self.frame_cols, self.frame_rows
            ));
            self.frame_rows = over_rows;
            self.frame_cols = over_cols;
            self.rebuild();
            self.reposition();
        }
    }

    fn tab_refs(&self) -> Vec<TabRef> {
        self.tabs
            .iter()
            .map(|tab| TabRef {
                position: tab.position,
                tab_id: tab.tab_id,
            })
            .collect()
    }

    fn panes_for_tab<'a>(
        &self,
        manifest: &'a PaneManifest,
        position: usize,
        tab_id: Option<usize>,
    ) -> Option<&'a Vec<PaneInfo>> {
        let keys: Vec<usize> = manifest.panes.keys().copied().collect();
        let tab_refs = self.tab_refs();
        resolve_tab_key(&keys, &tab_refs, position, tab_id).and_then(|key| manifest.panes.get(&key))
    }

    fn active_panes<'a>(&self, manifest: &'a PaneManifest) -> Option<&'a Vec<PaneInfo>> {
        self.panes_for_tab(manifest, self.active_tab, self.active_tab_id)
    }

    /// Capture the originating terminal: the focused, non-plugin, non-floating
    /// pane in the active tab. It keeps `is_focused` while our floating pane
    /// holds focus, so this resolves to the pane the user came from.
    fn detect_origin(&mut self, manifest: &PaneManifest) {
        // Scope to the active tab (from TabUpdate). Prefer the focused terminal;
        // fall back to any terminal in this tab, so a freshly-focused tab whose
        // terminal isn't yet flagged `is_focused` still re-homes the origin here
        // (rather than leaving a stale origin from the previous tab).
        let Some(panes) = self.active_panes(manifest) else {
            return;
        };
        let pick = panes
            .iter()
            .find(|p| p.is_focused && !p.is_plugin && !p.is_floating && !p.is_suppressed)
            .or_else(|| {
                panes
                    .iter()
                    .find(|p| !p.is_plugin && !p.is_floating && !p.is_suppressed)
            });
        if let Some(pane) = pick {
            self.origin = Some(PaneId::Terminal(pane.id));
        }
    }

    /// Whether the captured origin pane lives in the currently active tab.
    fn origin_in_active_tab(&self, manifest: &PaneManifest) -> bool {
        let Some(origin) = self.origin else {
            return false;
        };
        self.active_panes(manifest)
            .map(|panes| {
                panes
                    .iter()
                    .any(|p| !p.is_plugin && PaneId::Terminal(p.id) == origin)
            })
            .unwrap_or(false)
    }

    /// Learn which tab this instance lives on: the tab whose panes contain our
    /// own plugin pane. Recomputed each `PaneUpdate` so tab reordering/closing
    /// keeps `my_tab` (a position) in step with the manifest + `active_tab`.
    fn detect_my_tab(&mut self, manifest: &PaneManifest) {
        let my_id = get_plugin_ids().plugin_id;
        for tab in &self.tabs {
            let Some(panes) = self.panes_for_tab(manifest, tab.position, Some(tab.tab_id)) else {
                continue;
            };
            if panes.iter().any(|p| p.is_plugin && p.id == my_id) {
                self.my_tab = Some(tab.position);
                self.my_tab_id = Some(tab.tab_id);
                return;
            }
        }
        for (tab, panes) in &manifest.panes {
            if panes.iter().any(|p| p.is_plugin && p.id == my_id) {
                self.my_tab = Some(*tab);
                self.my_tab_id = None;
                return;
            }
        }
    }

    fn detect_which_key_peers(&mut self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        // All three roles share one wasm URL, so sibling which-key instances are
        // identified by the stable pane title ([`PANE_TITLE`]) rather than URL.
        let mut peers: Vec<u32> = manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| pane.is_plugin && pane.id != my_id && pane.title == PANE_TITLE)
            .map(|pane| pane.id)
            .collect();
        peers.sort_unstable();
        peers.dedup();
        if self.which_key_peers == peers {
            return false;
        }
        self.state_log(&format!(
            "[peers] old={:?} new={:?}\n",
            self.which_key_peers, peers
        ));
        self.which_key_peers = peers;
        true
    }

    /// Whether our floating pane is currently materialized on any tab.
    fn self_float_is_shown(&self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        manifest.panes.values().flatten().any(|p| {
            p.is_plugin
                && p.id == my_id
                && p.is_floating
                && !p.is_suppressed
                // A *parked* (hidden) panel keeps `is_floating` and is never
                // suppressed (see `begin_hide`), so it would otherwise read as
                // "shown" here. Distinguish it by its 1x1 parking size: only a
                // real, on-screen rect counts. Without this, the backstop below
                // re-runs `begin_hide` on every PaneUpdate while another float
                // (e.g. the search dialog) keeps the floating layer visible,
                // and each hide emits a fresh PaneUpdate — a CPU-pinning loop.
                && p.pane_columns > 1
        })
    }

    /// Whether *we* hold focus **on the active tab**. Scoped to the active tab
    /// on purpose: a pinned/stale copy of our pane on another tab can linger
    /// with `is_focused` set, and counting that would make us fight for focus
    /// (refocus loop) while the user is elsewhere. We only care when we're the
    /// focused pane on the tab the user is actually looking at.
    fn self_is_focused(&self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        self.active_panes(manifest)
            .map(|panes| {
                panes
                    .iter()
                    .any(|p| p.is_plugin && p.id == my_id && p.is_focused)
            })
            .unwrap_or(false)
    }

    fn active_terminal_is_focused(&self, manifest: &PaneManifest) -> bool {
        self.active_panes(manifest)
            .map(|panes| {
                panes
                    .iter()
                    .any(|p| !p.is_plugin && !p.is_floating && !p.is_suppressed && p.is_focused)
            })
            .unwrap_or(false)
    }

    /// Whether some *other* floating pane is present (shown) in the active tab.
    /// We key off **presence**, not focus: when the visual-search dialog and our
    /// panel are both floating in `Search` mode, whoever momentarily holds the
    /// floating-layer focus flaps, so a focus test is unstable and we'd fight it
    /// for focus. Presence is steady — the dialog's pane exists for its whole
    /// lifetime — so we simply yield (stay hidden) while any other float is up.
    /// We exclude all which-key panes and any suppressed (hidden) panes.
    fn other_float_present(&self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        // We only yield to *our own* sibling float — the search dialog — never to
        // unrelated floating plugins the user happens to run (e.g. a context-keys
        // pane). All our roles share one wasm URL, so identify siblings by the URL
        // of our own pane in the manifest. If we can't resolve it, we err toward
        // showing (treat nothing as a blocking float) rather than staying hidden.
        let my_url = manifest
            .panes
            .values()
            .flatten()
            .find(|p| p.is_plugin && p.id == my_id)
            .and_then(|p| p.plugin_url.clone());
        let Some(my_url) = my_url else {
            return false;
        };
        self.active_panes(manifest)
            .map(|panes| {
                panes.iter().any(|p| {
                    p.is_floating
                        && !p.is_suppressed
                        && p.is_plugin
                        // Only our own roles count: a foreign floating plugin must
                        // never suppress us.
                        && p.plugin_url.as_deref() == Some(my_url.as_str())
                        && p.id != my_id
                        && !self.which_key_peers.contains(&p.id)
                        // Ignore *parked* 1x1 floats. The per-tab search dialog
                        // sits parked (floating, not suppressed) until revealed,
                        // so without this it would read as a live "other float"
                        // and permanently suppress us. A real dialog is wider
                        // than its 1x1 parking rect (search reveals at >= 20
                        // cols), so width is the reliable park/shown signal.
                        && p.pane_columns > 1
                })
            })
            .unwrap_or(false)
    }

    /// Whether *any* other floating pane shares the active tab's floating layer:
    /// a foreign plugin (session-manager, configuration, about, layout-manager,
    /// plugin-manager, share, …), a toggled floating terminal, or our own search
    /// dialog. Unlike [`Self::other_float_present`] — which gates *visibility*
    /// and intentionally yields only to our sibling search dialog so a foreign
    /// float can't suppress us — this drives the floating-*layer* and focus
    /// decisions. While another float is up we must not hide the shared layer
    /// (that would hide their pane), not re-show it (redundant, and a focus
    /// hazard), and not pull focus back to our origin terminal (that would steal
    /// focus from their pane). Parked 1x1 floats (ours, or a dormant search
    /// dialog) are excluded by the width test.
    fn any_other_float_present(&self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        self.active_panes(manifest)
            .map(|panes| {
                panes.iter().any(|p| {
                    p.is_floating
                        && !p.is_suppressed
                        && p.pane_columns > 1
                        // Exclude our own which-key panes (this instance and its
                        // per-tab peers); everything else — foreign plugins,
                        // floating terminals, the search dialog — counts.
                        && !(p.is_plugin
                            && (p.id == my_id || self.which_key_peers.contains(&p.id)))
                })
            })
            .unwrap_or(false)
    }

    fn refocus_origin(&self) {
        match self.origin {
            Some(id) => focus_pane_with_id(id, false, false),
            None => focus_previous_pane(),
        }
    }
}
