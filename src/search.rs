//! Floating "visual search" pane.
//!
//! This is the *second* role of the plugin binary (see `main.rs`'s `Plugin`
//! dispatch). It is loaded headless at session start (via `load_plugins` with
//! `role "search"`) and stays a hidden background pane until the client enters
//! the `EnterSearch` input mode — at which point `activate` reveals it. A
//! keybind only needs `SwitchToMode "EnterSearch"` (e.g. `Alt+/`, Ghostty's
//! Cmd+F); we react to the resulting `ModeUpdate`. On activation we immediately
//! flip the client to `Search` — a keybind mode where our key grab wins, unlike
//! the `EnterSearch` text-input mode whose raw typing would bypass it. Sitting
//! in `Search` (not the base mode) also lets a sibling which-key plugin keep
//! its mode trail, so a cancel can restore the launching mode.
//!
//! ## Key capture
//!
//! A focused plugin only receives keystrokes that the *current input mode* does
//! not already bind — so in Search mode every letter would be swallowed by the
//! native search keybindings, and the `Alt+/` launch key would re-fire instead
//! of reaching us. To get a faithful text field regardless of mode we call
//! [`intercept_key_presses`] on load: while the dialog is open it captures
//! every key as [`Event::InterceptedKeyPress`], and we release the grab with
//! [`clear_key_presses_intercepts`] before closing.
//!
//! ## Origin pane
//!
//! We capture the originating terminal's [`PaneId`] from the pane manifest (the
//! focused non-plugin pane — it stays `is_focused` even while our floating
//! dialog holds focus) and re-focus it explicitly by id. Native search only
//! acts on the focused pane, so every search action is sandwiched between a
//! focus shift to the origin and back to us. On teardown (submit/cancel) we
//! shift focus to the origin and [`hide_self`]: the instance is persistent, so
//! hiding (not closing) returns us to the hidden background state, ready to be
//! re-revealed by the next `EnterSearch`.
//!
//! ## Live search
//!
//! Typing runs a *trailing-debounced* (`SEARCH_DEBOUNCE`) live search: we bounce
//! focus to the origin, push the term, re-apply the options, and bounce back —
//! all while staying in `Search` mode so the field keeps accepting keys.
//! Our own focus bounce is shielded from the cancel detection by `suppress_cancel`.
//! Because zellij's `clear_search` resets all search options on every
//! `SearchInput`, the case (Alt+C) / whole-word (Alt+B) toggles and the
//! always-on wrap are re-applied after the needle each time (see
//! `apply_search_options`).
//!
//! ## Persistence
//!
//! The pane is a persistent instance (shown/hidden per search). To make the
//! *last submitted* term reappear pre-filled the next time it opens, we persist
//! it to a temp file and read it back asynchronously on each `activate`.
//!
//! All buffer/cursor/edit/unicode bookkeeping is delegated to `tui_input`; the
//! only glue we own is the `KeyWithModifier -> InputRequest` mapping and the
//! rendering. On <Enter> we hand the term to Zellij's *native* search of the
//! originating terminal pane (so highlighting and `n`/`N` keep working) and
//! leave it in `Search` mode; on <Esc> we cancel back to the launching mode.

use std::collections::BTreeMap;
use unicode_width::UnicodeWidthChar;
use zellij_tile::prelude::actions::{Action, SearchDirection, SearchOption};
use zellij_tile::prelude::*;

use crate::icons;

/// Best-effort append to a debug log inside the plugin's WASI sandbox (root
/// must be a Zellij-preopened dir). Debug instrumentation only — gated by the
/// `debug_log` config key on the search-role plugin.
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

/// Title we give the floating pane via `rename_plugin_pane`, purely cosmetic.
/// (The status bar now lights its Search indicator from the client mode, not
/// from this pane's presence — see `render::build_right_side`.)
pub const PANE_TITLE: &str = "Search";

/// Shared (session-agnostic) file holding the last submitted term, read back on
/// the next launch to pre-fill the field. Not session-scoped: the launching
/// keybind opens us directly (no bar involvement) so we don't know the session
/// name, and sharing the last search term across sessions is harmless.
const SEARCH_FILE: &str = "/tmp/zj-statusbar-search";

