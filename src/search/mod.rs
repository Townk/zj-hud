//! Floating "visual search" pane.
//!
//! This is the *second* role of the plugin binary (see `main.rs`'s `Plugin`
//! dispatch). Like the which-key panel, it is spawned **per-tab** by the layout
//! (`floating_panes { … role "search" }`) and parked offscreen on startup, so
//! every tab has its own ready instance. A floating pane belongs to exactly one
//! tab, so a single shared instance could only ever appear on the tab it was
//! first revealed on — hence one per tab. Each instance stays parked until the
//! client enters `EnterSearch`, at which point **only the active tab's
//! instance** reveals (`activate`) and grabs the keyboard; the others ignore the
//! mode change. A keybind only needs `SwitchToMode "EnterSearch"` (e.g. `Alt+/`,
//! Ghostty's
//! Cmd+F); we react to the resulting `ModeUpdate`. On activation we immediately
//! flip the client to **`Normal`** — counter-intuitively the *only* mode in
//! which `intercept_key_presses` actually delivers keys to us. In
//! `EnterSearch`/`Search` zellij's native input layer consumes keystrokes
//! before any plugin grab can see them, so our custom text field would never
//! receive input there (verified empirically). The status bar still shows a
//! Search indicator: the search pane writes a `search_active` flag into the
//! shared session state (`publish_search_state`) and broadcasts it, since the
//! indicator can no longer be inferred from the (now `Normal`) client mode.
//!
//! ## Key capture
//!
//! We call [`intercept_key_presses`] on `activate`: while the dialog is open it
//! captures every key as [`Event::InterceptedKeyPress`] (the launch key
//! included, so `Alt+/` doesn't re-fire), and we release the grab with
//! [`clear_key_presses_intercepts`] when the search finishes. The grab is set
//! once and never re-asserted (re-asserting forces the client mode and was a
//! past source of churn); it persists for the dialog's lifetime.
//!
//! ## Origin pane
//!
//! We capture the originating terminal's [`PaneId`] from the focused pane at
//! activation (before we reveal our float). The dialog is then revealed
//! *pinned and unfocused*, so the origin terminal keeps focus the whole time —
//! native search acts on the focused pane, so we drive it directly with no
//! focus bounce. On teardown we re-park the pane offscreen (a 1x1 float) rather
//! than suppress it: like which-key, a suppressed pane does not reliably keep
//! receiving the `ModeUpdate`s this dialog is driven by — and with one instance
//! per tab that unreliability would mean a tab's dialog silently never opens.
//! Parking keeps the instance live and ready to be re-revealed by the next
//! `EnterSearch` on its tab.
//!
//! ## Live search
//!
//! Typing runs a *trailing-debounced* (`SEARCH_DEBOUNCE`) live search: we push
//! the term to the (already-focused) origin's native search and re-apply the
//! options — no focus bounce, no mode change. Because zellij's `clear_search`
//! resets all search options on every `SearchInput`, the case / whole-word
//! toggles (configurable `search { case_key; word_key }`, default `Alt+c` /
//! `Alt+b`) and wrap (`Alt+p`, seeded from the shared state) are re-applied
//! after the needle each time
//! (see `apply_search_options`).
//!
//! ## Persistence
//!
//! The pane is a persistent instance (shown/hidden per search). To make the
//! *last submitted* term reappear pre-filled the next time it opens, we persist
//! it to a temp file and read it back asynchronously on each `activate`.
//!
//! All buffer/cursor/edit/unicode bookkeeping is delegated to `tui_input`; the
//! only glue we own is the `KeyWithModifier -> InputRequest` mapping and the
//! rendering. On <Enter> we commit the term to Zellij's *native* search and
//! hand control to native `Search` mode (releasing our grab, parking the
//! dialog): native `n`/`N` navigate, <Esc> drops to `Normal`, and a `backspace`
//! keybind re-enters `EnterSearch` to reopen this dialog (pre-filled). Because
//! `Search` is a non-resting mode and our float is parked, the which-key panel
//! reveals there. On <Esc> from the field (before submit) we cancel straight
//! back to the launching mode (`origin_mode`, e.g. `Scroll`).

use std::collections::BTreeMap;
use unicode_width::UnicodeWidthChar;
use zellij_tile::prelude::actions::{Action, SearchDirection, SearchOption};
use zellij_tile::prelude::*;

use crate::shared::geometry::{place, Anchor, Padding, WidthMode};
use crate::shared::icons;

/// Title we give the floating pane via `rename_plugin_pane`, purely cosmetic.
/// (The status bar now lights its Search indicator from the client mode, not
/// from this pane's presence — see `render::build_right_side`.)
pub const PANE_TITLE: &str = "Search";

/// Shared (session-agnostic) file holding the last submitted term, read back on
/// the next launch to pre-fill the field. Not session-scoped: the launching
/// keybind opens us directly (no bar involvement) so we don't know the session
/// name, and sharing the last search term across sessions is harmless.
const SEARCH_FILE: &str = "/tmp/zj-hud-search";

/// `RunCommandResult` context tag for the prefill read.
const CTX_KEY: &str = "ctx";
const CTX_PREFILL: &str = "search_prefill";

/// Default dialog geometry and chrome, applied to our own floating pane on
/// setup. `PANE_WIDTH` is the default width; the live width comes from the
/// bar-authored `search { … }` block (see [`SearchGeom`]). The height is fixed
/// (single field row between a top/bottom chrome row) and not configurable.
const PANE_WIDTH: usize = 40;
const PANE_HEIGHT: usize = 3;
/// Columns reserved to the right of the input area (toggles + chrome). The
/// input's right edge is `width - RIGHT_INSET`; keeping this fixed lets the
/// dialog scale with the configured width while preserving its layout. Equals
/// the historical `PANE_WIDTH(40) - INPUT_END_COL(35)`.
const RIGHT_INSET: usize = 5;
/// Smallest width that still leaves a usable input area.
const MIN_WIDTH: usize = 20;

/// Placement geometry for the search dialog, authored once on the bar as a
/// `search { anchor "…"; width N; margin "t,r,b,l" }` block and forwarded
/// through the shared state (`SharedState::search_config`). Mirrors the
/// which-key panel's config-driven geometry. The height is fixed (the dialog is
/// a single input row); only `anchor`, `width`, and `margin` are configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchGeom {
    anchor: Anchor,
    margin: Padding,
    width: usize,
}

impl Default for SearchGeom {
    fn default() -> Self {
        Self {
            // Bottom-right, one column from the right edge and one row above the
            // status bar — reproduces the original hardcoded placement.
            anchor: Anchor {
                v: crate::shared::geometry::VAlign::Bottom,
                h: crate::shared::geometry::HAlign::Right,
            },
            margin: Padding {
                top: 0,
                right: 1,
                bottom: 1,
                left: 0,
            },
            width: PANE_WIDTH,
        }
    }
}

impl SearchGeom {
    /// Parse the forwarded `search { … }` block. Unknown/missing keys keep their
    /// default; reuses the which-key anchor/padding parsers for a single syntax.
    fn from_block(block: &str) -> Self {
        let mut geom = SearchGeom::default();
        let Some(doc) = crate::shared::kdl::parse_config_document(block, &[]) else {
            return geom;
        };
        if let Some(spec) = doc
            .get_arg("anchor")
            .map(crate::shared::kdl::kdl_value_to_config_string)
        {
            geom.anchor = crate::whichkey::config::parse_anchor(&spec);
        }
        if let Some(spec) = doc
            .get_arg("margin")
            .map(crate::shared::kdl::kdl_value_to_config_string)
        {
            geom.margin = crate::whichkey::config::parse_padding(&spec);
        }
        if let Some(w) = doc.get_arg("width").and_then(|v| v.as_i64()) {
            if w > 0 {
                geom.width = (w as usize).max(MIN_WIDTH);
            }
        }
        geom
    }

    /// Column where the input text area ends (0-indexed), derived from the width.
    fn input_end_col(&self) -> usize {
        self.width
            .saturating_sub(RIGHT_INSET)
            .max(INPUT_COL + TOGGLE_W + 2)
    }
}

