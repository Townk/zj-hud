use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::InputMode;

use crate::bar::click_map::{ClickAction, ClickMap};
use crate::bar::config::{Config, DEFAULT_BAR_BG};
use crate::bar::state::AppState;
use crate::bar::status::system;
use crate::bar::status::{
    divider, divider_width, info_widgets_segment, locked_hint_segment, mode_segment,
    search_hint_segment, session_segment, time_segment, which_key_hidden_hint_segment,
    InfoWidgetsResult, Segment,
};
use crate::bar::tabs::layout::{compute_tab_layout, CHEVRON_TAB_WIDTH};
use crate::bar::tabs::{compose_tab_title, render_tab_title};
use crate::shared::color::{gradient, Color};
use crate::shared::icons::{LEFT_HALF_BLOCK, SCROLL_LEFT_ARROW, SCROLL_RIGHT_ARROW};

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";

pub struct RenderedSide {
    pub text: String,
    pub width: usize,
    /// Click regions in this side's local column space (0 = first column
    /// of the side's text).
    pub clicks: ClickMap,
}

pub struct RenderedBar {
    pub text: String,
    /// Click regions in absolute bar columns (0 = leftmost column of the
    /// status bar). Mouse handlers can `lookup` directly with the column
    /// reported by Zellij.
    pub click_map: ClickMap,
}

/// The effective status-bar background. An explicit `bar_bg` override wins;
/// otherwise it follows the live Zellij theme
/// (`style.colors.text_unselected.background`), falling back to
/// [`DEFAULT_BAR_BG`] until the first `ModeUpdate` lands a `Style`.
pub(crate) fn resolve_bar_bg(state: &AppState, config: &Config) -> Color {
    if let Some(bar_bg) = config.bar_bg {
        return bar_bg;
    }
    state
        .style
        .map(|style| Color::from_palette(style.colors.text_unselected.background))
        .unwrap_or(DEFAULT_BAR_BG)
}

pub fn render_bar(state: &AppState, config: &Config, cols: usize) -> RenderedBar {
    let bar_bg = resolve_bar_bg(state, config);
    let right = build_right_side(state, config, cols);
    let left_budget = cols.saturating_sub(right.width);
    let left = build_left_side(state, config, left_budget);

    let used = left.width + right.width;
    let gap_cols = cols.saturating_sub(used);
    let gap = format!("{}{}", bar_bg.to_ansi_bg(), " ".repeat(gap_cols));

    let mut click_map = left.clicks;
    let mut right_clicks = right.clicks;
    right_clicks.shift(cols.saturating_sub(right.width));
    click_map.extend(right_clicks);

    RenderedBar {
        text: format!("{}{}{}{}", left.text, gap, right.text, ANSI_RESET),
        click_map,
    }
}

fn build_left_side(state: &AppState, config: &Config, available: usize) -> RenderedSide {
    let tab_count = state.tabs.len();

    if tab_count == 0 || (config.tab_hide_single && tab_count == 1) {
        return RenderedSide {
            text: String::new(),
            width: 0,
            clicks: ClickMap::new(),
        };
    }

    let active_idx = state.active_tab_index().unwrap_or(0);
    render_tabs(state, config, active_idx, available)
}