/// `RunCommandResult` context tag for the prefill read.
const CTX_KEY: &str = "ctx";
const CTX_PREFILL: &str = "search_prefill";

/// Dialog geometry and chrome, applied to our own floating pane on setup.
const PANE_WIDTH: usize = 40;
const PANE_HEIGHT: usize = 3;
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
/// case-sensitivity (``) and whole-word (``).
const GLYPH_CASE: char = '\u{EAB1}';
const GLYPH_WORD: char = '\u{EB7E}';
/// Indicator foreground when the option is OFF (dark grey) and ON (yellow).
const TOGGLE_OFF_RGB: (u8, u8, u8) = (0x45, 0x47, 0x5A);
const TOGGLE_ON_RGB: (u8, u8, u8) = (0xF9, 0xE2, 0xAF);
/// Columns reserved at the end of the input area for the two indicators.
const TOGGLE_W: usize = 3;

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
/// Last column the input text may occupy before it scrolls.
const INPUT_END_COL: usize = 35;

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
    /// Toggle case-sensitive matching (Alt+C).
    ToggleCase,
    /// Toggle whole-word matching (Alt+W).
    ToggleWord,
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
    active: bool,
    /// The client's current input mode, tracked from `ModeUpdate` so we know
    /// when `EnterSearch` is entered (activate) or left (deactivate).
    mode: InputMode,
    /// The mode we came *from* when `EnterSearch` was entered — where a cancel
    /// (or empty submit) returns the client, so an interrupted search restores
    /// the prior mode (e.g. `Scroll`) instead of always dropping to `Normal`.
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
    /// Set while we deliberately bounce focus to the origin for a live search,
    /// so `check_focus`/`check_visible` don't mistake it for a user cancel.
    suppress_cancel: bool,
    /// Outstanding debounce timers. Each edit schedules one; the live search
    /// only fires when the count drains to zero, i.e. once typing has paused
    /// for `SEARCH_DEBOUNCE` (a trailing debounce, not a per-key throttle).
    pending_searches: usize,
    /// Case-sensitive matching toggle (Alt+C). Off ⇒ case-insensitive.
    case_sensitive: bool,
    /// Whole-word matching toggle (Alt+W).
    whole_word: bool,
    /// Optional debug-log path (from the `debug_log` config key).
    debug_log: Option<String>,
}

impl ZellijPlugin for SearchPane {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.debug_log = configuration.get("debug_log").cloned();
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