/// A single modifier+letter chord (e.g. `Alt+b`), used for the dialog's
/// in-field search-option toggles. Only modifier+letter combos are supported —
/// enough for the toggles and unambiguous against typed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyChord {
    alt: bool,
    ctrl: bool,
    key: char,
}

impl KeyChord {
    /// Parse a chord spec like `"Alt b"` / `"Ctrl+b"` (Zellij/which-key style:
    /// space- or `+`-separated, modifiers then a single letter). `Shift` is
    /// folded into the letter and otherwise ignored. Returns `None` if no
    /// single-character key token is present.
    fn parse(spec: &str) -> Option<Self> {
        let mut alt = false;
        let mut ctrl = false;
        let mut key = None;
        for tok in spec
            .split(|c: char| c.is_whitespace() || c == '+')
            .filter(|s| !s.is_empty())
        {
            match tok.to_ascii_lowercase().as_str() {
                "alt" | "opt" | "option" => alt = true,
                "ctrl" | "control" => ctrl = true,
                "shift" => {}
                _ => {
                    let mut chars = tok.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => key = Some(c.to_ascii_lowercase()),
                        _ => return None,
                    }
                }
            }
        }
        key.map(|key| KeyChord { alt, ctrl, key })
    }

    /// Whether a received key event matches this chord (letters compared
    /// case-insensitively; the exact Alt/Ctrl set must match).
    fn matches(&self, key: &KeyWithModifier) -> bool {
        let alt = key.key_modifiers.contains(&KeyModifier::Alt);
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        alt == self.alt
            && ctrl == self.ctrl
            && matches!(key.bare_key, BareKey::Char(c) if c.eq_ignore_ascii_case(&self.key))
    }
}

/// The dialog's configurable search-option toggle chords, authored on the bar
/// as `search { case_key "…"; word_key "…"; wrap_key "…" }` and forwarded
/// through the shared state. Defaults: case = `Alt+c`, whole-word = `Alt+b`
/// ("boundaries"), wrap = `Alt+p` (matching the native `Search`-mode bind, so
/// the same chord toggles wrap whether the dialog is up or not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchKeys {
    case: KeyChord,
    word: KeyChord,
    wrap: KeyChord,
}

impl Default for SearchKeys {
    fn default() -> Self {
        Self {
            case: KeyChord {
                alt: true,
                ctrl: false,
                key: 'c',
            },
            word: KeyChord {
                alt: true,
                ctrl: false,
                key: 'b',
            },
            wrap: KeyChord {
                alt: true,
                ctrl: false,
                key: 'p',
            },
        }
    }
}

impl SearchKeys {
    /// Parse `case_key` / `word_key` / `wrap_key` from the forwarded
    /// `search { … }` block. Missing or unparseable specs keep their default.
    fn from_block(block: &str) -> Self {
        let mut keys = SearchKeys::default();
        let Some(doc) = crate::shared::kdl::parse_config_document(block, &[]) else {
            return keys;
        };
        if let Some(spec) = doc
            .get_arg("case_key")
            .map(crate::shared::kdl::kdl_value_to_config_string)
            .and_then(|s| KeyChord::parse(&s))
        {
            keys.case = spec;
        }
        if let Some(spec) = doc
            .get_arg("word_key")
            .map(crate::shared::kdl::kdl_value_to_config_string)
            .and_then(|s| KeyChord::parse(&s))
        {
            keys.word = spec;
        }
        if let Some(spec) = doc
            .get_arg("wrap_key")
            .map(crate::shared::kdl::kdl_value_to_config_string)
            .and_then(|s| KeyChord::parse(&s))
        {
            keys.wrap = spec;
        }
        keys
    }
}

/// Background `#282C41` as RGB. `set_pane_color` isn't honored for plugin panes
/// here, so we paint every cell with this truecolor escape in `render` instead.
const BG_RGB: (u8, u8, u8) = (0x28, 0x2C, 0x41);
/// Search mode color (`#61afef`, matching the layout's `modes { search ... }`),
/// used as the left border's foreground.
const SEARCH_RGB: (u8, u8, u8) = (0x61, 0xAF, 0xEF);
/// Background of the input text area (`#181825`), a touch darker than the pane.
const INPUT_BG_RGB: (u8, u8, u8) = (0x18, 0x18, 0x25);
/// Normal theme background (`#1E1E2E`), used behind the left border glyph.
const THEME_BG_RGB: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);
/// `┃` left border drawn on column 0 of every row.
const BORDER_CHAR: char = '┃';

/// Search-option indicator glyphs, shown at the right end of the input area:
/// case-sensitivity, whole-word, and result wrap.
const GLYPH_CASE: char = '\u{EAB1}';
const GLYPH_WORD: char = '\u{EB7E}';
const GLYPH_WRAP: char = '\u{F0547}';
/// Indicator foreground when the option is OFF (dark grey) and ON (yellow).
const TOGGLE_OFF_RGB: (u8, u8, u8) = (0x45, 0x47, 0x5A);
const TOGGLE_ON_RGB: (u8, u8, u8) = (0xF9, 0xE2, 0xAF);
/// Columns reserved at the end of the input area for the three indicators:
/// case, a space, word, two spaces (extra gap before wrap), wrap.
const TOGGLE_W: usize = 6;

// Decorative frame glyphs wrapping the input area. Drawn with the input bg as
// foreground over the pane bg, so the partial-block shapes blend the input box
// into a rounded inset rectangle. Corners are Symbols-for-Legacy-Computing.
const BOX_TL: char = '𜺠';
const BOX_TR: char = '𜺣';
const BOX_BL: char = '𜺫';
const BOX_BR: char = '𜺨';
const BOX_TOP: char = '▂';
const BOX_BOT: char = '🮂';
const BOX_LEFT: char = '▐';
const BOX_RIGHT: char = '▌';

// Layout, all 0-indexed (column 0 == leftmost). CSI positioning below adds 1.
/// Row carrying the glyph + input (the dialog's middle line).
const FIELD_ROW: usize = 1;
/// Column of the search glyph.
const GLYPH_COL: usize = 2;
/// First column of the input text.
const INPUT_COL: usize = 5;

/// Debounce before a keystroke triggers a live (search-as-you-type) search.
const SEARCH_DEBOUNCE: f64 = 0.2;

/// One decoded keystroke's effect on the search field.
enum KeyAct {
    /// Mutate the input buffer (insert / move / delete).
    Edit(tui_input::InputRequest),
    /// Commit the term to native search and close.
    Submit,
    /// Abandon the search and close.
    Cancel,
}

