//! which-key spike
//! ================
//!
//! Proves a floating "which-key" panel that:
//!   * appears when you leave the base mode (press LEADER),
//!   * updates as the mode changes,
//!   * does NOT keep focus (focus returns to the pane you came from),
//!   * survives focus moving between other panes (it is pinned), and
//!   * dismisses when you return to the base mode.
//!
//! Mechanism (all three matter):
//!   * `set_floating_pane_pinned(self, true)` — stay visible on top regardless
//!     of the floating-layer toggle / which pane is focused.
//!   * `set_selectable(false)` — never a focus target, never an action target.
//!   * On reveal we `show_pane_with_id(self, float=true, focus=false)` AND, as a
//!     belt-and-suspenders against the focus grab that `LaunchOrFocusPlugin`
//!     (and some show paths) cause, immediately `focus_pane_with_id(origin)` —
//!     the originating terminal we captured from the pane manifest, exactly like
//!     the search pane does.

use std::collections::BTreeMap;
use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::*;

struct State {
    /// Plugin-level setup has run (idempotent; works on fresh or cached grant).
    ready: bool,
    /// Floating geometry + pin applied once, on the first reveal (a background
    /// plugin has no floating pane to position until then).
    configured: bool,
    /// Whether the panel is currently revealed.
    visible: bool,
    /// Modes in which the panel is dismissed. Configurable via `dismiss_modes`;
    /// defaults to Normal + Locked.
    dismiss_modes: Vec<InputMode>,
    /// The base/default mode (shown in the panel header for context).
    base_mode: InputMode,
    /// Current mode, shown in the panel.
    mode: InputMode,
    /// Live keybindings for `mode`, straight from the user's config.
    keybinds: Vec<(KeyWithModifier, Vec<Action>)>,
    /// The terminal pane to hand focus back to, captured from the manifest.
    origin: Option<PaneId>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ready: false,
            configured: false,
            visible: false,
            dismiss_modes: vec![InputMode::Normal, InputMode::Locked],
            base_mode: InputMode::Normal,
            mode: InputMode::Normal,
            keybinds: Vec::new(),
            origin: None,
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        if let Some(spec) = configuration.get("dismiss_modes") {
            self.dismiss_modes = parse_modes(spec);
        }
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
        ]);
        // Stay selectable until the grant lands so the first-launch permission
        // prompt is shown and approvable; we flip to non-selectable in setup.
        subscribe(&[
            EventType::ModeUpdate,
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.ensure_ready();
                self.apply_visibility();
                true
            }
            Event::PermissionRequestResult(_) => true,
            Event::PaneUpdate(manifest) => {
                // Cached-grant path: no PermissionRequestResult fires, so make
                // sure setup has run. Safe — `ensure_ready` no longer hides.
                self.ensure_ready();
                self.detect_origin(&manifest);
                // If a show/launch left us holding focus, give it back.
                if self.visible && self.self_is_focused(&manifest) {
                    self.refocus_origin();
                }
                false
            }
            Event::ModeUpdate(mode_info) => {
                self.ensure_ready();
                self.base_mode = mode_info.base_mode.unwrap_or(InputMode::Normal);
                self.mode = mode_info.mode;
                self.keybinds = mode_info.get_keybinds_for_mode(self.mode);
                self.apply_visibility();
                true
            }
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, _cols: usize) {
        println!(" which-key (spike)");
        println!(" mode: {:?}   base: {:?}", self.mode, self.base_mode);
        println!();
        let mut shown = 0usize;
        for (key, actions) in &self.keybinds {
            let Some(first) = actions.first() else {
                continue;
            };
            println!("  {:<12} {:?}", key.to_string(), first);
            shown += 1;
            if shown >= 12 {
                println!("  …");
                break;
            }
        }
        if shown == 0 {
            println!("  (no keybindings reported for this mode)");
        }
    }
}

impl State {
    /// Idempotent plugin-level setup: name ourselves and become display-only.
    /// Float geometry + pin are deferred to the first reveal (`apply_visibility`)
    /// because a background-loaded plugin has no floating pane to position yet.
    fn ensure_ready(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        rename_plugin_pane(get_plugin_ids().plugin_id, "which-key");
        set_selectable(false);
    }

    /// Show in any non-base mode (returning focus to origin), hide in base mode.
    fn apply_visibility(&mut self) {
        if !self.ready {
            return;
        }
        let want_visible = !self.dismiss_modes.contains(&self.mode);
        if want_visible && !self.visible {
            let me = PaneId::Plugin(get_plugin_ids().plugin_id);
            // Reveal — floats the (so far paneless) background plugin.
            show_pane_with_id(me, true, false);
            // First reveal only: position + pin now that a floating pane exists.
            if !self.configured {
                change_floating_panes_coordinates(vec![(
                    me,
                    FloatingPaneCoordinates::default()
                        .with_x_fixed(4)
                        .with_y_fixed(2)
                        .with_width_fixed(48)
                        .with_height_fixed(16),
                )]);
                set_floating_pane_pinned(me, true);
                self.configured = true;
            }
            self.visible = true;
            self.refocus_origin();
        } else if !want_visible && self.visible {
            hide_self();
            self.visible = false;
        }
    }

    /// Capture the originating terminal: the focused, non-plugin, non-floating
    /// pane in the active tab. It keeps `is_focused` while our floating pane
    /// holds focus, so this resolves to the pane the user came from.
    fn detect_origin(&mut self, manifest: &PaneManifest) {
        let Ok((tab, _focused)) = get_focused_pane_info() else {
            return;
        };
        if let Some(panes) = manifest.panes.get(&tab) {
            if let Some(pane) = panes
                .iter()
                .find(|p| p.is_focused && !p.is_plugin && !p.is_floating && !p.is_suppressed)
            {
                self.origin = Some(PaneId::Terminal(pane.id));
            }
        }
    }

    fn self_is_focused(&self, manifest: &PaneManifest) -> bool {
        let my_id = get_plugin_ids().plugin_id;
        manifest
            .panes
            .values()
            .flatten()
            .any(|p| p.is_plugin && p.id == my_id && p.is_focused)
    }

    fn refocus_origin(&self) {
        match self.origin {
            Some(id) => focus_pane_with_id(id, false, false),
            None => focus_previous_pane(),
        }
    }
}

/// Parse a `dismiss_modes` spec: mode names separated by whitespace and/or
/// commas, case-insensitive. Unknown names are ignored.
fn parse_modes(spec: &str) -> Vec<InputMode> {
    spec.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(str_to_mode)
        .collect()
}

fn str_to_mode(s: &str) -> Option<InputMode> {
    match s.to_ascii_lowercase().as_str() {
        "normal" => Some(InputMode::Normal),
        "locked" | "lock" => Some(InputMode::Locked),
        "resize" => Some(InputMode::Resize),
        "pane" => Some(InputMode::Pane),
        "tab" => Some(InputMode::Tab),
        "scroll" => Some(InputMode::Scroll),
        "entersearch" => Some(InputMode::EnterSearch),
        "search" => Some(InputMode::Search),
        "renametab" => Some(InputMode::RenameTab),
        "renamepane" => Some(InputMode::RenamePane),
        "session" => Some(InputMode::Session),
        "move" => Some(InputMode::Move),
        "prompt" => Some(InputMode::Prompt),
        "tmux" => Some(InputMode::Tmux),
        _ => None,
    }
}
