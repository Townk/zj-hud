use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::InputMode;

use crate::click_map::{ClickAction, ClickMap};
use crate::color::{gradient, Color};
use crate::config::Config;
use crate::icons::{LEFT_HALF_BLOCK, SCROLL_LEFT_ARROW, SCROLL_RIGHT_ARROW};
use crate::layout::{compute_tab_layout, CHEVRON_TAB_WIDTH};
use crate::segments::{
    divider, divider_width, info_widgets_segment, mode_segment, session_segment, time_segment,
    InfoWidgetsResult, Segment,
};
use crate::state::AppState;
use crate::system;
use crate::tabs::{compose_tab_title, render_tab_title};

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";

const TAB_BG: Color = Color {
    r: 40,
    g: 44,
    b: 65,
};
const TAB_FG: Color = Color {
    r: 155,
    g: 159,
    b: 193,
};
const ACTIVE_TAB_BG: Color = Color {
    r: 101,
    g: 106,
    b: 131,
};
const ACTIVE_TAB_FG: Color = Color {
    r: 255,
    g: 255,
    b: 255,
};
const BAR_BG: Color = Color {
    r: 30,
    g: 30,
    b: 46,
};

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

pub fn render_bar(state: &AppState, config: &Config, cols: usize) -> RenderedBar {
    let right = build_right_side(state, config, cols);
    let left_budget = cols.saturating_sub(right.width);
    let left = build_left_side(state, config, left_budget);

    let used = left.width + right.width;
    let gap_cols = cols.saturating_sub(used);
    let gap = format!("{}{}", BAR_BG.to_ansi_bg(), " ".repeat(gap_cols));

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
                TAB_BG.to_ansi_bg(),
                TAB_FG.to_ansi_fg(),
                SCROLL_LEFT_ARROW,
                BAR_BG.to_ansi_bg(),
                TAB_BG.to_ansi_fg(),
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
            (ACTIVE_TAB_BG, ACTIVE_TAB_FG)
        } else {
            (TAB_BG, TAB_FG)
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
            BAR_BG.to_ansi_bg(),
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
                TAB_BG.to_ansi_bg(),
                TAB_FG.to_ansi_fg(),
                SCROLL_RIGHT_ARROW,
                BAR_BG.to_ansi_bg(),
                TAB_BG.to_ansi_fg(),
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
    // Drive the Search indicator from the client mode: the mode-driven search
    // dialog keeps the client in `EnterSearch` while typing, then settles in
    // `Search` for `n`/`N` navigation — collapse both to the Search indicator
    // so the bar reads "search" the whole time the dialog is up.
    let effective_mode = if state.mode == InputMode::EnterSearch {
        InputMode::Search
    } else {
        state.mode
    };

    let is_non_normal = effective_mode != InputMode::Normal;
    let session_present = !state.session_name.is_empty()
        && !state.session_name.eq_ignore_ascii_case("main")
        && state.session_name != config.default_session_name;

    let mode_present = is_non_normal;

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
        .and_then(|block| info_widgets_segment(state, config, block, BAR_BG, false, cols));
    let widgets_present = widgets_segment_probe.is_some();

    let segment_count = mode_present as usize
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
        config.mode_color(effective_mode).unwrap_or(TAB_BG)
    } else {
        TAB_BG
    };

    let grad_stops = gradient(base_color, BAR_BG, segment_count + 1);

    struct SegmentSpec {
        bg: Color,
        is_last: bool,
    }

    let mut specs: Vec<SegmentSpec> = Vec::with_capacity(segment_count);
    let mut position = 0usize;
    let mut next_spec = || {
        let bg = grad_stops[segment_count - 1 - position];
        position += 1;
        SegmentSpec { bg, is_last: false }
    };

    if mode_present {
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

    if let Some(last) = specs.last_mut() {
        last.is_last = true;
    }

    enum Built {
        Mode(Segment),
        Session(Segment),
        Widgets(InfoWidgetsResult),
        Time(Segment),
    }

    let mut built: Vec<Built> = Vec::with_capacity(segment_count);
    let mut spec_iter = specs.into_iter();

    if mode_present {
        let s = spec_iter.next().unwrap();
        if let Some(seg) = mode_segment(effective_mode, s.bg, s.is_last, config) {
            built.push(Built::Mode(seg));
        }
    }
    if session_present {
        let s = spec_iter.next().unwrap();
        if let Some(seg) = session_segment(&state.session_name, s.bg, s.is_last, config) {
            built.push(Built::Session(seg));
        }
    }
    if widgets_present {
        let s = spec_iter.next().unwrap();
        // Re-render with the correct gradient bg. Safe to unwrap: probe was Some.
        if let Some(result) = info_widgets_segment(
            state,
            config,
            system_info_block.expect("system_info present"),
            s.bg,
            s.is_last,
            cols,
        ) {
            built.push(Built::Widgets(result));
        }
    }
    if time_present {
        let s = spec_iter.next().unwrap();
        // Offset is populated asynchronously by `system::maybe_refresh_tz_offset`.
        // Until the first `date +%z` lands we render in UTC, which is briefly
        // wrong on first paint but corrects itself within one timer tick.
        let tz_offset_seconds = state.tz_offset.value.unwrap_or(0);
        built.push(Built::Time(time_segment(
            s.bg,
            s.is_last,
            dt,
            tz_offset_seconds,
        )));
    }

    let mut text = String::new();
    let mut total_width = 0usize;
    let mut clicks = ClickMap::new();

    for (idx, item) in built.iter().enumerate() {
        let seg_bg = match item {
            Built::Mode(s) | Built::Session(s) | Built::Time(s) => s.bg,
            Built::Widgets(r) => r.segment.bg,
        };
        let left_bg = if idx == 0 {
            BAR_BG
        } else {
            match &built[idx - 1] {
                Built::Mode(s) | Built::Session(s) | Built::Time(s) => s.bg,
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
            Built::Session(s) | Built::Time(s) => {
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
    use crate::config::Config;
    use crate::state::AppState;

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
    fn gap_fills_remaining_width() {
        let state = AppState::default();
        let config = Config::default();
        let bar = render_bar(&state, &config, 80);
        assert!(bar.text.contains(&BAR_BG.to_ansi_bg()));
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