fn render_tabs(
    state: &AppState,
    config: &Config,
    active_idx: usize,
    available: usize,
) -> RenderedSide {
    let tab_count = state.tabs.len();
    let bar_bg = resolve_bar_bg(state, config);

    let titles: Vec<_> = state
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| compose_tab_title(i, &tab.name, state, config))
        .collect();

    let natural_widths: Vec<usize> = titles
        .iter()
        .map(|t| {
            let rendered = render_tab_title(t, config.tab_max_width, config.tab_truncation_point);
            UnicodeWidthStr::width(rendered.as_str()) + 3 // 1 leading space + 1 trailing space + 1 edge glyph
        })
        .collect();

    let layout = compute_tab_layout(tab_count, active_idx, &natural_widths, available, config);

    let mut text = String::new();
    let mut total_width = 0usize;
    let mut clicks = ClickMap::new();

    // Overhead per tab: 1 leading space + 1 trailing space + 1 edge glyph = 3 cells.
    // CHEVRON_TAB_WIDTH (3) must equal the actual rendered width of each indicator below.
    let _ = CHEVRON_TAB_WIDTH; // assert the constant is in scope

    if let Some(ref scroll) = layout.scroll {
        if scroll.has_left {
            // " ◂▌" — styled like a non-active tab, no trailing space before the edge
            text.push_str(&format!(
                "{}{}{} {}{}{}{}",
                ANSI_BOLD,
                config.tab_bg.to_ansi_bg(),
                config.tab_fg.to_ansi_fg(),
                SCROLL_LEFT_ARROW,
                bar_bg.to_ansi_bg(),
                config.tab_bg.to_ansi_fg(),
                LEFT_HALF_BLOCK,
            ));
            // Clicking the left chevron jumps one tab beyond the visible
            // window. `scroll.has_left` implies `scroll.left > 0`, so the
            // 1-based switch_tab_to argument is just `scroll.left`.
            clicks.push(
                total_width,
                total_width + CHEVRON_TAB_WIDTH,
                ClickAction::SwitchToTab(scroll.left as u32),
            );
            total_width += CHEVRON_TAB_WIDTH;
        }
    }

    let (first_visible, last_visible) = layout
        .scroll
        .as_ref()
        .map(|scroll| (scroll.left, scroll.right))
        .unwrap_or((0, tab_count - 1));

    for (i, title) in titles
        .iter()
        .enumerate()
        .take(last_visible + 1)
        .skip(first_visible)
    {
        let alloc = layout.widths[i];
        if alloc == 0 {
            continue;
        }

        let is_active = i == active_idx;

        let (bg, fg) = if is_active {
            (config.active_tab_bg, config.active_tab_fg)
        } else {
            (config.tab_bg, config.tab_fg)
        };

        // Overhead is 3 (leading space + trailing space + edge glyph); pass the
        // remainder as the max text budget so width accounting stays exact.
        let rendered =
            render_tab_title(title, alloc.saturating_sub(3), config.tab_truncation_point);
        let rendered_width = UnicodeWidthStr::width(rendered.as_str());

        let edge_width = UnicodeWidthStr::width(LEFT_HALF_BLOCK);

        let tab_text = format!(
            "{}{}{} {} {}{}{}",
            ANSI_BOLD,
            bg.to_ansi_bg(),
            fg.to_ansi_fg(),
            rendered,
            bar_bg.to_ansi_bg(),
            bg.to_ansi_fg(),
            LEFT_HALF_BLOCK,
        );

        text.push_str(&tab_text);
        let tab_cells = 2 + rendered_width + edge_width;

        // Tabs are ordered by position; switch_tab_to is 1-based.
        clicks.push(
            total_width,
            total_width + tab_cells,
            ClickAction::SwitchToTab((i + 1) as u32),
        );
        total_width += tab_cells;
    }

    if let Some(ref scroll) = layout.scroll {
        if scroll.has_right {
            // " ▸▌" — styled like a non-active tab, no trailing space before the edge
            text.push_str(&format!(
                "{}{}{} {}{}{}{}",
                ANSI_BOLD,
                config.tab_bg.to_ansi_bg(),
                config.tab_fg.to_ansi_fg(),
                SCROLL_RIGHT_ARROW,
                bar_bg.to_ansi_bg(),
                config.tab_bg.to_ansi_fg(),
                LEFT_HALF_BLOCK,
            ));
            // `scroll.has_right` implies `scroll.right < tab_count - 1`, so
            // `scroll.right + 2` is in-range as a 1-based index.
            clicks.push(
                total_width,
                total_width + CHEVRON_TAB_WIDTH,
                ClickAction::SwitchToTab((scroll.right + 2) as u32),
            );
            total_width += CHEVRON_TAB_WIDTH;
        }
    }

    RenderedSide {
        text,
        width: total_width,
        clicks,
    }
}

