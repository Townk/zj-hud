//! Floating rename dialog.
//!
//! This role mirrors the visual-search dialog's lifecycle: each tab owns a
//! parked 1x1 floating plugin pane, and only the active tab's instance reveals
//! when the client enters `RenameTab` or `RenamePane`. While shown we intercept
//! keystrokes and hold the client in `Normal`, because Zellij's native rename
//! modes consume typed input before plugin key interception can see it.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;
use zellij_tile::prelude::*;

use crate::shared::geometry::{place, Anchor, Padding, WidthMode};
use crate::shared::icons;

pub const PANE_TITLE: &str = "Rename";

const PANE_WIDTH: usize = 40;
const PANE_HEIGHT: usize = 3;
const RIGHT_INSET: usize = 5;
const MIN_WIDTH: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenameGeom {
    anchor: Anchor,
    margin: Padding,
    width: usize,
}

impl Default for RenameGeom {
    fn default() -> Self {
        Self {
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

impl RenameGeom {
    /// Reuse the bar-authored `search { anchor; width; margin }` placement so
    /// the rename dialog appears where the existing search dialog does.
    fn from_search_block(block: &str) -> Self {
        let mut geom = RenameGeom::default();
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

    fn input_end_col(&self) -> usize {
        self.width.saturating_sub(RIGHT_INSET).max(INPUT_COL + 2)
    }
}

const BG_RGB: (u8, u8, u8) = (0x28, 0x2C, 0x41);
const RENAME_RGB: (u8, u8, u8) = (0xE5, 0xBF, 0x7B);
const INPUT_BG_RGB: (u8, u8, u8) = (0x18, 0x18, 0x25);
const THEME_BG_RGB: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);
const BORDER_CHAR: char = '┃';

const BOX_TL: char = '𜺠';
const BOX_TR: char = '𜺣';
const BOX_BL: char = '𜺫';
const BOX_BR: char = '𜺨';
const BOX_TOP: char = '▂';
const BOX_BOT: char = '🮂';
const BOX_LEFT: char = '▐';
const BOX_RIGHT: char = '▌';

const FIELD_ROW: usize = 1;
const GLYPH_COL: usize = 2;
const INPUT_COL: usize = 5;

enum KeyAct {
    Edit(tui_input::InputRequest),
    Submit,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameTarget {
    Tab { position: u32 },
    Pane { id: PaneId },
}

#[derive(Default)]
pub struct RenamePane {
    input: tui_input::Input,
    active: bool,
    ready: bool,
    mode: InputMode,
    origin_mode: InputMode,
    target_mode: InputMode,
    target: Option<RenameTarget>,
    origin: Option<PaneId>,
    anchored: Option<(usize, usize)>,
    was_focused: bool,
    seen_visible: bool,
    closing: bool,
    session_name: String,
    geom: RenameGeom,
    granted: bool,
    active_tab: usize,
    my_tab: Option<usize>,
    tabs: Vec<TabInfo>,
}

impl ZellijPlugin for RenamePane {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        request_permission(crate::PLUGIN_PERMISSIONS);
        subscribe(&[
            EventType::Key,
            EventType::InterceptedKeyPress,
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::ModeUpdate,
        ]);
        set_selectable(false);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(_) => {
                self.ensure_parked();
                true
            }
            Event::ModeUpdate(info) => {
                let new = info.mode;
                if let Some(name) = info.session_name {
                    self.session_name = name;
                }
                if matches!(new, InputMode::RenameTab | InputMode::RenamePane)
                    && !self.active
                    && self.is_on_active_tab()
                {
                    self.origin_mode = self.cancel_target();
                    self.activate(new);
                }
                self.mode = new;
                false
            }
            Event::PaneUpdate(manifest) => {
                self.detect_my_tab(&manifest);
                if !self.active {
                    return false;
                }
                self.anchor(&manifest);
                self.check_focus(&manifest);
                true
            }
            Event::TabUpdate(tabs) => {
                self.tabs = tabs.clone();
                if let Some(active) = tabs.iter().find(|t| t.active) {
                    self.active_tab = active.position;
                }
                if self.active && !self.is_on_active_tab() {
                    self.close();
                } else if self.active {
                    self.check_visible(&tabs);
                }
                false
            }
            Event::Key(key) | Event::InterceptedKeyPress(key) if self.active => {
                self.handle_key(key)
            }
            Event::Key(_) | Event::InterceptedKeyPress(_) => false,
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.render_field(rows, cols);
    }
}

impl RenamePane {
    fn me(&self) -> PaneId {
        PaneId::Plugin(get_plugin_ids().plugin_id)
    }

    fn is_on_active_tab(&self) -> bool {
        self.my_tab == Some(self.active_tab)
    }

    fn cancel_target(&self) -> InputMode {
        if matches!(self.mode, InputMode::RenameTab | InputMode::RenamePane) {
            InputMode::Normal
        } else {
            self.mode
        }
    }

    fn detect_my_tab(&mut self, manifest: &PaneManifest) {
        let my_id = get_plugin_ids().plugin_id;
        for (tab, panes) in &manifest.panes {
            if panes.iter().any(|p| p.is_plugin && p.id == my_id) {
                self.my_tab = Some(*tab);
                return;
            }
        }
    }

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

    fn activate(&mut self, mode: InputMode) {
        let Some(target) = self.resolve_target(mode) else {
            switch_to_input_mode(&self.origin_mode);
            return;
        };

        self.active = true;
        self.ready = true;
        self.closing = false;
        self.was_focused = false;
        self.seen_visible = false;
        self.anchored = None;
        self.target_mode = mode;
        self.target = Some(target);
        self.input = tui_input::Input::new(self.title_for_target(target));
        self.load_geom();

        let pane = self.me();
        rename_plugin_pane(get_plugin_ids().plugin_id, PANE_TITLE);
        set_pane_borderless(pane, true);
        change_floating_panes_coordinates(vec![(
            pane,
            FloatingPaneCoordinates::default()
                .with_width_fixed(self.geom.width)
                .with_height_fixed(PANE_HEIGHT),
        )]);
        show_pane_with_id(pane, true, false);
        set_selectable(true);
        set_floating_pane_pinned(pane, true);
        intercept_key_presses();
        self.publish_rename_state(true);
        switch_to_input_mode(&InputMode::Normal);
    }

    fn resolve_target(&mut self, mode: InputMode) -> Option<RenameTarget> {
        match mode {
            InputMode::RenameTab => Some(RenameTarget::Tab {
                position: self.active_tab as u32,
            }),
            InputMode::RenamePane => {
                let focused = get_focused_pane_info()
                    .ok()
                    .map(|(_, pane)| pane)
                    .filter(|pane| *pane != self.me())
                    .or(self.origin)?;
                self.origin = Some(focused);
                Some(RenameTarget::Pane { id: focused })
            }
            _ => None,
        }
    }

    fn title_for_target(&self, target: RenameTarget) -> String {
        match target {
            RenameTarget::Tab { position } => self
                .tabs
                .iter()
                .find(|tab| tab.position == position as usize)
                .map(|tab| tab.name.clone())
                .unwrap_or_default(),
            RenameTarget::Pane { id } => {
                get_pane_info(id).map(|pane| pane.title).unwrap_or_default()
            }
        }
    }

    fn load_geom(&mut self) {
        let path =
            crate::shared::state::state_path(get_plugin_ids().zellij_pid, &self.session_name);
        let shared = crate::shared::state::read_state_from(&path).unwrap_or_default();
        self.geom = RenameGeom::from_search_block(&shared.search_config);
    }

    fn publish_rename_state(&self, active: bool) {
        let path =
            crate::shared::state::state_path(get_plugin_ids().zellij_pid, &self.session_name);
        if let Some(state) =
            crate::shared::state::mutate_state_file(&path, get_plugin_ids().plugin_id, |s| {
                s.rename_active = active;
                s.rename_mode = crate::shared::state::mode_name(self.target_mode).to_string();
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
        match decode_key(&key) {
            Some(KeyAct::Edit(req)) => {
                self.input.handle(req);
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

    fn submit(&mut self) {
        let name = self.input.value().to_string();
        if let Some(target) = self.target {
            match target {
                RenameTarget::Tab { position } => rename_tab(position, &name),
                RenameTarget::Pane { id } => rename_pane_with_id(id, &name),
            }
        }
        self.finish(InputMode::Normal);
    }

    fn close(&mut self) {
        self.finish(self.origin_mode);
    }

    fn finish(&mut self, return_mode: InputMode) {
        self.closing = true;
        self.active = false;
        clear_key_presses_intercepts();
        self.publish_rename_state(false);
        self.refocus_origin();
        switch_to_input_mode(&return_mode);
        set_selectable(false);
        self.park();
    }

    fn refocus_origin(&self) {
        if let Some(id) = self.origin {
            focus_pane_with_id(id, false, false);
        }
    }

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

    fn anchor(&mut self, manifest: &PaneManifest) {
        let Ok((tab, _focused)) = get_focused_pane_info() else {
            return;
        };
        let Some(panes) = manifest.panes.get(&tab) else {
            return;
        };
        let Some(status) = panes
            .iter()
            .filter(|p| p.is_plugin && !p.is_selectable && !p.is_floating)
            .max_by_key(|p| p.pane_y)
        else {
            return;
        };

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
        if self.anchored == Some((x, y)) {
            return;
        }
        self.anchored = Some((x, y));
        change_floating_panes_coordinates(vec![(
            self.me(),
            FloatingPaneCoordinates::default()
                .with_x_fixed(x)
                .with_y_fixed(y)
                .with_width_fixed(rect.width)
                .with_height_fixed(rect.height),
        )]);
    }

    fn render_field(&mut self, rows: usize, cols: usize) {
        let input_end_col = self.geom.input_end_col();
        let (r, g, b) = BG_RGB;
        let bg = format!("\u{1b}[48;2;{r};{g};{b}m");
        let (br, bg_, bb) = RENAME_RGB;
        let border_fg = format!("\u{1b}[38;2;{br};{bg_};{bb}m");
        let (tr, tg, tb) = THEME_BG_RGB;
        let theme_bg = format!("\u{1b}[48;2;{tr};{tg};{tb}m");
        let reset = "\u{1b}[0m";
        let blank = " ".repeat(cols);
        let rows = rows.max(1);

        let mut out = String::new();
        for row in 1..=rows {
            out.push_str(&format!("\u{1b}[{row};1H{bg}{blank}"));
            out.push_str(&format!(
                "\u{1b}[{row};1H{theme_bg}{border_fg}{BORDER_CHAR}{reset}"
            ));
        }

        out.push_str(&format!(
            "\u{1b}[{};{}H{bg}{}{reset}",
            FIELD_ROW + 1,
            GLYPH_COL + 1,
            icons::MODE_RENAME
        ));

        let (fr, fg2, fb) = INPUT_BG_RGB;
        let box_fg = format!("\u{1b}[38;2;{fr};{fg2};{fb}m");
        let interior = input_end_col.saturating_sub(INPUT_COL) + 2;
        let box_left = INPUT_COL.saturating_sub(1);
        let box_right = input_end_col + 2;
        let top_csi = FIELD_ROW;
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

        let area_w = input_end_col.saturating_sub(INPUT_COL) + 1;
        let field_w = area_w.max(1);
        let scroll = self.input.visual_scroll(field_w);
        let shown = skip_columns(self.input.value(), scroll);
        let cursor_col = self.input.visual_cursor().saturating_sub(scroll);

        let (ir, ig, ib) = INPUT_BG_RGB;
        let input_bg = format!("\u{1b}[48;2;{ir};{ig};{ib}m");
        out.push_str(&format!(
            "\u{1b}[{};{}H{input_bg}{}{reset}",
            FIELD_ROW + 1,
            INPUT_COL + 1,
            " ".repeat(area_w + 1),
        ));

        let (sr, sg, sb) = RENAME_RGB;
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

        print!("{out}");
    }
}

fn decode_key(key: &KeyWithModifier) -> Option<KeyAct> {
    use tui_input::InputRequest as R;
    use BareKey::*;

    let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
    let alt = key.key_modifiers.contains(&KeyModifier::Alt);

    let act = match key.bare_key {
        Enter => KeyAct::Submit,
        Esc => KeyAct::Cancel,
        Char('c') if ctrl => KeyAct::Cancel,

        Char('a') if ctrl => KeyAct::Edit(R::GoToStart),
        Char('e') if ctrl => KeyAct::Edit(R::GoToEnd),
        Char('b') if ctrl => KeyAct::Edit(R::GoToPrevChar),
        Char('f') if ctrl => KeyAct::Edit(R::GoToNextChar),
        Char('w') if ctrl => KeyAct::Edit(R::DeletePrevWord),
        Char('u') if ctrl => KeyAct::Edit(R::DeleteLine),
        Char('k') if ctrl => KeyAct::Edit(R::DeleteTillEnd),

        Char(c) if !ctrl && !alt => KeyAct::Edit(R::InsertChar(c)),

        Left if alt => KeyAct::Edit(R::GoToPrevWord),
        Right if alt => KeyAct::Edit(R::GoToNextWord),
        Left => KeyAct::Edit(R::GoToPrevChar),
        Right => KeyAct::Edit(R::GoToNextChar),
        Home => KeyAct::Edit(R::GoToStart),
        End => KeyAct::Edit(R::GoToEnd),

        Backspace => KeyAct::Edit(R::DeletePrevChar),
        Delete => KeyAct::Edit(R::DeleteNextChar),

        _ => return None,
    };
    Some(act)
}

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
    fn enter_submits_esc_cancels() {
        assert!(matches!(
            decode_key(&key(BareKey::Enter, &[])),
            Some(KeyAct::Submit)
        ));
        assert!(matches!(
            decode_key(&key(BareKey::Esc, &[])),
            Some(KeyAct::Cancel)
        ));
    }

    #[test]
    fn editing_keys_match_search_dialog() {
        assert!(matches!(
            decode_key(&key(BareKey::Char('a'), &[KeyModifier::Ctrl])),
            Some(KeyAct::Edit(tui_input::InputRequest::GoToStart))
        ));
        assert!(matches!(
            decode_key(&key(BareKey::Left, &[KeyModifier::Alt])),
            Some(KeyAct::Edit(tui_input::InputRequest::GoToPrevWord))
        ));
        assert!(matches!(
            decode_key(&key(BareKey::Char('x'), &[])),
            Some(KeyAct::Edit(tui_input::InputRequest::InsertChar('x')))
        ));
    }

    #[test]
    fn rename_geom_reuses_search_placement_keys() {
        use crate::shared::geometry::{HAlign, VAlign};
        let g = RenameGeom::from_search_block("anchor \"top+left\"\nwidth 60\nmargin \"2,3,2,3\"");
        assert_eq!(g.anchor.v, VAlign::Top);
        assert_eq!(g.anchor.h, HAlign::Left);
        assert_eq!(g.width, 60);
        assert_eq!(g.margin.top, 2);
        assert_eq!(g.margin.left, 3);
        assert_eq!(RenameGeom::from_search_block("width 5").width, MIN_WIDTH);
    }

    #[test]
    fn skip_columns_handles_ascii() {
        assert_eq!(skip_columns("hello", 0), "hello");
        assert_eq!(skip_columns("hello", 2), "llo");
        assert_eq!(skip_columns("hello", 10), "");
    }
}