#[derive(Default)]
pub struct SearchPane {
    input: tui_input::Input,
    /// The terminal pane to search / return focus to, detected from the pane
    /// manifest (the focused non-plugin pane in the active tab — it stays
    /// `is_focused` even while our floating pane holds focus).
    origin: Option<PaneId>,
    /// Whether permission-gated setup (intercept, rename, prefill) has run.
    ready: bool,
    /// Whether the dialog is currently shown (we've entered `EnterSearch`).
    /// While `false` we are a hidden background plugin ignoring most events.
    /// Once a search is submitted we hand off to native `Search` mode and are no
    /// longer `active` — the dialog parks and only re-activates on the next
    /// `EnterSearch` (e.g. the `backspace`→reopen keybind from `Search` mode).
    active: bool,
    /// The client's current input mode, tracked from `ModeUpdate` so we know
    /// when `EnterSearch` is entered (activate) or left (deactivate).
    mode: InputMode,
    /// Where a cancel (<Esc>, or empty submit) returns the client, captured when
    /// `EnterSearch` is entered (see [`SearchPane::cancel_target`]). For a fresh
    /// open it's the launching mode (e.g. `Scroll`); when reopened from native
    /// `Search` it's the mode behind `Search` (backstack top) or `Normal`, so
    /// <Esc> exits search instead of looping back into it.
    origin_mode: InputMode,
    /// Last (x, y) we anchored to, to avoid redundant repositioning calls.
    anchored: Option<(usize, usize)>,
    /// Set once we've observed our own pane focused, so a later loss of focus
    /// (e.g. a click on another floating pane) can be treated as a cancel.
    was_focused: bool,
    /// Set once we've observed the floating layer visible. If it later hides
    /// (the user clicked a tiled pane), we treat that as a cancel.
    seen_visible: bool,
    /// Set while we tear ourselves down, to ignore the focus changes we cause.
    closing: bool,
    /// The last term we pushed to native search, so the debounced live search
    /// is a no-op when nothing changed.
    last_searched: Option<String>,
    /// Outstanding debounce timers. Each edit schedules one; the live search
    /// only fires when the count drains to zero, i.e. once typing has paused
    /// for `SEARCH_DEBOUNCE` (a trailing debounce, not a per-key throttle).
    pending_searches: usize,
    /// Case-sensitive matching toggle (see `keys.case`). Off ⇒ case-insensitive.
    case_sensitive: bool,
    /// Whole-word matching toggle (see `keys.word`).
    whole_word: bool,
    /// Result-wrapping toggle (see `keys.wrap`). Defaults on (set in `load`),
    /// reproducing the dialog's historical always-wrap behaviour.
    term_wrap: bool,
    /// Optional debug-log path (from the `debug_log` config key).
    debug_log: Option<String>,
    /// Session name, tracked from `ModeUpdate` so we can write the shared
    /// `search_active` flag to the same session-scoped state file the bar reads.
    session_name: String,
    /// Placement geometry, refreshed from the bar's forwarded `search { … }`
    /// block on each activation (see `load_geom`).
    geom: SearchGeom,
    /// Configurable in-field toggle chords (case / whole-word), refreshed from
    /// the same `search { … }` block on each activation (see `load_geom`).
    keys: SearchKeys,
    /// Whether permission-gated startup has run (we parked our layout-spawned
    /// floating pane offscreen). Guards `ensure_parked` against re-running.
    granted: bool,
    /// Position of the currently active tab, tracked from `TabUpdate`. Paired
    /// with [`Self::my_tab`] to decide whether *this* per-tab instance owns the
    /// next search (only the active tab's instance reveals + grabs the keyboard).
    active_tab: usize,
    /// This instance's home tab position, learned from the manifest (the tab
    /// whose panes contain our own plugin pane). `None` until first seen. We are
    /// one of several per-tab instances spawned by the layout; like the
    /// which-key panel, each only acts while its home tab is the active tab.
    my_tab: Option<usize>,
}

impl ZellijPlugin for SearchPane {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.debug_log = configuration.get("debug_log").cloned();
        // Wrap is on by default (the dialog historically always wrapped); the
        // `wrap_key` chord toggles it off. The other options default off.
        self.term_wrap = true;
        // Identical set for both roles so the per-URL permission cache stays
        // stable (see `crate::PLUGIN_PERMISSIONS`). We actually use
        // `RunActionsAsUser` (native search), `ChangeApplicationState` (mode
        // switches + focus), `RunCommands` (prefill file), `InterceptInput`
        // (grab keystrokes) and `ReadApplicationState` (read the manifest to
        // find our origin pane); the rest are the bar's and harmless here.
        request_permission(crate::PLUGIN_PERMISSIONS);
        subscribe(&[
            EventType::Key,
            EventType::InterceptedKeyPress,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::Timer,
            EventType::ModeUpdate,
        ]);

        // Inert while parked: NOT selectable, so the background instance never
        // joins the focus rotation (a selectable hidden pane fights tab/pane
        // focus). We flip to selectable on `activate` (so the cursor renders in
        // the field) and back off on teardown. The layout spawns us as a small
        // floating pane; `ensure_parked` (on permission grant) parks it offscreen
        // so it stays hidden until the client enters `EnterSearch` on our tab.
        set_selectable(false);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(_) => {
                self.ensure_parked();
                true
            }
            // The dialog is mode-driven. `EnterSearch` is the *trigger*; on
            // entering it we record where we came from (`origin_mode`) and
            // `activate`, which flips the client to `Normal` (the only mode in
            // which our intercept receives keys). We never auto-tear-down on a
            // mode change — the only mode changes while we're up are ones we
            // drive; teardown happens explicitly in submit/close/finish.
            Event::ModeUpdate(info) => {
                let new = info.mode;
                // Track the session name so `publish_search_state` writes to the
                // same session-scoped state file the bar reads.
                if let Some(name) = info.session_name {
                    self.session_name = name;
                }
                self.log(&format!(
                    "[ModeUpdate] new={new:?} prev={:?} active={}\n",
                    self.mode, self.active
                ));
                // Every per-tab instance receives this client-global mode
                // change, but only the one on the active tab may reveal and grab
                // the keyboard — otherwise N instances would fight over the
                // (session-global) key intercept. The rest just record the mode.
                if new == InputMode::EnterSearch && !self.active && self.is_on_active_tab() {
                    self.origin_mode = self.cancel_target();
                    self.activate();
                }
                self.mode = new;
                false
            }
            // While shown, PaneUpdate lets us (a) capture the origin pane,
            // (b) re-anchor, (c) detect a click-away cancel, and (d) re-assert
            // the key grab after a live-search focus bounce. Ignored while
            // hidden so the background instance stays inert.
            Event::PaneUpdate(manifest) => {
                // Learn which tab we live on even while parked — the activation
                // gate (`is_on_active_tab`) depends on it.
                self.detect_my_tab(&manifest);
                if !self.active {
                    return false;
                }
                let my_id = get_plugin_ids().plugin_id;
                let self_focused = manifest
                    .panes
                    .values()
                    .flatten()
                    .find(|p| p.is_plugin && p.id == my_id)
                    .map(|p| p.is_focused);
                self.log(&format!(
                    "[PaneUpdate] active={} mode={:?} self_focused={self_focused:?} zellij_focus={:?}\n",
                    self.active,
                    self.mode,
                    get_focused_pane_info().ok(),
                ));
                self.detect_origin(&manifest);
                self.anchor(&manifest);
                self.check_focus(&manifest);
                // NB: we deliberately do NOT re-assert `intercept_key_presses`
                // here — the single grab from `activate` suffices and persists
                // (the pinned dialog never bounces focus), and re-asserting
                // perturbs the client mode unnecessarily.
                true
            }
            Event::TabUpdate(tabs) => {
                if let Some(active) = tabs.iter().find(|t| t.active) {
                    self.active_tab = active.position;
                }
                // If the user switched tabs while the dialog was open, our home
                // tab is no longer active: tear down (release the intercept +
                // restore the mode) so we don't keep grabbing keys from another
                // tab's terminal. Otherwise reconcile visibility. (Once a search
                // is submitted we hand off to native `Search` mode and are no
                // longer `active`, so tab switches then need no cleanup here.)
                if self.active && !self.is_on_active_tab() {
                    self.close();
                } else if self.active {
                    self.check_visible(&tabs);
                }
                false
            }
            Event::Timer(_) => {
                // Only the last of a burst of debounce timers triggers a search.
                if self.pending_searches > 0 {
                    self.pending_searches -= 1;
                    if self.pending_searches == 0 {
                        self.live_search();
                    }
                }
                true
            }
            Event::Key(key) => {
                self.log(&format!(
                    "[Key] active={} mode={:?} zellij_focus={:?} key={key:?}\n",
                    self.active,
                    self.mode,
                    get_focused_pane_info().ok(),
                ));
                if self.active {
                    self.handle_key(key)
                } else {
                    false
                }
            }
            Event::InterceptedKeyPress(key) => {
                self.log(&format!(
                    "[InterceptedKeyPress] active={} mode={:?} zellij_focus={:?} key={key:?}\n",
                    self.active,
                    self.mode,
                    get_focused_pane_info().ok(),
                ));
                if self.active {
                    self.handle_key(key)
                } else {
                    false
                }
            }
            Event::RunCommandResult(exit_code, stdout, _stderr, context)
                if context.get(CTX_KEY).map(String::as_str) == Some(CTX_PREFILL) =>
            {
                self.apply_prefill(exit_code, &stdout)
            }
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.render_field(rows, cols);
    }
}