pub fn build_right_side(state: &AppState, config: &Config, cols: usize) -> RenderedSide {
    let bar_bg = resolve_bar_bg(state, config);

    // Drive floating-dialog indicators. Search and Rename hold the client in
    // `Normal` while they intercept text input, so the visible mode can come
    // from shared dialog state instead of the raw client mode.
    let effective_mode = if state.search_active || state.mode == InputMode::EnterSearch {
        InputMode::Search
    } else if state.rename_active {
        state.rename_mode
    } else {
        state.mode
    };

    let is_non_normal = effective_mode != InputMode::Normal;
    let session_present = !state.session_name.is_empty()
        && !state.session_name.eq_ignore_ascii_case("main")
        && state.session_name != config.default_session_name;

    let mode_present = is_non_normal;

    // ── Mode hint segment (Locked exit hint / Search options) ──────────────
    // Sits directly right of the mode indicator. Only Locked and Search carry a
    // hint, and both are non-Normal, so `mode_present` is always true here.
    let mode_hint_present = matches!(effective_mode, InputMode::Locked | InputMode::Search);
    let which_key_hint_present = state.which_key_suppressed
        && !matches!(effective_mode, InputMode::Normal | InputMode::Locked)
        && config.which_key_toggle_key.is_some();

    // ── Info widgets segment (None unless `system_info` is configured) ─────
    let system_info_block =
        config.status.system_info.as_ref().filter(|block| {
            system::is_visible(block.visibility, block.min_cols, state, config, cols)
        });

    // ── Date/time visibility ───────────────────────────────────────────────
    let dt = &config.status.date_time;
    let time_present = system::is_visible(dt.visibility, dt.min_cols, state, config, cols);

    // The widgets segment is added *only* if it actually produces something —
    // we discover that lazily by calling `info_widgets_segment` later. To
    // compute the gradient correctly we need the count up front, so first
    // do a dry-run: build the segment with a placeholder bg and remember
    // whether it produced anything.
    //
    // (We build it twice to keep the gradient step count exact — info
    // widgets that have no samples yet should not steal a gradient stop.)
    let widgets_segment_probe = system_info_block
        .and_then(|block| info_widgets_segment(state, config, block, bar_bg, cols));
    let widgets_present = widgets_segment_probe.is_some();

    let segment_count = mode_present as usize
        + mode_hint_present as usize
        + which_key_hint_present as usize
        + session_present as usize
        + widgets_present as usize
        + time_present as usize;

    if segment_count == 0 {
        return RenderedSide {
            text: String::new(),
            width: 0,
            clicks: ClickMap::new(),
        };
    }

    let base_color = if is_non_normal {
        config.mode_color(effective_mode).unwrap_or(config.tab_bg)
    } else {
        config.tab_bg
    };

    let grad_stops = gradient(base_color, bar_bg, segment_count + 1);

    let mut specs: Vec<Color> = Vec::with_capacity(segment_count);
    let mut position = 0usize;
    let mut next_spec = || {
        let bg = grad_stops[segment_count - 1 - position];
        position += 1;
        bg
    };

    if mode_present {
        specs.push(next_spec());
    }
    if mode_hint_present {
        specs.push(next_spec());
    }
    if which_key_hint_present {
        specs.push(next_spec());
    }
    if session_present {
        specs.push(next_spec());
    }
    if widgets_present {
        specs.push(next_spec());
    }
    if time_present {
        specs.push(next_spec());
    }

    enum Built {
        Mode(Segment),
        Hint(Segment),
        Session(Segment),
        Widgets(InfoWidgetsResult),
        Time(Segment),
    }

    let mut built: Vec<Built> = Vec::with_capacity(segment_count);
    let mut spec_iter = specs.into_iter();

    // The mode indicator's background doubles as the "darker" label/off-glyph
    // color for the hint segment (it's one gradient stop darker than the hint's
    // own bg). Capture it as we consume the mode spec.
    let mut mode_bg = base_color;
    if mode_present {
        let bg = spec_iter.next().unwrap();
        mode_bg = bg;
        if let Some(seg) = mode_segment(effective_mode, bg, config) {
            built.push(Built::Mode(seg));
        }
    }
    if mode_hint_present {
        let bg = spec_iter.next().unwrap();
        let seg = match effective_mode {
            InputMode::Locked => locked_hint_segment(bg, mode_bg, config.hint_glyph_on),
            _ => search_hint_segment(
                bg,
                mode_bg,
                config.hint_glyph_on,
                state.search_case_sensitive,
                state.search_whole_word,
                state.search_wrap,
            ),
        };
        built.push(Built::Hint(seg));
    }
    if which_key_hint_present {
        let bg = spec_iter.next().unwrap();
        let key = config
            .which_key_toggle_key
            .as_deref()
            .expect("which_key_toggle_key present");
        built.push(Built::Hint(which_key_hidden_hint_segment(
            bg,
            mode_bg,
            config.hint_glyph_on,
            key,
        )));
    }
    if session_present {
        let bg = spec_iter.next().unwrap();
        if let Some(seg) = session_segment(&state.session_name, bg, config) {
            built.push(Built::Session(seg));
        }
    }
    if widgets_present {
        let bg = spec_iter.next().unwrap();
        // Re-render with the correct gradient bg. Safe to unwrap: probe was Some.
        if let Some(result) = info_widgets_segment(
            state,
            config,
            system_info_block.expect("system_info present"),
            bg,
            cols,
        ) {
            built.push(Built::Widgets(result));
        }
    }
    if time_present {
        let bg = spec_iter.next().unwrap();
        // Offset is populated asynchronously by `system::maybe_refresh_tz_offset`.
        // Until the first `date +%z` lands we render in UTC, which is briefly
        // wrong on first paint but corrects itself within one timer tick.
        let tz_offset_seconds = state.tz_offset.value.unwrap_or(0);
        built.push(Built::Time(time_segment(bg, dt, tz_offset_seconds)));
    }

    let mut text = String::new();
    let mut total_width = 0usize;
    let mut clicks = ClickMap::new();

    for (idx, item) in built.iter().enumerate() {
        let seg_bg = match item {
            Built::Mode(s) | Built::Hint(s) | Built::Session(s) | Built::Time(s) => s.bg,
            Built::Widgets(r) => r.segment.bg,
        };
        let left_bg = if idx == 0 {
            bar_bg
        } else {
            match &built[idx - 1] {
                Built::Mode(s) | Built::Hint(s) | Built::Session(s) | Built::Time(s) => s.bg,
                Built::Widgets(r) => r.segment.bg,
            }
        };
        let div = divider(left_bg, seg_bg);
        let dw = divider_width();

        let seg_start_with_div = total_width;
        text.push_str(&div);
        total_width += dw;
        let seg_text_start = total_width;

        match item {
            Built::Mode(s) => {
                text.push_str(&s.text);
                total_width += s.width;
                clicks.push(
                    seg_start_with_div,
                    total_width,
                    ClickAction::SwitchToMode(InputMode::Normal),
                );
            }
            Built::Hint(s) | Built::Session(s) | Built::Time(s) => {
                text.push_str(&s.text);
                total_width += s.width;
                if matches!(item, Built::Time(_)) {
                    if let Some(idx) = config.status.date_time.on_click {
                        clicks.push(
                            seg_start_with_div,
                            total_width,
                            ClickAction::RunCommand(idx),
                        );
                    }
                }
            }
            Built::Widgets(r) => {
                text.push_str(&r.segment.text);
                total_width += r.segment.width;
                for region in &r.click_regions {
                    clicks.push(
                        seg_text_start + region.start,
                        seg_text_start + region.end,
                        ClickAction::RunCommand(region.on_click),
                    );
                }
            }
        }
    }

    RenderedSide {
        text,
        width: total_width,
        clicks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::config::Config;
    use crate::bar::state::AppState;

    #[test]
    fn empty_state_renders() {
        let state = AppState::default();
        let config = Config::default();
        let bar = render_bar(&state, &config, 80);
        assert!(!bar.text.is_empty());
        assert!(bar.text.contains(ANSI_RESET));
    }

    #[test]
    fn right_side_empty_in_normal_mode_main_session_narrow() {
        let state = AppState {
            mode: InputMode::Normal,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert_eq!(right.width, 0);
    }

    #[test]
    fn right_side_has_mode_in_locked() {
        let state = AppState {
            mode: InputMode::Locked,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert!(right.width > 0);
    }

    #[test]
    fn locked_mode_shows_exit_hint() {
        let state = AppState {
            mode: InputMode::Locked,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert!(right.text.contains("exit"));
    }

    #[test]
    fn search_mode_shows_option_hint() {
        let state = AppState {
            mode: InputMode::Normal,
            search_active: true,
            search_whole_word: true,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert!(right.text.contains("case"));
        assert!(right.text.contains("word"));
        assert!(right.text.contains("wrap"));
    }

    #[test]
    fn suppressed_which_key_shows_toggle_hint() {
        let state = AppState {
            mode: InputMode::Scroll,
            which_key_suppressed: true,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "which_key".to_string(),
            r#"
toggle_key "Alt ."
"#
            .to_string(),
        );
        let config = Config::from_map(map);
        let right = build_right_side(&state, &config, 80);
        assert!(right.text.contains("\u{F0635} .")); // Alt+.
        assert!(right.text.contains("keys"));
    }

    #[test]
    fn suppressed_which_key_hint_requires_configured_toggle_key() {
        let state = AppState {
            mode: InputMode::Scroll,
            which_key_suppressed: true,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert!(!right.text.contains("keys"));
    }

    #[test]
    fn gap_fills_remaining_width() {
        use zellij_tile::prelude::{PaletteColor, Style};

        // No `bar_bg` override → the gap fill tracks the live Style background.
        // The stock Catppuccin theme reports base `#1e1e2e`, which equals the
        // historical `BAR_BG` default, so the bar is unchanged by this wiring.
        let mut style = Style::default();
        style.colors.text_unselected.background = PaletteColor::Rgb((30, 30, 46));
        let state = AppState {
            style: Some(style),
            ..AppState::default()
        };
        let config = Config::default();
        assert_eq!(config.bar_bg, None);

        let bar = render_bar(&state, &config, 80);

        let expected = resolve_bar_bg(&state, &config);
        assert_eq!(expected, DEFAULT_BAR_BG);
        assert!(bar.text.contains(&expected.to_ansi_bg()));
    }

    fn tab(position: usize, name: &str, active: bool) -> zellij_tile::prelude::TabInfo {
        zellij_tile::prelude::TabInfo {
            position,
            name: name.to_string(),
            active,
            // Use a tab_id distinct from `position` to catch any code that
            // accidentally uses one in place of the other.
            tab_id: position + 100,
            ..Default::default()
        }
    }

    #[test]
    fn tab_clicks_map_to_1_based_indices() {
        let state = AppState {
            tabs: vec![
                tab(0, "alpha", true),
                tab(1, "beta", false),
                tab(2, "gamma", false),
            ],
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let bar = render_bar(&state, &config, 80);

        // The first tab's click region starts at column 0.
        match bar.click_map.lookup(1) {
            Some(ClickAction::SwitchToTab(idx)) => assert_eq!(idx, 1),
            other => panic!("expected SwitchToTab(1) at col 1, got {:?}", other),
        }
        // The right edge of the bar (in normal mode, main session, narrow
        // width) has no segments, so it should have no click action.
        assert!(bar.click_map.lookup(79).is_none());
    }

    #[test]
    fn mode_segment_click_resets_to_normal() {
        let state = AppState {
            mode: InputMode::Locked,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let bar = render_bar(&state, &config, 80);

        // Mode segment is the only right-side segment in this state. It sits
        // at the right edge of the bar. Probe near the right edge.
        let action = (40..80).rev().find_map(|col| bar.click_map.lookup(col));
        match action {
            Some(ClickAction::SwitchToMode(InputMode::Normal)) => {}
            other => panic!("expected SwitchToMode(Normal), got {:?}", other),
        }
    }

    #[test]
    fn empty_normal_bar_has_no_click_regions() {
        let state = AppState {
            mode: InputMode::Normal,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let bar = render_bar(&state, &config, 80);
        assert!(bar.click_map.is_empty());
    }

    #[test]
    fn scroll_chevrons_have_click_regions_targeting_adjacent_tabs() {
        // 20 tabs with long names — forces the layout to scroll and render
        // both left and right chevrons around the active tab.
        let tabs: Vec<_> = (0..20)
            .map(|i| {
                let active = i == 10;
                tab(i, &format!("verylongtabname-{:02}", i), active)
            })
            .collect();

        let state = AppState {
            tabs,
            session_name: "main".to_string(),
            ..AppState::default()
        };
        let config = Config::default();
        let bar = render_bar(&state, &config, 60);

        // The left chevron occupies the first 3 cells of the bar.
        match bar.click_map.lookup(0) {
            Some(ClickAction::SwitchToTab(idx)) => {
                assert!(idx >= 1 && (idx as usize) <= 20);
                // Should target a tab to the left of the active one.
                assert!(idx <= 11);
            }
            other => panic!(
                "expected left chevron click region at col 0, got {:?}",
                other
            ),
        }
    }
}
