//! Grid layout: arrange merged [`Entry`]s into paginated body lines honoring
//! the width-derived column mode, `sort_by`, and `max_height` config.
//!
//! Pure and host-testable. Each rendered column snugs to its own widest cell,
//! and pagination packs as many ordered entries as fit within `inner_width`.

use crate::whichkey::config::SortBy;
use crate::whichkey::labels::Entry;
use crate::whichkey::theme::Theme;

/// One space between a cell's segments: `<keys> <sep> <icon?> <label>`.
const SEG_GAP: usize = 1;
/// Two spaces between adjacent grid columns.
const COL_GAP: usize = 2;

/// How many binding columns the grid may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Columns {
    /// Fit as many columns as the text budget allows.
    Auto,
    /// Cap the planner at a fixed number of columns.
    Fixed(usize),
}

/// A rendered cell and its **visible** width (display columns, excluding any
/// SGR color escapes) so column padding and `content_width` stay correct when
/// the text is colored.
struct Cell {
    text: String,
    width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColumnWidths {
    keys_w: usize,
    icon_w: usize,
    label_w: usize,
    cell_w: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PagePlan {
    entry_count: usize,
    cols: usize,
    rows: usize,
    width: usize,
    col_widths: Vec<ColumnWidths>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Body lines for each page (no separator/footer).
    pub pages: Vec<Vec<String>>,
    /// Widest body line across all pages (display chars).
    pub content_width: usize,
    /// Widest body line on each page (display chars), index-aligned with
    /// `pages`. Lets the binary snug the pane to the current page.
    pub page_widths: Vec<usize>,
}

impl Layout {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

pub fn lay_out(
    entries: &[Entry],
    inner_width: usize,
    columns: Columns,
    sort_by: SortBy,
    max_rows: usize,
    separator: &str,
    theme: Option<&Theme>,
) -> Layout {
    if entries.is_empty() {
        return Layout {
            pages: Vec::new(),
            content_width: 0,
            page_widths: Vec::new(),
        };
    }

    let sep_w = separator.chars().count();
    let plans = plan_pages(
        entries,
        inner_width,
        columns,
        max_rows.max(1),
        sort_by,
        sep_w,
    );
    let mut pages = Vec::new();
    let mut page_widths = Vec::new();
    let mut content_width = 0usize;
    let mut offset = 0usize;
    for plan in plans {
        let chunk = &entries[offset..offset + plan.entry_count];
        let (lines, page_w) = lay_out_page(chunk, &plan, sort_by, separator, theme);
        content_width = content_width.max(page_w);
        page_widths.push(page_w);
        pages.push(lines);
        offset += plan.entry_count;
    }

    Layout {
        pages,
        content_width,
        page_widths,
    }
}

/// Widths for a set of entries in a single rendered column. When no entry has
/// an icon, the icon column (and its trailing gap) collapses to nothing.
fn col_widths<'a>(entries: impl IntoIterator<Item = &'a Entry>, sep_w: usize) -> ColumnWidths {
    let mut keys_w = 0usize;
    let mut icon_w = 0usize;
    let mut label_w = 0usize;
    for entry in entries {
        keys_w = keys_w.max(entry.keys_display().chars().count());
        if let Some(icon) = &entry.icon {
            icon_w = icon_w.max(icon.chars().count());
        }
        label_w = label_w.max(entry.label.chars().count());
    }
    let icon_col = if icon_w > 0 { icon_w + SEG_GAP } else { 0 };
    let cell_w = keys_w + SEG_GAP + sep_w + SEG_GAP + icon_col + label_w;
    ColumnWidths {
        keys_w,
        icon_w,
        label_w,
        cell_w,
    }
}

fn entry_index(sort_by: SortBy, row: usize, col: usize, cols: usize, rows: usize) -> usize {
    match sort_by {
        SortBy::Row => row * cols + col,
        SortBy::Column => col * rows + row,
    }
}

fn max_candidate_columns(
    entries: &[Entry],
    inner_width: usize,
    columns: Columns,
    sep_w: usize,
) -> usize {
    let explicit_max = match columns {
        Columns::Fixed(n) => n.max(1),
        Columns::Auto => {
            let min_cell_w = entries
                .iter()
                .map(|entry| col_widths(std::iter::once(entry), sep_w).cell_w)
                .min()
                .unwrap_or(1);
            fit_columns(min_cell_w, inner_width)
        }
    };
    explicit_max.min(entries.len().max(1)).max(1)
}

fn column_widths_for(
    entries: &[Entry],
    entry_count: usize,
    cols: usize,
    rows: usize,
    sort_by: SortBy,
    sep_w: usize,
) -> Vec<ColumnWidths> {
    (0..cols)
        .map(|col| {
            let column_entries = (0..rows).filter_map(|row| {
                let idx = entry_index(sort_by, row, col, cols, rows);
                (idx < entry_count).then(|| &entries[idx])
            });
            col_widths(column_entries, sep_w)
        })
        .collect()
}

fn planned_page_width(
    entries: &[Entry],
    entry_count: usize,
    cols: usize,
    rows: usize,
    sort_by: SortBy,
    sep_w: usize,
) -> (usize, Vec<ColumnWidths>) {
    let col_widths = column_widths_for(entries, entry_count, cols, rows, sort_by, sep_w);
    let mut max_w = 0usize;
    for row in 0..rows {
        let mut width = 0usize;
        for (col, widths) in col_widths.iter().enumerate() {
            let idx = entry_index(sort_by, row, col, cols, rows);
            if idx >= entry_count {
                break;
            }
            if col > 0 {
                width += COL_GAP;
            }
            width += widths.cell_w;
        }
        max_w = max_w.max(width);
    }
    (max_w, col_widths)
}

fn candidate_page_plans(
    entries: &[Entry],
    inner_width: usize,
    columns: Columns,
    rows: usize,
    sort_by: SortBy,
    sep_w: usize,
) -> Vec<PagePlan> {
    let max_cols = max_candidate_columns(entries, inner_width, columns, sep_w);
    let mut candidates = Vec::new();
    for cols in 1..=max_cols {
        let max_count = entries.len().min(cols * rows);
        for entry_count in (1..=max_count).rev() {
            let (width, col_widths) =
                planned_page_width(entries, entry_count, cols, rows, sort_by, sep_w);
            if cols > 1 && width > inner_width {
                continue;
            }
            candidates.push(PagePlan {
                entry_count,
                cols,
                rows,
                width,
                col_widths,
            });
        }
    }
    if candidates.is_empty() {
        let (width, col_widths) = planned_page_width(entries, 1, 1, rows, sort_by, sep_w);
        candidates.push(PagePlan {
            entry_count: 1,
            cols: 1,
            rows,
            width,
            col_widths,
        });
    }
    candidates
}

fn same_page_tie_break(candidate: &PagePlan, current: &PagePlan, columns: Columns) -> bool {
    if candidate.entry_count != current.entry_count {
        return candidate.entry_count > current.entry_count;
    }
    match columns {
        Columns::Fixed(_) => candidate.cols > current.cols,
        Columns::Auto => {
            candidate.width < current.width
                || (candidate.width == current.width && candidate.cols < current.cols)
        }
    }
}

fn plan_pages(
    entries: &[Entry],
    inner_width: usize,
    columns: Columns,
    rows: usize,
    sort_by: SortBy,
    sep_w: usize,
) -> Vec<PagePlan> {
    let n = entries.len();
    let mut best_pages = vec![usize::MAX / 2; n + 1];
    let mut best_plan: Vec<Option<PagePlan>> = vec![None; n + 1];
    best_pages[n] = 0;

    for offset in (0..n).rev() {
        for candidate in candidate_page_plans(
            &entries[offset..],
            inner_width,
            columns,
            rows,
            sort_by,
            sep_w,
        ) {
            let next = offset + candidate.entry_count;
            let page_count = 1 + best_pages[next];
            let replace = match best_plan[offset].as_ref() {
                None => true,
                Some(_) if page_count != best_pages[offset] => page_count < best_pages[offset],
                Some(current) => same_page_tie_break(&candidate, current, columns),
            };
            if replace {
                best_pages[offset] = page_count;
                best_plan[offset] = Some(candidate);
            }
        }
    }

    let mut plans = Vec::new();
    let mut offset = 0usize;
    while offset < n {
        let plan = best_plan[offset].clone().unwrap_or_else(|| {
            let (width, col_widths) =
                planned_page_width(&entries[offset..], 1, 1, rows, sort_by, sep_w);
            PagePlan {
                entry_count: 1,
                cols: 1,
                rows,
                width,
                col_widths,
            }
        });
        offset += plan.entry_count;
        plans.push(plan);
    }
    plans
}

/// Human-readable per-page layout diagnostics: the packed column count, entry
/// count, and per-column widths for each page. Used by the plugin's `debug_log`.
pub fn diagnostics(
    entries: &[Entry],
    inner_width: usize,
    columns: Columns,
    sort_by: SortBy,
    max_rows: usize,
    separator: &str,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if entries.is_empty() {
        out.push_str("  (no entries)\n");
        return out;
    }
    let sep_w = separator.chars().count();
    let rows = max_rows.max(1);
    let plans = plan_pages(entries, inner_width, columns, rows, sort_by, sep_w);
    let _ = writeln!(
        out,
        "  entries={} inner_width={inner_width} rows={rows} columns={columns:?}",
        entries.len()
    );
    let mut offset = 0usize;
    for (page, plan) in plans.iter().enumerate() {
        let chunk = &entries[offset..offset + plan.entry_count];
        let _ = writeln!(
            out,
            "  page {page}: entries={} cols={} width={}",
            plan.entry_count, plan.cols, plan.width
        );
        for (ci, widths) in plan.col_widths.iter().enumerate() {
            let _ = writeln!(
                out,
                "    col {ci}: keys_w={} icon_w={} label_w={} cell_w={}",
                widths.keys_w, widths.icon_w, widths.label_w, widths.cell_w
            );
        }
        for e in chunk {
            let kd = e.keys_display();
            let _ = writeln!(
                out,
                "    [{:>2} ch] {:?}  ->  {}",
                kd.chars().count(),
                kd,
                e.label
            );
        }
        offset += plan.entry_count;
    }
    out
}

/// Largest column count whose total width (columns + gaps) fits `inner_width`.
fn fit_columns(cell_w: usize, inner_width: usize) -> usize {
    if cell_w == 0 {
        return 1;
    }
    ((inner_width + COL_GAP) / (cell_w + COL_GAP)).max(1)
}

/// Lay a page's entries into lines, returning the lines and the page's widest
/// **visible** line width. Trailing empty cells are omitted (no trailing pad),
/// so the reported width matches what actually renders even when colored.
fn lay_out_page(
    chunk: &[Entry],
    plan: &PagePlan,
    sort_by: SortBy,
    separator: &str,
    theme: Option<&Theme>,
) -> (Vec<String>, usize) {
    let n = chunk.len();
    let mut lines = Vec::new();
    let mut max_w = 0usize;
    for r in 0..plan.rows {
        // Gather this row's populated cells. For both fill orders the populated
        // columns are contiguous from c=0, so we stop at the first empty slot.
        let mut cells = Vec::new();
        for c in 0..plan.cols {
            let idx = entry_index(sort_by, r, c, plan.cols, plan.rows);
            if idx >= n {
                break;
            }
            let widths = plan.col_widths[c];
            cells.push((
                render_cell(&chunk[idx], widths.keys_w, widths.icon_w, separator, theme),
                widths.cell_w,
            ));
        }
        if cells.is_empty() {
            continue;
        }
        let mut line = String::new();
        let mut width = 0usize;
        let last = cells.len() - 1;
        for (ci, (cell, cell_w)) in cells.iter().enumerate() {
            line.push_str(&cell.text);
            width += cell.width;
            if ci != last {
                let gap = cell_w.saturating_sub(cell.width) + COL_GAP;
                line.push_str(&" ".repeat(gap));
                width += gap;
            }
        }
        max_w = max_w.max(width);
        lines.push(line);
    }
    (lines, max_w)
}

/// A cell is `<keys> <sep> <icon?> <label>` with the keys **right-aligned**
/// within the column's key width and the icon **left-aligned** within `icon_w`
/// (so the labels line up). `icon_w == 0` drops the icon column entirely. When
/// `theme` is set the keys are bright white, the separator dim, and the label
/// takes the entry's color (blue for mode switches, else pink). The icon uses
/// the entry's own `icon_color` when set, otherwise the label color; pads/gaps
/// stay uncolored. `Cell::width` is always the visible width.
fn render_cell(
    entry: &Entry,
    keys_w: usize,
    icon_w: usize,
    separator: &str,
    theme: Option<&Theme>,
) -> Cell {
    let keys = entry.keys_display();
    let keys_len = keys.chars().count();
    let key_pad = keys_w.saturating_sub(keys_len);
    let sep_w = separator.chars().count();
    let icon = entry.icon.as_deref().unwrap_or("");
    let icon_len = icon.chars().count();

    let icon_col = if icon_w > 0 { icon_w + SEG_GAP } else { 0 };
    let width =
        key_pad + keys_len + SEG_GAP + sep_w + SEG_GAP + icon_col + entry.label.chars().count();

    let sp = |n: usize| " ".repeat(n);
    let gap = sp(SEG_GAP);
    let text = match theme {
        Some(t) => {
            let label_color = if entry.switch { &t.switch } else { &t.label };
            let icon_sgr = entry.icon_color.as_deref().unwrap_or(label_color.as_str());
            let mut s = format!(
                "{}{}{keys}{}{gap}{}{separator}{}{gap}",
                sp(key_pad),
                t.key,
                t.reset,
                t.dim,
                t.reset,
            );
            if icon_w > 0 {
                if icon.is_empty() {
                    s.push_str(&sp(icon_w));
                } else {
                    s.push_str(&format!("{icon_sgr}{icon}{}", t.reset));
                    s.push_str(&sp(icon_w - icon_len));
                }
                s.push_str(&gap);
            }
            s.push_str(&format!("{label_color}{}{}", entry.label, t.reset));
            s
        }
        None => {
            let mut s = format!("{}{keys}{gap}{separator}{gap}", sp(key_pad));
            if icon_w > 0 {
                s.push_str(icon);
                s.push_str(&sp(icon_w - icon_len));
                s.push_str(&gap);
            }
            s.push_str(&entry.label);
            s
        }
    };
    Cell { text, width }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test separator (1 visible cell) to keep width math simple.
    const SEP: &str = "-";

    fn entry(keys: &[&str], label: &str) -> Entry {
        Entry {
            keys: keys.iter().map(|s| s.to_string()).collect(),
            label: label.into(),
            icon: None,
            icon_color: None,
            switch: false,
        }
    }

    fn entry_icon(keys: &[&str], label: &str, icon: &str) -> Entry {
        Entry {
            keys: keys.iter().map(|s| s.to_string()).collect(),
            label: label.into(),
            icon: Some(icon.into()),
            icon_color: None,
            switch: false,
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry(&["k", "↑"], "focus up"),
            entry(&["j", "↓"], "focus down"),
            entry(&["h", "←"], "focus left"),
            entry(&["l", "→"], "focus right"),
        ]
    }

    #[test]
    fn single_column_when_narrow() {
        let layout = lay_out(&sample(), 20, Columns::Auto, SortBy::Row, 8, SEP, None);
        assert_eq!(layout.page_count(), 1);
        assert_eq!(layout.pages[0].len(), 4);
        assert!(layout.pages[0][0].starts_with("k,↑"));
        assert!(layout.pages[0][0].contains("focus up"));
    }

    #[test]
    fn fixed_two_columns_row_major() {
        let layout = lay_out(&sample(), 80, Columns::Fixed(2), SortBy::Row, 8, SEP, None);
        assert_eq!(layout.pages[0].len(), 2);
        // Row-major: row 0 holds entries 0 and 1.
        assert!(layout.pages[0][0].contains("focus up"));
        assert!(layout.pages[0][0].contains("focus down"));
        assert!(layout.pages[0][1].contains("focus left"));
    }

    #[test]
    fn column_major_fill_order() {
        // 2 cols, 2 rows/page: col 0 = entries 0,1 ; col 1 = entries 2,3.
        let layout = lay_out(
            &sample(),
            80,
            Columns::Fixed(2),
            SortBy::Column,
            2,
            SEP,
            None,
        );
        assert_eq!(layout.page_count(), 1);
        assert!(layout.pages[0][0].contains("focus up")); // (r0,c0)=0
        assert!(layout.pages[0][0].contains("focus left")); // (r0,c1)=2
        assert!(layout.pages[0][1].contains("focus down")); // (r1,c0)=1
        assert!(layout.pages[0][1].contains("focus right")); // (r1,c1)=3
    }

    #[test]
    fn paginates_when_over_capacity() {
        // 1 column, max 2 rows → 2 pages for 4 entries.
        let layout = lay_out(&sample(), 20, Columns::Fixed(1), SortBy::Row, 2, SEP, None);
        assert_eq!(layout.page_count(), 2);
        assert_eq!(layout.pages[0].len(), 2);
        assert_eq!(layout.pages[1].len(), 2);
    }

    #[test]
    fn adaptive_auto_packs_narrow_later_columns() {
        let entries = vec![
            entry(&["a"], "super-wide-label"),
            entry(&["b"], "x"),
            entry(&["c"], "x"),
            entry(&["d"], "x"),
            entry(&["e"], "x"),
            entry(&["f"], "x"),
            entry(&["g"], "x"),
        ];
        let layout = lay_out(&entries, 30, Columns::Auto, SortBy::Row, 6, SEP, None);
        assert_eq!(layout.page_count(), 1);
        assert_eq!(layout.pages[0].len(), 4);
        assert!(layout.pages[0][0].contains("super-wide-label"));
        assert!(layout.pages[0][0].contains("b - x"));
        assert!(layout.pages[0][3].contains("g - x"));
    }

    #[test]
    fn each_column_uses_its_own_width() {
        let entries = vec![entry(&["a"], "x"), entry(&["b"], "super-wide")];
        let layout = lay_out(&entries, 80, Columns::Fixed(2), SortBy::Row, 1, SEP, None);
        assert_eq!(layout.pages[0][0], "a - x  b - super-wide");
    }

    #[test]
    fn fixed_columns_are_an_upper_bound() {
        let entries = vec![
            entry(&["a"], "one"),
            entry(&["b"], "two"),
            entry(&["c"], "three"),
            entry(&["d"], "four"),
            entry(&["e"], "five"),
        ];
        let layout = lay_out(&entries, 80, Columns::Fixed(2), SortBy::Row, 2, SEP, None);
        assert_eq!(layout.page_count(), 2);
        assert_eq!(layout.pages[0].len(), 2);
        assert!(layout.pages[0][0].contains("a - one"));
        assert!(layout.pages[0][0].contains("b - two"));
        assert!(layout.pages[0][1].contains("c - three"));
        assert!(layout.pages[0][1].contains("d - four"));
        assert_eq!(layout.pages[1], vec!["e - five"]);
    }

    #[test]
    fn adaptive_column_major_keeps_column_fill_order() {
        let entries = vec![
            entry(&["a"], "one"),
            entry(&["b"], "two"),
            entry(&["c"], "three"),
            entry(&["d"], "four"),
            entry(&["e"], "five"),
        ];
        let layout = lay_out(&entries, 80, Columns::Auto, SortBy::Column, 2, SEP, None);
        assert_eq!(layout.page_count(), 1);
        assert_eq!(layout.pages[0].len(), 2);
        assert!(layout.pages[0][0].contains("a - one"));
        assert!(layout.pages[0][0].contains("c - three"));
        assert!(layout.pages[0][0].contains("e - five"));
        assert!(layout.pages[0][1].contains("b - two"));
        assert!(layout.pages[0][1].contains("d - four"));
    }

    #[test]
    fn key_column_snugs_per_page() {
        // Page 0 has a wide chord; page 1 has only a single-char key. With a
        // global key width, page 1 would be indented to match page 0 — instead
        // each page snugs, so page 1's key sits flush (right-align, pad 0).
        let entries = vec![
            entry(&["\u{F0634} \u{F0636} H"], "wide"), // 󰘴 󰘶 H (5 chars)
            entry(&["a"], "short"),
        ];
        let layout = lay_out(&entries, 40, Columns::Fixed(1), SortBy::Row, 1, SEP, None);
        assert_eq!(layout.page_count(), 2);
        assert!(layout.pages[0][0].starts_with("\u{F0634} \u{F0636} H"));
        assert!(layout.pages[1][0].starts_with("a")); // no leading pad
    }

    #[test]
    fn icon_column_reserved_when_any_entry_has_one() {
        // One entry has an icon, one doesn't: both align past a 1-wide icon
        // column, so the icon-less row gets blank padding where the icon would be.
        let entries = vec![entry_icon(&["a"], "act", "I"), entry(&["b"], "other")];
        let layout = lay_out(&entries, 40, Columns::Fixed(1), SortBy::Row, 2, SEP, None);
        assert_eq!(layout.pages[0][0], format!("a {SEP} I act"));
        // No icon → a space stands in for the icon glyph, keeping labels aligned.
        assert_eq!(layout.pages[0][1], format!("b {SEP}   other"));
    }

    #[test]
    fn no_icon_column_when_none_have_icons() {
        let layout = lay_out(&sample(), 40, Columns::Fixed(1), SortBy::Row, 8, SEP, None);
        // `<keys> <sep> <label>` with no icon column.
        assert_eq!(layout.pages[0][0], format!("k,↑ {SEP} focus up"));
    }

    #[test]
    fn icon_color_overrides_label_color_for_icon_only() {
        let theme = Theme {
            key: "\u{1b}[38;5;1m".into(),
            label: "\u{1b}[38;5;2m".into(),
            switch: "\u{1b}[38;5;4m".into(),
            dim: "\u{1b}[2m".into(),
            reset: "\u{1b}[0m".into(),
        };
        let icon_sgr = "\u{1b}[38;5;9m";
        let entries = vec![Entry {
            keys: vec!["a".into()],
            label: "act".into(),
            icon: Some("I".into()),
            icon_color: Some(icon_sgr.into()),
            switch: false,
        }];
        let out = lay_out(
            &entries,
            40,
            Columns::Fixed(1),
            SortBy::Row,
            1,
            SEP,
            Some(&theme),
        );
        let line = &out.pages[0][0];
        // Icon wears the override color; the label keeps the label color.
        assert!(line.contains(&format!("{icon_sgr}I\u{1b}[0m")));
        assert!(line.contains("\u{1b}[38;5;2mact\u{1b}[0m"));
    }

    #[test]
    fn empty_entries_no_pages() {
        let layout = lay_out(&[], 80, Columns::Auto, SortBy::Row, 8, SEP, None);
        assert_eq!(layout.page_count(), 0);
        assert_eq!(layout.content_width, 0);
    }

    #[test]
    fn themed_output_colors_segments_but_width_excludes_escapes() {
        let theme = Theme {
            key: "\u{1b}[38;5;1m".into(),
            label: "\u{1b}[38;5;2m".into(),
            switch: "\u{1b}[38;5;4m".into(),
            dim: "\u{1b}[2m".into(),
            reset: "\u{1b}[0m".into(),
        };
        let entries = vec![entry(&["a"], "act")];
        let plain = lay_out(&entries, 40, Columns::Fixed(1), SortBy::Row, 1, SEP, None);
        let themed = lay_out(
            &entries,
            40,
            Columns::Fixed(1),
            SortBy::Row,
            1,
            SEP,
            Some(&theme),
        );
        // Same visible width regardless of the (zero-width) color escapes.
        assert_eq!(plain.content_width, themed.content_width);
        // <keys> SP <sep> SP <label> = 1 + 1 + 1 + 1 + 3 (no icon column).
        assert_eq!(
            plain.content_width,
            "a".len() + 1 + SEP.len() + 1 + "act".len()
        );
        // The themed line carries the key + label colors and a reset.
        let line = &themed.pages[0][0];
        assert!(line.contains("\u{1b}[38;5;1ma\u{1b}[0m"));
        assert!(line.contains("\u{1b}[38;5;2mact\u{1b}[0m"));
    }
}
