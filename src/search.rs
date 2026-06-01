//! Floating "visual search" pane.
//!
//! This is the *second* role of the plugin binary (see `main.rs`'s `Plugin`
//! dispatch). It is loaded headless at session start (via `load_plugins` with
//! `role "search"`) and stays a hidden background pane until the client enters
//! the `EnterSearch` input mode — at which point `activate` reveals it. A
//! keybind only needs `SwitchToMode "EnterSearch"` (e.g. `Alt+/`, Ghostty's
//! Cmd+F); we react to the resulting `ModeUpdate`. On activation we immediately
//! flip the client to **`Normal`** — counter-intuitively the *only* mode in
//! which `intercept_key_presses` actually delivers keys to us. In
//! `EnterSearch`/`Search` zellij's native input layer consumes keystrokes
//! before any plugin grab can see them, so our custom text field would never
//! receive input there (verified empirically). The status bar still shows a
//! Search indicator: the search pane pushes it explicitly to the bar role via
//! the `__zj_statusbar_search` pipe (`set_bar_search_indicator`), since it can
//! no longer be inferred from the (now `Normal`) client mode.
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
//! focus bounce. On teardown we [`hide_self`]: the instance is persistent, so
//! hiding (not closing) returns us to the hidden background state, ready to be
//! re-revealed by the next `EnterSearch`.
//!
//! ## Live search
//!
//! Typing runs a *trailing-debounced* (`SEARCH_DEBOUNCE`) live search: we push
//! the term to the (already-focused) origin's native search and re-apply the
//! options — no focus bounce, no mode change. Because zellij's `clear_search`
//! resets all search options on every `SearchInput`, the case (Alt+C) /
//! whole-word (Alt+O) toggles and the always-on wrap are re-applied after the
//! needle each time (see `apply_search_options`).
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
//! enter a *navigation phase*: the input field hides but we keep the key grab
//! and drive `n`/`N` ourselves, so <Esc> can return the client to the launching
//! mode (`origin_mode`) instead of native `Search` mode's drop to `Normal`. On
//! <Esc> from the field (before submit) we cancel straight back to that mode.

use std::collections::BTreeMap;
use unicode_width::UnicodeWidthChar;
use zellij_tile::prelude::actions::{Action, SearchDirection, SearchOption};
use zellij_tile::prelude::*;

use crate::icons;

/// Title we give the floating pane via `rename_plugin_pane`, purely cosmetic.
/// (The status bar now lights its Search indicator from the client mode, not
/// from this pane's presence — see `render::build_right_side`.)
pub const PANE_TITLE: &str = "Search";

/// Pipe name used to tell the status-bar role(s) whether the search dialog is
/// open. We keep the *client* mode in `Normal` while the dialog is up (the only
/// mode in which `intercept_key_presses` delivers keys to us), so the bar can't
/// infer "search active" from the mode and must be told explicitly.
pub const SEARCH_INDICATOR_PIPE: &str = "__zj_statusbar_search";

