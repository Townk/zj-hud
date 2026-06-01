//! Layout module for zj-statusbar.
//!
//! Implements tab width equalization and scrolling window algorithms for
//! distributing available terminal columns across visible tab labels.

use crate::bar::config::Config;
use crate::bar::state::ScrollState;

/// Cell width of one scroll-indicator tab (" ◂▌" or " ▸▌").
/// Must match the actual rendered width in render.rs.
pub const CHEVRON_TAB_WIDTH: usize = 3;

// ─── Data Structures ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TabWidth {
    pub index: usize,
    pub width: usize,
    pub natural: usize,
}

pub struct TabLayout {
    pub widths: Vec<usize>,          // width per tab (0 = hidden)
    pub scroll: Option<ScrollState>, // from crate::bar::state
}

// ─── Algorithm 1: equalize_tab_widths (spec §5.4.4) ──────────────────────────

/// Shrink tab widths so they fit within `available` columns.
///
/// Shrinkable tabs lose columns together, stopping at
/// `min(natural, min_shrink_width)`. If `allow_below_min` is false and those
/// floors cannot fit, widths are left at their floors and `false` is returned.
/// When `allow_below_min` is true, the remaining overflow is shared below the
/// floor as evenly as possible.
pub fn equalize_tab_widths(
    tab_widths: &mut [TabWidth],
    available: usize,
    min_shrink_width: usize,
    allow_below_min: bool,
) -> bool {
    let total: usize = tab_widths.iter().map(|t| t.width).sum();
    if total <= available {
        return true; // already fits
    }

    let floors = tab_widths
        .iter()
        .map(|tw| tw.natural.min(min_shrink_width))
        .collect::<Vec<_>>();

    shrink_evenly_to_floors(tab_widths, &floors, available);

    let floor_sum: usize = floors.iter().sum();
    if floor_sum > available {
        if !allow_below_min {
            return false;
        }
        let zero_floors = vec![0; tab_widths.len()];
        shrink_evenly_to_floors(tab_widths, &zero_floors, available);
    }

    tab_widths.iter().map(|t| t.width).sum::<usize>() <= available
}

fn shrink_evenly_to_floors(tab_widths: &mut [TabWidth], floors: &[usize], available: usize) {
    loop {
        let total: usize = tab_widths.iter().map(|t| t.width).sum();
        if total <= available {
            break;
        }

        let active = tab_widths
            .iter()
            .enumerate()
            .filter_map(|(i, tw)| (tw.width > floors[i]).then_some(i))
            .collect::<Vec<_>>();

        if active.is_empty() {
            break;
        }

        let overflow = total - available;
        let base_reduction = overflow / active.len();

        if base_reduction == 0 {
            for &idx in active.iter().rev().take(overflow) {
                tab_widths[idx].width -= 1;
            }
            break;
        }

        let shared_reduction = active
            .iter()
            .map(|&idx| tab_widths[idx].width - floors[idx])
            .min()
            .unwrap_or(0)
            .min(base_reduction);

        for &idx in &active {
            tab_widths[idx].width -= shared_reduction;
        }
    }
}

// ─── Algorithm 2: visible_tab_range (spec §5.4.5) ────────────────────────────

/// Compute the visible window [L, R] (inclusive) centered on `active_idx`.
///
/// `tab_widths` holds the per-tab width estimate to use when deciding whether
/// a candidate tab fits. The caller sets this to
/// `min(natural_width_capped, tab_min_shrink_width)` for inactive tabs and
/// `active_width` for the active tab so that:
///
/// - Small tabs (natural < tab_min_shrink_width, e.g. "~" at 8 cells) use their real
///   size — so many of them can be included in the visible window.
/// - Large tabs (natural ≥ tab_min_shrink_width) use tab_min_shrink_width as a lower bound —
///   preserving the original behaviour of fitting more tabs in truncated form.
pub fn visible_tab_range(
    active_idx: usize,
    tab_widths: &[usize],
    available_total: usize,
) -> (usize, usize) {
    let tab_count = tab_widths.len();
    if tab_count == 0 {
        return (0, 0);
    }
    let mut l = active_idx;
    let mut r = active_idx;
    let mut visible_sum = tab_widths[active_idx];

    loop {
        let mut expanded = false;
        let left_count = active_idx - l;
        let right_count = r - active_idx;

        if left_count <= right_count {
            // Try left first, then right
            if l > 0 {
                let w = tab_widths[l - 1];
                if visible_sum + w <= available_total {
                    l -= 1;
                    visible_sum += w;
                    expanded = true;
                }
            }
            if r + 1 < tab_count {
                let w = tab_widths[r + 1];
                if visible_sum + w <= available_total {
                    r += 1;
                    visible_sum += w;
                    expanded = true;
                }
            }
        } else {
            // Try right first, then left
            if r + 1 < tab_count {
                let w = tab_widths[r + 1];
                if visible_sum + w <= available_total {
                    r += 1;
                    visible_sum += w;
                    expanded = true;
                }
            }
            if l > 0 {
                let w = tab_widths[l - 1];
                if visible_sum + w <= available_total {
                    l -= 1;
                    visible_sum += w;
                    expanded = true;
                }
            }
        }

        if !expanded {
            break;
        }
    }

    (l, r)
}