impl SearchPane {
    /// Append a line to the debug log when `debug_log` is configured. Written
    /// host-side via `run_command` (the plugin's WASI sandbox can't reach an
    /// arbitrary host path) so the file lives at the real configured path.
    fn log(&self, msg: &str) {
        if let Some(path) = &self.debug_log {
            // `$1` = message, `$2` = path — both argv, never interpolated.
            run_command(
                &["sh", "-c", "printf '%s' \"$1\" >> \"$2\"", "sh", msg, path],
                BTreeMap::new(),
            );
        }
    }

    /// This instance's own plugin pane id.
    fn me(&self) -> PaneId {
        PaneId::Plugin(get_plugin_ids().plugin_id)
    }

    /// Whether this per-tab instance lives on the currently active tab. Until a
    /// `PaneUpdate` has resolved our home tab we answer `false`: better to miss
    /// one activation than to let a background tab's instance grab the keyboard.
    fn is_on_active_tab(&self) -> bool {
        self.my_tab == Some(self.active_tab)
    }

    /// Drive a native `Search`-mode option toggle and mirror it to the bar. The
    /// `search`-mode keybind sends `MessagePlugin "status-bar" { role "search" }`
    /// to this pipe (payload `case`/`word`/`wrap`). We own the search options, so
    /// we flip the matching flag, run the native `SearchToggleOption` against the
    /// focused pane, and publish so the bar's Search-mode hint tracks it — a
    /// plugin can't otherwise observe native `SearchToggleOption`. The pipe
    /// reaches every per-tab Search instance; only the active tab's acts, since
    /// both the action and the flag are toggles (multiple would net the wrong
    /// parity). Matching is exact and our config is just `role "search"`, so the
    /// pipe lands here rather than spawning a fresh instance.
    pub(crate) fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name != crate::shared::state::SEARCH_TOGGLE_PIPE
            || !self.is_on_active_tab()
        {
            return false;
        }
        let option = match pipe_message.payload.as_deref().map(str::trim) {
            Some("case") => {
                self.case_sensitive = !self.case_sensitive;
                SearchOption::CaseSensitivity
            }
            Some("word") => {
                self.whole_word = !self.whole_word;
                SearchOption::WholeWord
            }
            Some("wrap") => {
                self.term_wrap = !self.term_wrap;
                SearchOption::Wrap
            }
            _ => return false,
        };
        run_action(Action::SearchToggleOption { option }, BTreeMap::new());
        self.publish_search_state(self.active);
        false
    }

    /// Where a cancel (<Esc>) should land, captured when the dialog opens.
    ///
    /// Normally that's the mode we launched from (e.g. `Normal`, `Scroll`). But
    /// when we're reopened from native `Search` mode (the `backspace` keybind),
    /// returning to `Search` would loop the user back into the results they were
    /// trying to leave. Instead <Esc> must exit search entirely: drop to the
    /// mode *behind* `Search` — the top of the shared mode-trail (backstack), or
    /// `Normal` when the trail is empty. The bar freezes the trail across our
    /// `Normal` excursion, so by the time we're in `Search` the backstack still
    /// reflects how the user got there.
    fn cancel_target(&self) -> InputMode {
        if matches!(self.mode, InputMode::Search | InputMode::EnterSearch) {
            self.backstack_top().unwrap_or(InputMode::Normal)
        } else {
            self.mode
        }
    }

    /// Top of the shared mode-trail (the mode `Search` would unwind to), read
    /// fresh from the session state file the bar maintains.
    fn backstack_top(&self) -> Option<InputMode> {
        let path =
            crate::shared::state::state_path(get_plugin_ids().zellij_pid, &self.session_name);
        crate::shared::state::read_state_from(&path)
            .ok()
            .and_then(|s| s.backstack().last().copied())
    }

    /// Learn which tab we live on: the tab whose panes contain our own plugin
    /// pane. `manifest.panes` is keyed by tab position — the same space as
    /// `active_tab` (from `TabUpdate`) — so the two compare directly.
    fn detect_my_tab(&mut self, manifest: &PaneManifest) {
        let my_id = get_plugin_ids().plugin_id;
        for (tab, panes) in &manifest.panes {
            if panes.iter().any(|p| p.is_plugin && p.id == my_id) {
                self.my_tab = Some(*tab);
                return;
            }
        }
    }

    /// One-time startup (on the permission grant — pane-shaping commands are
    /// no-ops before it): drop Zellij's frame and park our layout-spawned float
    /// offscreen so it stays hidden until the first `activate`. Mirrors the
    /// which-key panel's `ensure_ready`.
    fn ensure_parked(&mut self) {
        if self.granted {
            return;
        }
        self.granted = true;
        set_pane_borderless(self.me(), true);
        set_selectable(false);
        show_pane_with_id(self.me(), false, false);
        self.park();
    }

    /// Park the pane as a 1x1 float in the far corner: invisible, but still a
    /// live (non-suppressed) floating pane so it keeps receiving the
    /// `ModeUpdate`s the dialog is driven by. Used for the idle state and on
    /// teardown. Clears `anchored` so the next reveal always re-positions.
    fn park(&mut self) {
        set_floating_pane_pinned(self.me(), false);
        self.anchored = None;
        change_floating_panes_coordinates(vec![(
            self.me(),
            FloatingPaneCoordinates::default()
                .with_x_fixed(9999)
                .with_y_fixed(9999)
                .with_width_fixed(1)
                .with_height_fixed(1),
        )]);
    }

    /// Reveal the dialog and run its per-open setup: reset the field/flags,
    /// become selectable (so the cursor renders), shape and reveal our hidden
    /// background pane (borderless, fixed size, floating, *pinned + unfocused*),
    /// grab keystrokes, flip the client to `Normal`, raise the bar's Search
    /// indicator, and kick off the prefill read.
    ///
    /// We hold the client in `Normal` while typing because it is the only mode
    /// in which `intercept_key_presses` delivers keys to us — `EnterSearch`/
    /// `Search` consume them natively first. The bar's Search indicator is
    /// driven explicitly (see `publish_search_state`) rather than from the
    /// client mode. On cancel (<Esc>) we return to `origin_mode`; on submit
    /// (<Enter>) we hand off to native `Search` mode (see `submit`).
    fn activate(&mut self) {
        self.active = true;
        self.ready = true;
        self.closing = false;
        self.was_focused = false;
        self.seen_visible = false;
        self.last_searched = None;
        self.pending_searches = 0;
        self.anchored = None;
        self.input = tui_input::Input::default();
        // Refresh placement from the bar's forwarded `search { … }` block. Read
        // here (not via a pipe) since geometry only matters while the dialog is
        // up; the bar has long since published it by the first search.
        self.load_geom();

        // Capture the origin terminal *now*, before we steal focus: at this
        // point (just entered EnterSearch via a keybind) the focused pane is
        // still the terminal we want to search. Once we focus our own float the
        // per-layer focus model makes this ambiguous, so detecting it here is
        // far more reliable than scraping the manifest afterwards.
        if let Ok((tab, focused)) = get_focused_pane_info() {
            if let PaneId::Terminal(_) = focused {
                self.origin = Some(focused);
            }
            self.log(&format!(
                "[activate] origin_mode={:?} focused_tab={tab} focused={focused:?} origin={:?}\n",
                self.origin_mode, self.origin
            ));
        } else {
            self.log("[activate] get_focused_pane_info failed\n");
        }

        let pane = PaneId::Plugin(get_plugin_ids().plugin_id);
        rename_plugin_pane(get_plugin_ids().plugin_id, PANE_TITLE);
        // Borderless + fixed size + custom background: shape the dialog from the
        // plugin itself. `anchor` repositions it bottom-right on PaneUpdate.
        set_pane_borderless(pane, true);
        change_floating_panes_coordinates(vec![(
            pane,
            FloatingPaneCoordinates::default()
                .with_width_fixed(self.geom.width)
                .with_height_fixed(PANE_HEIGHT),
        )]);
        // Order matters. The pane is a *suppressed* background instance (loaded
        // via `load_plugins`); `set_selectable`/focus are no-ops until it's
        // actually materialised in the tab. So:
        //   1. reveal it (unsuppress + float) WITHOUT focusing — focusing here
        //      fails with "pane is not selectable" because a freshly-revealed
        //      plugin pane defaults to non-selectable,
        //   2. mark it selectable now that it exists in the tab (also lets the
        //      real terminal cursor render in-field), then
        //   3. focus it explicitly with the dedicated FocusPluginPane command.
        // Reveal the dialog *without* focusing it (unsuppress + float, leaving
        // focus on the origin terminal), mark it selectable, then PIN it.
        //
        // This is the crux of the whole feature. A normal floating pane is only
        // visible while a floating pane holds focus, so keeping the dialog up
        // used to force us to focus it — but native search only acts on the
        // *focused* pane, so every keystroke had to bounce focus to the origin
        // terminal and back, and that bounce kept knocking the client out of
        // `Search` into `Normal` (and ping-ponged focus). Pinned, the dialog
        // stays visible on top while the origin terminal keeps focus the whole
        // time: search runs against it directly (no bounce, no mode churn), and
        // keys still reach us through the client-global `intercept_key_presses`
        // grab regardless of focus. The dialog draws its own cursor, so it needs
        // no real focus to look active.
        show_pane_with_id(pane, true, false);
        set_selectable(true);
        set_floating_pane_pinned(pane, true);
        // Order is critical: `intercept_key_presses` neutralises the client's
        // mode bindings (so every key reaches us regardless of focus), which
        // drops the client to `Normal`. Assert it FIRST, then set `Search` LAST
        // so the indicator/mode-trail land on `Search`. We do NOT re-assert the
        // intercept afterwards (see PaneUpdate) — each re-assert would re-drop
        // the mode to `Normal`. The single grab here persists for the dialog's
        // lifetime because we never bounce focus.
        intercept_key_presses();
        // CRUCIAL: hold the client in `Normal`. `intercept_key_presses` only
        // delivers `InterceptedKeyPress` while the client is in `Normal` — in
        // `EnterSearch`/`Search` the native input layer consumes the keys first
        // and our dialog never sees them (proven empirically). So our custom
        // text field can only work in `Normal`. The bar still shows the Search
        // indicator: we write the shared `search_active` flag (`publish_search_state`)
        // because it can no longer be read from the (now `Normal`) client mode.
        switch_to_input_mode(&InputMode::Normal);
        self.publish_search_state(true);
        self.log(
            "[activate] revealed(unfocused) + pinned + intercept; mode->Normal + bar:search=1\n",
        );
        self.request_prefill();
    }

    /// Publish the search-dialog on/off state plus the current option toggles to
    /// the shared session state, so the visible bar can light its Search
    /// indicator and render the option hint (case / word / wrap). We keep the
    /// *client* mode in `Normal` while the dialog is up (the only mode in which
    /// `intercept_key_presses` delivers keys to us), so the bar can't infer
    /// "search active" from the mode and reads these fields instead. Writing the
    /// shared file (and broadcasting it) keeps a single cross-role state
    /// contract rather than a dedicated indicator pipe. The option flags persist
    /// across the dialog parking, so the hint stays accurate in native `Search`.
    fn publish_search_state(&self, active: bool) {
        let path =
            crate::shared::state::state_path(get_plugin_ids().zellij_pid, &self.session_name);
        if let Some(state) =
            crate::shared::state::mutate_state_file(&path, get_plugin_ids().plugin_id, |s| {
                s.search_active = active;
                s.search_case_sensitive = self.case_sensitive;
                s.search_whole_word = self.whole_word;
                s.search_wrap = self.term_wrap;
            })
        {
            if let Ok(payload) = serde_json::to_string(&state) {
                pipe_message_to_plugin(
                    MessageToPlugin::new(crate::shared::state::SYNC_PIPE).with_payload(payload),
                );
            }
        }
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        // Configurable search-option toggles are matched first (they're
        // modifier chords, so they never collide with the readline/typing keys
        // decoded below). Toggling forces a re-run with the new option even
        // though the term itself is unchanged (hence `last_searched = None`).
        if self.keys.case.matches(&key) {
            self.case_sensitive = !self.case_sensitive;
            self.last_searched = None;
            self.live_search();
            self.publish_search_state(true);
            return true;
        }
        if self.keys.word.matches(&key) {
            self.whole_word = !self.whole_word;
            self.last_searched = None;
            self.live_search();
            self.publish_search_state(true);
            return true;
        }
        if self.keys.wrap.matches(&key) {
            self.term_wrap = !self.term_wrap;
            self.last_searched = None;
            self.live_search();
            self.publish_search_state(true);
            return true;
        }
        match decode_key(&key) {
            Some(KeyAct::Edit(req)) => {
                self.input.handle(req);
                self.schedule_search();
                true
            }
            Some(KeyAct::Submit) => {
                self.submit();
                false
            }
            Some(KeyAct::Cancel) => {
                self.close();
                false
            }
            None => false,
        }
    }

    /// Arm a trailing debounce: bump the pending count and set a timer. The
    /// search only runs once the count drains back to zero (see the Timer arm).
    fn schedule_search(&mut self) {
        self.pending_searches += 1;
        set_timeout(SEARCH_DEBOUNCE);
    }

    /// Re-apply our search options to the origin. Must run *after* every
    /// `SearchInput`, because zellij's `update_search_term` calls `clear_search`
    /// (resetting `search_results` to its default: case-sensitive, no
    /// whole-word, no wrap). `SearchToggleOption` flips the corresponding grid
    /// flag, so from that known default we toggle on exactly what we want.
    fn apply_search_options(&self) {
        // Grid default is case-sensitive (`case_insensitive = false`), so toggle
        // when the user wants insensitive matching.
        if !self.case_sensitive {
            run_action(
                Action::SearchToggleOption {
                    option: SearchOption::CaseSensitivity,
                },
                BTreeMap::new(),
            );
        }
        if self.whole_word {
            run_action(
                Action::SearchToggleOption {
                    option: SearchOption::WholeWord,
                },
                BTreeMap::new(),
            );
        }
        // The grid default is wrap-off, so toggle only when the user wants it on.
        if self.term_wrap {
            run_action(
                Action::SearchToggleOption {
                    option: SearchOption::Wrap,
                },
                BTreeMap::new(),
            );
        }
    }

    /// Search-as-you-type: when the debounce fires and the term changed, push
    /// it to the origin's native search. The origin already holds focus (our
    /// dialog is pinned, not focused) so there's no focus bounce and no mode
    /// change. Emptying the field clears the highlight.
    fn live_search(&mut self) {
        if self.closing || !self.ready {
            return;
        }
        let term = self.input.value().to_string();
        if self.last_searched.as_deref() == Some(term.as_str()) {
            return;
        }
        self.last_searched = Some(term.clone());
        let Some(origin) = self.origin else {
            self.log(&format!("[live_search] no origin; term={term:?} dropped\n"));
            return;
        };
        self.log(&format!("[live_search] origin={origin:?} term={term:?}\n"));
        // Drive native search against the already-focused origin (the dialog is
        // pinned, not focused). We do NOT touch the client mode here: it stays
        // `Normal` so our intercept keeps receiving keys. `SearchInput` itself
        // async-resets the client to `Normal` anyway, which is exactly what we
        // want now. Reset the buffer first, push the term + options, then run
        // the `Search` nav so the match is highlighted/jumped to.
        run_action(Action::SearchInput { input: vec![0] }, BTreeMap::new());
        if !term.is_empty() {
            run_action(
                Action::SearchInput {
                    input: term.into_bytes(),
                },
                BTreeMap::new(),
            );
            self.apply_search_options();
        }
        run_action(
            Action::Search {
                direction: SearchDirection::Down,
            },
            BTreeMap::new(),
        );
        self.log("[live_search] done (Normal, bar:search=1)\n");
    }

    /// Commit the term to native search, then enter the *results-navigation*
    /// phase by handing control to Zellij's *native* `Search` mode.
    ///
    /// On <Enter> we commit the term, release our key grab, park the dialog, and
    /// switch the client into real `Search` mode. From there native bindings
    /// drive navigation (`n`/`N`), <Esc> drops to `Normal`, and a `backspace`
    /// keybind re-enters `EnterSearch` to reopen this dialog (pre-filled). Two
    /// things fall out for free: the bar shows `Search` from the real client
    /// mode (no `search_active` flag needed), and the which-key panel reveals,
    /// since `Search` is a non-resting mode and our dialog float is now parked.
    ///
    /// An empty submit has nothing to commit, so it falls through to `close`. We
    /// are persistent, so on close/reopen we re-park the pane rather than close
    /// it; the next `EnterSearch` re-activates us, pre-filled.
    fn submit(&mut self) {
        let term = self.input.value().to_string();
        if term.is_empty() {
            self.close();
            return;
        }
        self.persist_term(&term);
        // The origin already holds focus (dialog is pinned), so the committed
        // search targets it directly. Reset the buffer first so a re-search
        // doesn't append to the previous term.
        run_action(Action::SearchInput { input: vec![0] }, BTreeMap::new());
        run_action(
            Action::SearchInput {
                input: term.into_bytes(),
            },
            BTreeMap::new(),
        );
        // Preserve the case / whole-word / wrap toggles (SearchInput reset them).
        self.apply_search_options();
        run_action(
            Action::Search {
                direction: SearchDirection::Down,
            },
            BTreeMap::new(),
        );
        // Hand off to native `Search` mode: release the grab and park our pane,
        // then switch the client into `Search`. We clear `search_active` because
        // the bar now reads the indicator straight from the real client mode,
        // and parking the float lets the which-key panel reveal for `Search`.
        self.active = false;
        self.closing = false;
        clear_key_presses_intercepts();
        self.publish_search_state(false);
        set_selectable(false);
        self.park();
        switch_to_input_mode(&InputMode::Search);
        self.log("[submit] committed; -> native Search mode (grab released)\n");
    }

    /// Cancel: drop the grab, re-focus the origin, clear the live-search
    /// highlight, return the client to the mode the search was launched from
    /// (`origin_mode` — e.g. `Scroll`, or `Normal` when launched from there),
    /// and hide. The persisted term is left untouched so a later open still
    /// pre-fills it.
    fn close(&mut self) {
        self.closing = true;
        self.active = false;
        clear_key_presses_intercepts();
        self.refocus_origin();
        // Clear the live-search highlight: with the origin focused, reset its
        // search buffer and re-search the now-empty term.
        if self.origin.is_some() {
            run_action(Action::SearchInput { input: vec![0] }, BTreeMap::new());
            run_action(
                Action::Search {
                    direction: SearchDirection::Down,
                },
                BTreeMap::new(),
            );
        }
        self.publish_search_state(false);
        switch_to_input_mode(&self.origin_mode);
        set_selectable(false);
        self.park();
    }

    /// Treat losing focus (e.g. a click on another pane) as a cancel. We only
    /// act once we've seen ourselves focused, and skip while already closing so
    /// the focus shift we cause on teardown doesn't re-enter.
    fn check_focus(&mut self, manifest: &PaneManifest) {
        if self.closing {
            return;
        }
        let my_id = get_plugin_ids().plugin_id;
        let me = manifest
            .panes
            .values()
            .flatten()
            .find(|p| p.is_plugin && p.id == my_id);
        match me {
            Some(p) if p.is_focused => self.was_focused = true,
            Some(_) if self.was_focused => self.close(),
            _ => {}
        }
    }

    /// Treat hiding the floating layer as a cancel. Clicking a tiled pane keeps
    /// our pane `is_focused` (focus is per-layer) but flips the active tab's
    /// `are_floating_panes_visible` to false — so `check_focus` can't see it,
    /// but this can. We only act after the layer has actually been shown.
    fn check_visible(&mut self, tabs: &[TabInfo]) {
        if self.closing || !self.ready {
            return;
        }
        let visible = tabs
            .iter()
            .find(|t| t.active)
            .map(|t| t.are_floating_panes_visible)
            .unwrap_or(false);
        if visible {
            self.seen_visible = true;
        } else if self.seen_visible {
            self.close();
        }
    }

    /// Return focus to the originating terminal. We focus it explicitly by id
    /// (captured from the manifest) rather than `focus_previous_pane`, which
    /// walks chronological order and lands on the wrong pane.
    fn refocus_origin(&self) {
        if let Some(id) = self.origin {
            focus_pane_with_id(id, false, false);
        } else {
            // No manifest seen yet: best-effort fall back to the prior pane.
            focus_previous_pane();
        }
    }

    /// Anchor the dialog to the bottom-right: `RIGHT_MARGIN` empty columns to
    /// the screen edge and `BOTTOM_GAP` empty rows above the status bar. The
    /// status bar is the bottommost unselectable plugin pane (a UI bar); we
    /// position relative to it so we sit just above it regardless of screen size.
    /// Refresh placement geometry from the bar's forwarded `search { … }` block
    /// in the shared session state. Falls back to defaults when absent.
    fn load_geom(&mut self) {
        let path =
            crate::shared::state::state_path(get_plugin_ids().zellij_pid, &self.session_name);
        let shared = crate::shared::state::read_state_from(&path).unwrap_or_default();
        self.geom = SearchGeom::from_block(&shared.search_config);
        self.keys = SearchKeys::from_block(&shared.search_config);
        // Seed the option flags from the shared state so the dialog reflects (and
        // preserves) toggles made elsewhere (the dialog's own `keys.wrap`/`case`/
        // `word`, or — if wired — a native `Search`-mode toggle mirrored to the
        // bar). Without this, reopening the dialog would clobber the prior state.
        self.case_sensitive = shared.search_case_sensitive;
        self.whole_word = shared.search_whole_word;
        self.term_wrap = shared.search_wrap;
        self.log(&format!(
            "[geom] {:?} keys={:?} case={} word={} wrap={}\n",
            self.geom, self.keys, self.case_sensitive, self.whole_word, self.term_wrap
        ));
    }

    fn anchor(&mut self, manifest: &PaneManifest) {
        let Ok((tab, _focused)) = get_focused_pane_info() else {
            self.log("[anchor] get_focused_pane_info failed\n");
            return;
        };
        let Some(panes) = manifest.panes.get(&tab) else {
            self.log(&format!("[anchor] no panes for tab {tab}\n"));
            return;
        };
        let Some(status) = panes
            .iter()
            .filter(|p| p.is_plugin && !p.is_selectable && !p.is_floating)
            .max_by_key(|p| p.pane_y)
        else {
            self.log("[anchor] no status-bar pane found\n");
            return;
        };
        // Place within the area *above* the status bar: width spans to the bar's
        // right edge, height is the rows above it. A `bottom` anchor then sits
        // just above the bar (the original behavior), `top` hugs the screen top.
        let screen_w = status.pane_x + status.pane_columns;
        let screen_h = status.pane_y;
        let rect = place(
            (screen_w, screen_h),
            (self.geom.width, PANE_HEIGHT),
            WidthMode::Fixed(self.geom.width),
            self.geom.anchor,
            self.geom.margin,
        );
        let (x, y) = (rect.x, rect.y);
        self.log(&format!(
            "[anchor] tab={tab} status(x={},y={},cols={}) screen=({screen_w},{screen_h}) -> x={x} y={y} prev={:?}\n",
            status.pane_x, status.pane_y, status.pane_columns, self.anchored
        ));
        if self.anchored == Some((x, y)) {
            return;
        }
        self.anchored = Some((x, y));
        change_floating_panes_coordinates(vec![(
            PaneId::Plugin(get_plugin_ids().plugin_id),
            FloatingPaneCoordinates::default()
                .with_x_fixed(x)
                .with_y_fixed(y)
                .with_width_fixed(rect.width)
                .with_height_fixed(rect.height),
        )]);
    }

    /// Capture the originating terminal from a pane manifest: the focused,
    /// non-plugin, non-floating pane in the active tab. It keeps `is_focused`
    /// while our floating dialog holds focus (focus is tracked per surface), so
    /// this resolves to the terminal the user launched us from.
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

    /// Kick off an async read of the last-term file to pre-fill the field.
    fn request_prefill(&self) {
        let mut ctx = BTreeMap::new();
        ctx.insert(CTX_KEY.to_string(), CTX_PREFILL.to_string());
        // `$1` is the path; passing it as an argv keeps the path out of the
        // script text (no shell injection).
        run_command(
            &["sh", "-c", "cat \"$1\" 2>/dev/null", "sh", SEARCH_FILE],
            ctx,
        );
    }

    /// Apply the prefill read result. Only fills when the user hasn't started
    /// typing yet, so a slow read can't clobber in-progress input.
    fn apply_prefill(&mut self, exit_code: Option<i32>, stdout: &[u8]) -> bool {
        if exit_code != Some(0) || !self.input.value().is_empty() {
            return false;
        }
        let term = String::from_utf8_lossy(stdout)
            .trim_end_matches('\n')
            .to_string();
        if term.is_empty() {
            return false;
        }
        self.input = tui_input::Input::new(term);
        // Re-highlight the prefilled term on open: leave `last_searched` unset
        // so the debounced live search treats it as new and runs once.
        self.last_searched = None;
        self.schedule_search();
        true
    }

    /// Persist the submitted term so the next launch can pre-fill it.
    fn persist_term(&self, term: &str) {
        // `$1` = term, `$2` = path — both argv, never interpolated.
        run_command(
            &[
                "sh",
                "-c",
                "printf '%s' \"$1\" > \"$2\"",
                "sh",
                term,
                SEARCH_FILE,
            ],
            BTreeMap::new(),
        );
    }

    fn render_field(&mut self, rows: usize, cols: usize) {
        let input_end_col = self.geom.input_end_col();
        let (r, g, b) = BG_RGB;
        let bg = format!("\u{1b}[48;2;{r};{g};{b}m");
        let (br, bg_, bb) = SEARCH_RGB;
        let border_fg = format!("\u{1b}[38;2;{br};{bg_};{bb}m");
        let (tr, tg, tb) = THEME_BG_RGB;
        let theme_bg = format!("\u{1b}[48;2;{tr};{tg};{tb}m");
        let reset = "\u{1b}[0m";
        let blank = " ".repeat(cols);
        let rows = rows.max(1);

        // Absolute CSI positioning (`ESC[row;colH`, 1-indexed) rather than
        // `\r\n`. Paint the background first, then the chrome on top.
        let mut out = String::new();

        // Background fill, then the left border (theme bg behind it) on every row.
        for row in 1..=rows {
            out.push_str(&format!("\u{1b}[{row};1H{bg}{blank}"));
            out.push_str(&format!(
                "\u{1b}[{row};1H{theme_bg}{border_fg}{BORDER_CHAR}{reset}"
            ));
        }

        // Search glyph on the field row.
        out.push_str(&format!(
            "\u{1b}[{};{}H{bg}{}{reset}",
            FIELD_ROW + 1,
            GLYPH_COL + 1,
            icons::MODE_SEARCH
        ));

        // Decorative frame around the input area, input-bg glyphs over pane bg.
        let (fr, fg2, fb) = INPUT_BG_RGB;
        let box_fg = format!("\u{1b}[38;2;{fr};{fg2};{fb}m");
        // One extra interior column past the text area: a painted empty block.
        let interior = input_end_col - INPUT_COL + 2;
        let box_left = INPUT_COL.saturating_sub(1);
        let box_right = input_end_col + 2;
        let top_csi = FIELD_ROW; // 0-indexed (FIELD_ROW - 1) + 1
        let mid_csi = FIELD_ROW + 1;
        let bot_csi = FIELD_ROW + 2;
        out.push_str(&format!(
            "\u{1b}[{top_csi};{}H{bg}{box_fg}{BOX_TL}{}{BOX_TR}{reset}",
            box_left + 1,
            BOX_TOP.to_string().repeat(interior),
        ));
        out.push_str(&format!(
            "\u{1b}[{bot_csi};{}H{bg}{box_fg}{BOX_BL}{}{BOX_BR}{reset}",
            box_left + 1,
            BOX_BOT.to_string().repeat(interior),
        ));
        out.push_str(&format!(
            "\u{1b}[{mid_csi};{}H{bg}{box_fg}{BOX_LEFT}{reset}",
            box_left + 1,
        ));
        out.push_str(&format!(
            "\u{1b}[{mid_csi};{}H{bg}{box_fg}{BOX_RIGHT}{reset}",
            box_right + 1,
        ));

        // The input area is split: text on the left, then `TOGGLE_W` columns at
        // the right end for the option indicators.
        let area_w = input_end_col - INPUT_COL + 1;
        // One extra column past the toggles is left as a gap, shrinking the text
        // field by one so it never butts up against the indicators.
        let field_w = area_w.saturating_sub(TOGGLE_W + 1).max(1);
        let scroll = self.input.visual_scroll(field_w);
        let shown = skip_columns(self.input.value(), scroll);
        let cursor_col = self.input.visual_cursor().saturating_sub(scroll);

        // Distinct background spanning the whole input area plus the extra block.
        let (ir, ig, ib) = INPUT_BG_RGB;
        let input_bg = format!("\u{1b}[48;2;{ir};{ig};{ib}m");
        out.push_str(&format!(
            "\u{1b}[{};{}H{input_bg}{}{reset}",
            FIELD_ROW + 1,
            INPUT_COL + 1,
            " ".repeat(area_w + 1),
        ));

        // Cursor block painted in the search color. We draw it ourselves because
        // `show_cursor` blanks the pane in this version; after it we restore the
        // input bg so following glyphs keep their background.
        let (sr, sg, sb) = SEARCH_RGB;
        let cursor_on = format!("\u{1b}[48;2;{sr};{sg};{sb}m{box_fg}");
        let mut line = String::with_capacity(shown.len() + 8);
        let mut col = 0usize;
        let mut placed = false;
        for ch in shown.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if col == cursor_col {
                line.push_str(&cursor_on);
                line.push(ch);
                line.push_str(reset);
                line.push_str(&input_bg);
                placed = true;
            } else {
                line.push(ch);
            }
            col += w;
        }
        if !placed {
            line.push_str(&cursor_on);
            line.push(' ');
            line.push_str(reset);
        }

        out.push_str(&format!(
            "\u{1b}[{};{}H{input_bg}{line}{reset}",
            FIELD_ROW + 1,
            INPUT_COL + 1,
        ));

        // Option indicators in the reserved columns at the right end of the
        // input area: case glyph, gap, word glyph, gap, gap, wrap glyph (6
        // columns). Yellow when on, dark grey when off, over the input
        // background. Wrap toggles in-dialog with `keys.wrap` (default Alt+p)
        // and is seeded from the shared state on activate.
        let color = |(r, g, b): (u8, u8, u8)| format!("\u{1b}[38;2;{r};{g};{b}m");
        let c_fg = color(if self.case_sensitive {
            TOGGLE_ON_RGB
        } else {
            TOGGLE_OFF_RGB
        });
        let w_fg = color(if self.whole_word {
            TOGGLE_ON_RGB
        } else {
            TOGGLE_OFF_RGB
        });
        let p_fg = color(if self.term_wrap {
            TOGGLE_ON_RGB
        } else {
            TOGGLE_OFF_RGB
        });
        out.push_str(&format!(
            "\u{1b}[{};{}H{input_bg}{c_fg}{GLYPH_CASE}{reset}{input_bg} {w_fg}{GLYPH_WORD}{reset}{input_bg}  {p_fg}{GLYPH_WRAP}{reset}",
            FIELD_ROW + 1,
            // Anchored to the right end of the input area, independent of field_w.
            input_end_col - TOGGLE_W + 2,
        ));

        print!("{out}");
    }
}