        // Headless and inert while hidden: NOT selectable, so the background
        // instance never joins the focus rotation (a selectable hidden pane
        // fights tab/pane focus). We flip to selectable on `activate` (so the
        // cursor renders in the field) and back off on teardown.
        set_selectable(false);
        // We load at session start (via `load_plugins`) and stay a hidden
        // background pane until the client enters `EnterSearch`; all reveal/
        // setup work happens in `activate` (see the ModeUpdate handler).
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(_) => true,
            // The dialog is mode-driven. `EnterSearch` is the *trigger* (the
            // text-input mode whose raw typing would otherwise bypass our key
            // grab); on entering it we record where we came from and `activate`,
            // which immediately flips the client to `Search` — a keybind mode
            // where our intercept wins and we can host the field. We never
            // auto-tear-down on a mode change (the only mode changes while we're
            // up are the ones we drive: our own flip to `Search`, then a
            // submit/cancel); teardown happens explicitly in submit/close.
            Event::ModeUpdate(info) => {
                let new = info.mode;
                if new == InputMode::EnterSearch && !self.active {
                    self.origin_mode = self.mode;
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
                if !self.active {
                    return false;
                }
                self.detect_origin(&manifest);
                self.anchor(&manifest);
                self.check_focus(&manifest);
                // A live search bounces focus to the origin and back; the grab
                // doesn't survive that bounce. Re-assert it here, once focus has
                // actually settled (calling it mid-bounce in `live_search` is a
                // no-op because we aren't focused yet).
                if !self.closing {
                    intercept_key_presses();
                }
                true
            }
            Event::TabUpdate(tabs) => {
                if self.active {
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
            Event::Key(key) | Event::InterceptedKeyPress(key) if self.active => {
                self.handle_key(key)
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
    /// Append a line to the debug log when `debug_log` is configured.
    fn log(&self, msg: &str) {
        if let Some(path) = &self.debug_log {
            append_log(path, msg);
        }
    }

    /// Reveal the dialog and run its per-open setup: reset the field/flags,
    /// become selectable (so the cursor renders), shape and show our hidden
    /// background pane (borderless, fixed size, floating + focused), flip the
    /// client to `Search` mode, grab keystrokes, and kick off the prefill read.
    ///
    /// We sit in `Search` (not `EnterSearch`) while typing: `EnterSearch` is a
    /// text-input mode whose raw typing bypasses the key grab, whereas `Search`
    /// is an ordinary keybind mode where `intercept_key_presses` wins — so the
    /// field works, the bar reads "search", and which-key's mode trail stays
    /// intact (it isn't the base mode). On submit we stay in `Search` for
    /// `n`/`N`; on cancel we return to `origin_mode`.
    fn activate(&mut self) {
        self.active = true;
        self.ready = true;
        self.closing = false;
        self.was_focused = false;
        self.seen_visible = false;
        self.suppress_cancel = false;
        self.last_searched = None;
        self.pending_searches = 0;
        self.anchored = None;
        self.input = tui_input::Input::default();

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
        // Selectable while shown so the real terminal cursor renders in-field.
        set_selectable(true);
        rename_plugin_pane(get_plugin_ids().plugin_id, PANE_TITLE);
        // Borderless + fixed size + custom background: shape the dialog from the
        // plugin itself. `anchor` repositions it bottom-right on PaneUpdate.
        set_pane_borderless(pane, true);
        change_floating_panes_coordinates(vec![(
            pane,
            FloatingPaneCoordinates::default()
                .with_width_fixed(PANE_WIDTH)
                .with_height_fixed(PANE_HEIGHT),
        )]);
        // Reveal as a focused floating pane so the cursor renders and our grab
        // has somewhere to draw; key capture itself comes from the intercept.
        show_pane_with_id(pane, true, true);
        // Leave the text-input `EnterSearch` for the keybind `Search` mode so
        // the intercept (asserted next) actually receives keystrokes.
        switch_to_input_mode(&InputMode::Search);
        intercept_key_presses();
        self.request_prefill();
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
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
            Some(KeyAct::ToggleCase) => {
                self.case_sensitive = !self.case_sensitive;
                // Force a re-run with the new option (the term itself is unchanged).
                self.last_searched = None;
                self.live_search();
                true
            }
            Some(KeyAct::ToggleWord) => {
                self.whole_word = !self.whole_word;
                self.last_searched = None;
                self.live_search();
                true
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
        // Wrap is always on; the default is off, so always toggle it.
        run_action(
            Action::SearchToggleOption {
                option: SearchOption::Wrap,
            },
            BTreeMap::new(),
        );
    }

    /// Search-as-you-type: when the debounce fires and the term changed, push
    /// it to the origin's native search, then return focus to ourselves. We set
    /// `suppress_cancel` around the focus bounce so our own focus changes aren't
    /// read as a user dismissal. Emptying the field clears the highlight.
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
        self.suppress_cancel = true;
        // Drive native search on the origin *without* switching the client mode:
        // staying in Normal keeps our dialog typable (and the grab intact). The
        // actions run as the user against the focused origin regardless of mode.
        focus_pane_with_id(origin, false, false);
        // Reset the pane's search buffer first; then either search the new term
        // or, when emptied, search the now-empty buffer to clear the highlight.
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
        // `should_float_if_hidden = true`: focusing the origin hid the floating
        // layer, so we must ask for it back or our pane stays gone. The grab is
        // re-asserted from the PaneUpdate handler once focus settles back on us.
        focus_pane_with_id(PaneId::Plugin(get_plugin_ids().plugin_id), true, false);
    }

    /// Hand the term to Zellij's native search of the origin pane, then hide.
    ///
    /// Order matters: we release the key-press grab and re-focus the origin
    /// first, so the subsequent `SearchInput`/`Search` actions and mode switch
    /// target it (not us). We re-apply the options after the needle (every
    /// `SearchInput` resets them), then settle in `Search` mode so `n`/`N` work.
    /// An empty submit has nothing to commit, so it falls through to `close`
    /// (returning the client to `origin_mode`). Because we are a persistent
    /// headless instance we `hide_self` rather than close — the next
    /// `EnterSearch` re-activates us, pre-filled with the last submitted term.
    fn submit(&mut self) {
        let term = self.input.value().to_string();
        if term.is_empty() {
            self.close();
            return;
        }
        self.closing = true;
        self.active = false;
        self.persist_term(&term);
        clear_key_presses_intercepts();
        self.refocus_origin();
        // Reset the pane's search buffer first (mirrors the native
        // `SearchInput 0`); otherwise a re-search appends to the previous
        // term and finds nothing.
        run_action(Action::SearchInput { input: vec![0] }, BTreeMap::new());
        run_action(
            Action::SearchInput {
                input: term.into_bytes(),
            },
            BTreeMap::new(),
        );
        // Preserve the case / whole-word / wrap toggles in the committed
        // search (the SearchInput above reset them to defaults).
        self.apply_search_options();
        run_action(
            Action::Search {
                direction: SearchDirection::Down,
            },
            BTreeMap::new(),
        );
        // We're already in `Search`; assert it anyway so `n`/`N` work even if
        // focus bounces nudged the client mode. Then go inert + hidden: dropping
        // selectable keeps the background instance out of the focus rotation,
        // and which-key (which yields to our focused float) reclaims the panel.
        switch_to_input_mode(&InputMode::Search);
        set_selectable(false);
        hide_self();
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
        switch_to_input_mode(&self.origin_mode);
        set_selectable(false);
        hide_self();
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
            Some(_) if self.was_focused && !self.suppress_cancel => self.close(),
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
            // A live-search bounce briefly focuses the origin (hiding the
            // floating layer) then refocuses us (showing it again). Seeing the
            // layer shown is the reliable end-of-bounce signal — clear the
            // suppression here, *not* in `check_focus` (our pane stays
            // `is_focused` throughout the bounce, so that would clear too soon
            // and let the bounce's transient hide read as a cancel).
            self.suppress_cancel = false;
        } else if self.seen_visible && !self.suppress_cancel {
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
    fn anchor(&mut self, manifest: &PaneManifest) {
        const RIGHT_MARGIN: usize = 1;
        const BOTTOM_GAP: usize = 1;
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
        let screen_w = status.pane_x + status.pane_columns;
        let x = screen_w.saturating_sub(PANE_WIDTH + RIGHT_MARGIN);
        let y = status.pane_y.saturating_sub(PANE_HEIGHT + BOTTOM_GAP);
        self.log(&format!(
            "[anchor] tab={tab} status(x={},y={},cols={}) -> x={x} y={y} prev={:?}\n",
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
                .with_width_fixed(PANE_WIDTH)
                .with_height_fixed(PANE_HEIGHT),
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
        let interior = INPUT_END_COL - INPUT_COL + 2;
        let box_left = INPUT_COL.saturating_sub(1);
        let box_right = INPUT_END_COL + 2;
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
        let area_w = INPUT_END_COL - INPUT_COL + 1;
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
        // input area: ` 󰘵 󰘵 ` style — leading gap, case glyph, gap, word glyph,
        // trailing gap (5 columns). Yellow when on, dark grey when off, over the
        // input background.
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
        out.push_str(&format!(
            "\u{1b}[{};{}H{input_bg}{c_fg}{GLYPH_CASE}{reset}{input_bg} {w_fg}{GLYPH_WORD}{reset}",
            FIELD_ROW + 1,
            // Anchored to the right end of the input area, independent of field_w.
            INPUT_END_COL - TOGGLE_W + 2,
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

        // Search-option toggles. (Whole-word is Alt+B since Alt+W is the leader.)
        Char('c') if alt => KeyAct::ToggleCase,
        Char('b') if alt => KeyAct::ToggleWord,

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