// ─── Algorithm 3: compute_tab_layout ─────────────────────────────────────────

/// Compute the full layout for all tabs.
///
/// Returns a `TabLayout` with a width per tab (0 = hidden) and an optional
/// `ScrollState` indicating which tabs are offscreen.
pub fn compute_tab_layout(
    tab_count: usize,
    active_idx: usize,
    natural_widths: &[usize],
    available_total: usize,
    config: &Config,
) -> TabLayout {
    if tab_count == 0 {
        return TabLayout {
            widths: Vec::new(),
            scroll: None,
        };
    }

    // 1. Active tab width is capped at tab_max_width and the total budget.
    let active_width = natural_widths[active_idx]
        .min(config.tab_max_width)
        .min(available_total);

    // 2. Budget for inactive tabs
    let available_inactive = available_total.saturating_sub(active_width);

    // 3. Build inactive TabWidth list (all tabs except active)
    let mut inactive: Vec<TabWidth> = natural_widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != active_idx)
        .map(|(i, &w)| TabWidth {
            index: i,
            width: w.min(config.tab_max_width),
            natural: w.min(config.tab_max_width),
        })
        .collect();

    let total_needed: usize = inactive.iter().map(|t| t.width).sum();

    // 4. If everything fits naturally, return natural widths, no scroll
    if total_needed <= available_inactive {
        let mut widths = natural_widths
            .iter()
            .map(|&w| w.min(config.tab_max_width))
            .collect::<Vec<_>>();
        widths[active_idx] = active_width;
        return TabLayout {
            widths,
            scroll: None,
        };
    }

    // 5. Equalize inactive tabs without crossing their readable floor. If that
    // floor cannot fit, scrolling is needed.
    let fits_without_scroll = equalize_tab_widths(
        &mut inactive,
        available_inactive,
        config.tab_min_shrink_width,
        false,
    );

    if fits_without_scroll {
        // No scrolling needed — build widths from equalized inactive list
        let mut widths = vec![0usize; tab_count];
        widths[active_idx] = active_width;
        for tw in &inactive {
            widths[tw.index] = tw.width;
        }
        return TabLayout {
            widths,
            scroll: None,
        };
    }

    // 6. Scrolling needed.
    //
    // Build per-tab range estimates: the active tab uses its real width; each
    // inactive tab uses min(natural_capped, tab_min_shrink_width). This lets
    // small tabs (natural < tab_min_shrink_width, like "~") fill the visible
    // window with their actual small size instead of being over-estimated.
    let mut range_widths: Vec<usize> = natural_widths
        .iter()
        .map(|&w| w.min(config.tab_max_width).min(config.tab_min_shrink_width))
        .collect();
    range_widths[active_idx] = active_width;

    let (vis_l, vis_r) = visible_tab_range(active_idx, &range_widths, available_total);

    // Determine if chevrons will actually be needed
    let has_left = vis_l > 0;
    let has_right = vis_r < tab_count - 1;

    let chevron_space = if has_left { CHEVRON_TAB_WIDTH } else { 0 }
        + if has_right { CHEVRON_TAB_WIDTH } else { 0 };
    let available_for_tabs = available_total.saturating_sub(chevron_space);

    let (final_l, final_r) = visible_tab_range(active_idx, &range_widths, available_for_tabs);

    let final_has_left = final_l > 0;
    let final_has_right = final_r < tab_count - 1;

    let final_chevron_space = if final_has_left { CHEVRON_TAB_WIDTH } else { 0 }
        + if final_has_right {
            CHEVRON_TAB_WIDTH
        } else {
            0
        };
    let final_active_width = active_width.min(available_total.saturating_sub(final_chevron_space));

    // Re-equalize only visible inactive tabs within the remaining budget
    let visible_inactive_budget =
        available_total.saturating_sub(final_chevron_space + final_active_width);

    let mut visible_inactive: Vec<TabWidth> = (final_l..=final_r)
        .filter(|&i| i != active_idx)
        .map(|i| TabWidth {
            index: i,
            width: natural_widths[i].min(config.tab_max_width),
            natural: natural_widths[i].min(config.tab_max_width),
        })
        .collect();

    equalize_tab_widths(
        &mut visible_inactive,
        visible_inactive_budget,
        config.tab_min_shrink_width,
        true,
    );

    // Build the final widths vector: 0 for hidden tabs
    let mut widths = vec![0usize; tab_count];
    widths[active_idx] = final_active_width;
    for tw in &visible_inactive {
        widths[tw.index] = tw.width;
    }

    TabLayout {
        widths,
        scroll: Some(ScrollState {
            left: final_l,
            right: final_r,
            has_left: final_has_left,
            has_right: final_has_right,
        }),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tabs(widths: &[usize]) -> Vec<TabWidth> {
        widths
            .iter()
            .enumerate()
            .map(|(i, &w)| TabWidth {
                index: i,
                width: w,
                natural: w,
            })
            .collect()
    }

    fn total_width(tabs: &[TabWidth]) -> usize {
        tabs.iter().map(|t| t.width).sum()
    }

    #[test]
    fn equalize_no_op_when_fits() {
        let mut tabs = make_tabs(&[10, 15, 20]);
        assert!(equalize_tab_widths(&mut tabs, 50, 20, false));
        assert_eq!(tabs[0].width, 10);
        assert_eq!(tabs[1].width, 15);
        assert_eq!(tabs[2].width, 20);
    }

    #[test]
    fn equalize_shrinks_to_budget() {
        let mut tabs = make_tabs(&[20, 20, 20]);
        assert!(equalize_tab_widths(&mut tabs, 30, 0, true));
        assert_eq!(total_width(&tabs), 30);
    }

    #[test]
    fn equalize_shrinks_evenly_without_left_bias() {
        let mut tabs = make_tabs(&[10, 30, 30]);
        assert!(equalize_tab_widths(&mut tabs, 50, 20, false));
        assert_eq!(total_width(&tabs), 50);
        assert_eq!(tabs[0].width, 10);
        assert_eq!(tabs[1].width, 20);
        assert_eq!(tabs[2].width, 20);
    }

    #[test]
    fn equalize_can_shrink_below_floor_when_allowed() {
        let mut tabs = make_tabs(&[10, 10, 10]);
        assert!(equalize_tab_widths(&mut tabs, 21, 10, true));
        assert_eq!(total_width(&tabs), 21);
        assert_eq!(tabs[0].width, 7);
        assert_eq!(tabs[1].width, 7);
        assert_eq!(tabs[2].width, 7);
    }

    #[test]
    fn equalize_below_floor_with_remainder() {
        let mut tabs = make_tabs(&[10, 10, 10]);
        assert!(equalize_tab_widths(&mut tabs, 20, 10, true));
        assert_eq!(total_width(&tabs), 20);
        assert_eq!(tabs[0].width, 7);
        assert_eq!(tabs[1].width, 7);
        assert_eq!(tabs[2].width, 6);
    }

    #[test]
    fn equalize_reports_when_floor_cannot_fit() {
        let mut tabs = make_tabs(&[30, 30, 30, 30]);
        assert!(!equalize_tab_widths(&mut tabs, 70, 20, false));
        assert_eq!(
            tabs.iter().map(|t| t.width).collect::<Vec<_>>(),
            vec![20; 4]
        );
    }

    #[test]
    fn scroll_single_tab() {
        let widths = vec![30usize; 1];
        let (l, r) = visible_tab_range(0, &widths, 80);
        assert_eq!((l, r), (0, 0));
    }

    #[test]
    fn scroll_centers_on_active() {
        // active tab at idx 4 is wider; inactive use 12-cell estimates
        let mut widths = vec![12usize; 10];
        widths[4] = 30;
        let (l, r) = visible_tab_range(4, &widths, 90);
        assert!(l <= 4 && r >= 4);
    }

    #[test]
    fn scroll_active_at_start() {
        let mut widths = vec![12usize; 10];
        widths[0] = 30;
        let (l, _r) = visible_tab_range(0, &widths, 66);
        assert_eq!(l, 0);
    }

    #[test]
    fn scroll_active_at_end() {
        let mut widths = vec![12usize; 10];
        widths[9] = 30;
        let (_l, r) = visible_tab_range(9, &widths, 66);
        assert_eq!(r, 9);
    }

    #[test]
    fn scroll_small_tabs_fill_window() {
        // Home tabs have natural_width 8; the visible window should fit many more
        // than the old MIN_TAB_WIDTH=12 estimate would allow.
        let widths = vec![8usize; 13]; // 13 home-sized tabs, active at 0
                                       // available=100: 8 + 11*8=96 <= 100 → 12 tabs; 8+12*8=104 > 100 → stop
        let (l, r) = visible_tab_range(0, &widths, 100);
        assert_eq!(l, 0);
        assert_eq!(r, 11); // 12 tabs visible
    }

    #[test]
    fn layout_all_fit() {
        let config = Config::default();
        let layout = compute_tab_layout(3, 1, &[20, 25, 15], 100, &config);
        assert!(layout.scroll.is_none());
    }

    #[test]
    fn layout_equalization_needed() {
        let config = Config::default();
        let layout = compute_tab_layout(3, 0, &[30, 30, 30], 70, &config);
        assert!(layout.scroll.is_none());
        assert_eq!(layout.widths, vec![30, 20, 20]);
    }

    #[test]
    fn layout_scrolling_needed() {
        let config = Config::default();
        let widths = vec![30; 20];
        let layout = compute_tab_layout(20, 10, &widths, 80, &config);
        assert!(layout.scroll.is_some());
        let scroll = layout.scroll.unwrap();
        assert!(scroll.left <= 10);
        assert!(scroll.right >= 10);
    }

    #[test]
    fn configured_min_shrink_width_triggers_scrolling() {
        let config = Config {
            tab_min_shrink_width: 20,
            ..Config::default()
        };
        let layout = compute_tab_layout(6, 0, &[30, 30, 30, 30, 30, 30], 100, &config);

        assert!(layout.scroll.is_some());
    }

    #[test]
    fn naturally_small_tabs_do_not_grow_to_min_shrink_width() {
        let config = Config {
            tab_min_shrink_width: 20,
            ..Config::default()
        };
        let layout = compute_tab_layout(12, 0, &[8; 12], 80, &config);
        let scroll = layout.scroll.unwrap();

        assert_eq!(scroll.left, 0);
        assert_eq!(scroll.right, 8);
        assert!(layout.widths.iter().all(|&width| width == 0 || width == 8));
    }

    #[test]
    fn inactive_tabs_shrink_together_while_active_stays_full() {
        let config = Config {
            tab_min_shrink_width: 20,
            ..Config::default()
        };
        let layout = compute_tab_layout(7, 6, &[8, 8, 29, 29, 29, 29, 29], 137, &config);

        assert!(layout.scroll.is_none());
        assert_eq!(layout.widths, vec![8, 8, 23, 23, 23, 23, 29]);
    }

    #[test]
    fn layout_clamps_active_tab_to_available_width() {
        let config = Config::default();
        let layout = compute_tab_layout(3, 1, &[30, 40, 30], 8, &config);
        assert!(layout.widths.iter().sum::<usize>() <= 8);
        assert!(layout.widths[1] <= 8);
    }

    #[test]
    fn layout_with_scroll_indicators_fits_tiny_budget() {
        let config = Config::default();
        let layout = compute_tab_layout(20, 10, &[30; 20], 8, &config);
        let indicator_width = layout
            .scroll
            .as_ref()
            .map(|scroll| {
                (scroll.has_left as usize + scroll.has_right as usize) * CHEVRON_TAB_WIDTH
            })
            .unwrap_or(0);
        assert!(layout.widths.iter().sum::<usize>() + indicator_width <= 8);
    }
}