/// Translate a key event into a single search-field action.
///
/// Ctrl/Alt chords are matched before the catch-all `Char` arm so that e.g.
/// Ctrl-a is "go to start" rather than a literal `a`.
fn decode_key(key: &KeyWithModifier) -> Option<KeyAct> {
    use tui_input::InputRequest as R;
    use BareKey::*;

    let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
    let alt = key.key_modifiers.contains(&KeyModifier::Alt);

    let act = match key.bare_key {
        Enter => KeyAct::Submit,
        Esc => KeyAct::Cancel,
        Char('c') if ctrl => KeyAct::Cancel,

        // Readline-style chords.
        Char('a') if ctrl => KeyAct::Edit(R::GoToStart),
        Char('e') if ctrl => KeyAct::Edit(R::GoToEnd),
        Char('b') if ctrl => KeyAct::Edit(R::GoToPrevChar),
        Char('f') if ctrl => KeyAct::Edit(R::GoToNextChar),
        Char('w') if ctrl => KeyAct::Edit(R::DeletePrevWord),
        Char('u') if ctrl => KeyAct::Edit(R::DeleteLine),
        Char('k') if ctrl => KeyAct::Edit(R::DeleteTillEnd),

        // Printable input (Shift is folded into the char already).
        Char(c) if !ctrl && !alt => KeyAct::Edit(R::InsertChar(c)),

        // Cursor movement.
        Left if alt => KeyAct::Edit(R::GoToPrevWord),
        Right if alt => KeyAct::Edit(R::GoToNextWord),
        Left => KeyAct::Edit(R::GoToPrevChar),
        Right => KeyAct::Edit(R::GoToNextChar),
        Home => KeyAct::Edit(R::GoToStart),
        End => KeyAct::Edit(R::GoToEnd),

        // Deletion.
        Backspace => KeyAct::Edit(R::DeletePrevChar),
        Delete => KeyAct::Edit(R::DeleteNextChar),

        _ => return None,
    };
    Some(act)
}