/// Broadcast the search-dialog on/off state to the bar role(s).
fn set_bar_search_indicator(active: bool) {
    pipe_message_to_plugin(
        MessageToPlugin::new(SEARCH_INDICATOR_PIPE).with_payload(if active { "1" } else { "0" }),
    );
}

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
    /// Toggle whole-word matching (Alt+O — Alt+W is the which-key leader).
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
    /// Post-submit "results navigation" phase. After <Enter> the input field is
    /// hidden but we KEEP the key-intercept grab (and the client in `Normal`) so
    /// we can drive `n`/`N` ourselves and, crucially, redirect <Esc> back to
    /// `origin_mode` (e.g. `Scroll`) instead of letting native `Search` mode
    /// drop the client to `Normal`.
    navigating: bool,
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
    /// Outstanding debounce timers. Each edit schedules one; the live search
    /// only fires when the count drains to zero, i.e. once typing has paused
    /// for `SEARCH_DEBOUNCE` (a trailing debounce, not a per-key throttle).
    pending_searches: usize,
    /// Case-sensitive matching toggle (Alt+C). Off ⇒ case-insensitive.
    case_sensitive: bool,
    /// Whole-word matching toggle (Alt+O).
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
            // The dialog is mode-driven. `EnterSearch` is the *trigger*; on
            // entering it we record where we came from (`origin_mode`) and
            // `activate`, which flips the client to `Normal` (the only mode in
            // which our intercept receives keys). We never auto-tear-down on a
            // mode change — the only mode changes while we're up are ones we
            // drive; teardown happens explicitly in submit/close/finish.
            Event::ModeUpdate(info) => {
                let new = info.mode;
                self.log(&format!(
                    "[ModeUpdate] new={new:?} prev={:?} active={}\n",
                    self.mode, self.active
                ));
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
            Event::Key(key) => {
                self.log(&format!(
                    "[Key] active={} mode={:?} zellij_focus={:?} key={key:?}\n",
                    self.active,
                    self.mode,
                    get_focused_pane_info().ok(),
                ));
                if self.active {
                    self.handle_key(key)
                } else if self.navigating {
                    self.handle_nav_key(key)
                } else {
                    false
                }
            }
            Event::InterceptedKeyPress(key) => {
                self.log(&format!(
                    "[InterceptedKeyPress] active={} navigating={} mode={:?} zellij_focus={:?} key={key:?}\n",
                    self.active,
                    self.navigating,
                    self.mode,
                    get_focused_pane_info().ok(),
                ));
                if self.active {
                    self.handle_key(key)
                } else if self.navigating {
                    self.handle_nav_key(key)
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

    /// Reveal the dialog and run its per-open setup: reset the field/flags,
    /// become selectable (so the cursor renders), shape and reveal our hidden
    /// background pane (borderless, fixed size, floating, *pinned + unfocused*),
    /// grab keystrokes, flip the client to `Normal`, raise the bar's Search
    /// indicator, and kick off the prefill read.
    ///
    /// We hold the client in `Normal` while typing because it is the only mode
    /// in which `intercept_key_presses` delivers keys to us — `EnterSearch`/
    /// `Search` consume them natively first. The bar's Search indicator is
    /// driven explicitly (see `set_bar_search_indicator`) rather than from the
    /// client mode. On cancel we return to `origin_mode`; on submit we enter the
    /// navigation phase (`n`/`N` + <Esc>→`origin_mode`).
    fn activate(&mut self) {
        self.active = true;
        self.navigating = false;
        self.ready = true;
        self.closing = false;
        self.was_focused = false;
        self.seen_visible = false;
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
        // indicator: we push it explicitly via `set_bar_search_indicator`
        // because it can no longer be read from the (now `Normal`) client mode.
        switch_to_input_mode(&InputMode::Normal);
        set_bar_search_indicator(true);
        self.log(
            "[activate] revealed(unfocused) + pinned + intercept; mode->Normal + bar:search=1\n",
        );
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
    /// phase rather than handing control back to native `Search` mode.
    ///
    /// We can't use native `Search` mode because its <Esc> drops the client to
    /// `Normal`, but the user wants <Esc> to return to where they launched from
    /// (`origin_mode`, e.g. `Scroll`). So we KEEP the key-intercept grab and the
    /// client in `Normal`, hide the input field, and drive `n`/`N`/<Esc>
    /// ourselves (see `handle_nav_key`). The bar indicator stays on Search for
    /// the whole navigation phase. An empty submit has nothing to commit, so it
    /// falls through to `close`. We are persistent, so we `hide_self` (not
    /// close); the next `EnterSearch` re-activates us, pre-filled.
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
        // Hide the input field but stay live: keep the intercept grab, keep the
        // client in `Normal`, and keep the Search indicator up. We're now in the
        // navigation phase (`n`/`N`/<Esc> handled in `handle_nav_key`).
        self.active = false;
        self.navigating = true;
        self.closing = false;
        set_selectable(false);
        hide_self();
        self.log("[submit] committed; -> navigating (intercept kept, bar:search=1)\n");
    }

    /// Navigation-phase key handling (post-submit). `n`/Down → next match,
    /// `N`/`p`/Up → previous match, <Esc>/<Enter> → finish and return the client
    /// to `origin_mode`. Every other key is swallowed (mirrors native `Search`
    /// mode's restricted bindings) so stray keys don't leak to the terminal.
    fn handle_nav_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Esc | BareKey::Enter => {
                self.finish_navigation();
                false
            }
            BareKey::Char('n') | BareKey::Down => {
                run_action(
                    Action::Search {
                        direction: SearchDirection::Down,
                    },
                    BTreeMap::new(),
                );
                true
            }
            BareKey::Char('N') | BareKey::Char('p') | BareKey::Up => {
                run_action(
                    Action::Search {
                        direction: SearchDirection::Up,
                    },
                    BTreeMap::new(),
                );
                true
            }
            _ => true,
        }
    }

    /// End the navigation phase: release the grab, clear the Search indicator,
    /// and restore the launching mode (e.g. `Scroll`). The highlight is left in
    /// place — the user is simply back in their origin mode.
    fn finish_navigation(&mut self) {
        self.navigating = false;
        clear_key_presses_intercepts();
        set_bar_search_indicator(false);
        switch_to_input_mode(&self.origin_mode);
        set_selectable(false);
        self.log(&format!(
            "[finish_navigation] grab cleared; bar:search=0; mode->{:?}\n",
            self.origin_mode
        ));
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
        set_bar_search_indicator(false);
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

        // Search-option toggles. Whole-word is Alt+O ("whole-wOrd"); Alt+W can't
        // be used because it's the which-key leader.
        Char('c') if alt => KeyAct::ToggleCase,
        Char('o') if alt => KeyAct::ToggleWord,

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