/// Return the suffix of `s` after skipping the first `cols` display columns.
/// Used to implement horizontal scrolling of the input field.
fn skip_columns(s: &str, cols: usize) -> &str {
    if cols == 0 {
        return s;
    }
    let mut acc = 0usize;
    for (i, ch) in s.char_indices() {
        if acc >= cols {
            return &s[i..];
        }
        acc += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bare: BareKey, mods: &[KeyModifier]) -> KeyWithModifier {
        KeyWithModifier {
            bare_key: bare,
            key_modifiers: mods.iter().copied().collect(),
        }
    }

    #[test]
    fn plain_char_inserts() {
        assert!(matches!(
            decode_key(&key(BareKey::Char('x'), &[])),
            Some(KeyAct::Edit(tui_input::InputRequest::InsertChar('x')))
        ));
    }

    #[test]
    fn search_geom_defaults_reproduce_legacy_placement() {
        use crate::shared::geometry::{HAlign, VAlign};
        let g = SearchGeom::default();
        assert_eq!(g.width, PANE_WIDTH);
        assert_eq!(g.anchor.v, VAlign::Bottom);
        assert_eq!(g.anchor.h, HAlign::Right);
        assert_eq!(g.margin.right, 1);
        assert_eq!(g.margin.bottom, 1);
        // Historic INPUT_END_COL was 35 at width 40.
        assert_eq!(g.input_end_col(), 35);
    }

    #[test]
    fn search_geom_from_block_parses_and_clamps() {
        use crate::shared::geometry::{HAlign, VAlign};
        let g = SearchGeom::from_block("anchor \"top+left\"\nwidth 60\nmargin \"2,3,2,3\"");
        assert_eq!(g.anchor.v, VAlign::Top);
        assert_eq!(g.anchor.h, HAlign::Left);
        assert_eq!(g.width, 60);
        assert_eq!(g.margin.top, 2);
        assert_eq!(g.margin.left, 3);
        assert_eq!(g.input_end_col(), 60 - RIGHT_INSET);
        // Width below the floor is clamped up.
        assert_eq!(SearchGeom::from_block("width 5").width, MIN_WIDTH);
        // Empty block yields defaults.
        assert_eq!(SearchGeom::from_block(""), SearchGeom::default());
    }

    #[test]
    fn search_keys_default_to_alt_c_and_alt_b() {
        let k = SearchKeys::default();
        assert!(k
            .case
            .matches(&key(BareKey::Char('c'), &[KeyModifier::Alt])));
        assert!(k
            .word
            .matches(&key(BareKey::Char('b'), &[KeyModifier::Alt])));
        // Empty block keeps defaults.
        assert_eq!(SearchKeys::from_block(""), SearchKeys::default());
    }

    #[test]
    fn search_keys_from_block_overrides_chords() {
        let k = SearchKeys::from_block("case_key \"Ctrl i\"\nword_key \"Alt+w\"");
        assert!(k
            .case
            .matches(&key(BareKey::Char('i'), &[KeyModifier::Ctrl])));
        assert!(k
            .word
            .matches(&key(BareKey::Char('w'), &[KeyModifier::Alt])));
        // The old default chords no longer match once overridden.
        assert!(!k
            .case
            .matches(&key(BareKey::Char('c'), &[KeyModifier::Alt])));
    }

    #[test]
    fn key_chord_parse_and_match_semantics() {
        let c = KeyChord::parse("Alt b").unwrap();
        assert_eq!(c, KeyChord::parse("alt+B").unwrap()); // case- and sep-insensitive
                                                          // Matches Alt+b (any letter case), but not bare b or Ctrl+b.
        assert!(c.matches(&key(BareKey::Char('b'), &[KeyModifier::Alt])));
        assert!(c.matches(&key(BareKey::Char('B'), &[KeyModifier::Alt])));
        assert!(!c.matches(&key(BareKey::Char('b'), &[])));
        assert!(!c.matches(&key(BareKey::Char('b'), &[KeyModifier::Ctrl])));
        // Modifier-only or empty specs have no key → None.
        assert!(KeyChord::parse("Alt").is_none());
        assert!(KeyChord::parse("").is_none());
    }

    #[test]
    fn ctrl_a_goes_to_start_not_insert() {
        assert!(matches!(
            decode_key(&key(BareKey::Char('a'), &[KeyModifier::Ctrl])),
            Some(KeyAct::Edit(tui_input::InputRequest::GoToStart))
        ));
    }

    #[test]
    fn enter_submits_esc_cancels() {
        assert!(matches!(
            decode_key(&key(BareKey::Enter, &[])),
            Some(KeyAct::Submit)
        ));
        assert!(matches!(
            decode_key(&key(BareKey::Esc, &[])),
            Some(KeyAct::Cancel)
        ));
        assert!(matches!(
            decode_key(&key(BareKey::Char('c'), &[KeyModifier::Ctrl])),
            Some(KeyAct::Cancel)
        ));
    }

    #[test]
    fn alt_arrows_move_by_word() {
        assert!(matches!(
            decode_key(&key(BareKey::Left, &[KeyModifier::Alt])),
            Some(KeyAct::Edit(tui_input::InputRequest::GoToPrevWord))
        ));
    }

    #[test]
    fn unmapped_key_is_ignored() {
        assert!(decode_key(&key(BareKey::F(5), &[])).is_none());
    }

    // Drive an `Input` purely through the decoded edit actions. This mirrors
    // what `handle_key` does for the `Edit` case without referencing the
    // submit/close paths (which call wasm host functions and would break the
    // native test link).
    fn apply(input: &mut tui_input::Input, k: &KeyWithModifier) {
        if let Some(KeyAct::Edit(req)) = decode_key(k) {
            input.handle(req);
        }
    }

    #[test]
    fn editing_updates_value_and_cursor() {
        let mut input = tui_input::Input::default();
        for c in "foo".chars() {
            apply(&mut input, &key(BareKey::Char(c), &[]));
        }
        assert_eq!(input.value(), "foo");
        apply(&mut input, &key(BareKey::Char('a'), &[KeyModifier::Ctrl])); // go to start
        apply(&mut input, &key(BareKey::Char('X'), &[]));
        assert_eq!(input.value(), "Xfoo");
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let mut input = tui_input::Input::default();
        for c in "foo bar".chars() {
            apply(&mut input, &key(BareKey::Char(c), &[]));
        }
        apply(&mut input, &key(BareKey::Char('w'), &[KeyModifier::Ctrl]));
        assert_eq!(input.value(), "foo ");
    }

    #[test]
    fn skip_columns_handles_ascii() {
        assert_eq!(skip_columns("hello", 0), "hello");
        assert_eq!(skip_columns("hello", 2), "llo");
        assert_eq!(skip_columns("hello", 10), "");
    }
}
