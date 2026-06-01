//! Panel painting.
//!
//! Plugin panes don't honor bare `\n` (a line feed moves down a row but doesn't
//! return the carriage — producing a staircase), so we paint with **absolute
//! CSI positioning** (`ESC[row;colH`, 1-indexed), the same approach the
//! `zj-statusbar` floating panes use. Each render fully repaints the pane.
//!
//! We draw our **own** rounded frame (the pane is set borderless) so the title
//! is just the mode symbol — Zellij's native frame would also stamp its
//! `SCROLL`/`PIN` indicators, which we don't want. The frame and chrome are
//! colored from the palette [`Theme`]; the mode symbol takes the accent.

use crate::shared::geometry::Padding;
use crate::whichkey::footer::Footer;
use crate::whichkey::grid::Layout;
use crate::whichkey::theme::Theme;

/// Shown when the current mode has no (non-no-op) keybindings.
pub const NO_BINDINGS: &str = "(no keybindings for this mode)";

/// Heavy rule between the body and the footer.
const SEP_CHAR: &str = "\u{2501}"; // ━

// Rounded box-drawing frame.
const TL: &str = "\u{256d}"; // ╭
const TR: &str = "\u{256e}"; // ╮
const BL: &str = "\u{2570}"; // ╰
const BR: &str = "\u{256f}"; // ╯
const HBAR: &str = "\u{2500}"; // ─
const VBAR: &str = "\u{2502}"; // │

/// The frame's appearance: colors, the top-border title glyph + its tint, and
/// the interior padding. Bundled so [`paint`] keeps a small signature.
pub struct Frame<'a> {
    pub theme: &'a Theme,
    /// Top-border label: the mode breadcrumb, **already colored** (each mode
    /// glyph in its tint, joined by a dim `»`).
    pub title: &'a str,
    /// Visible width of `title` (its SGR codes have zero display width, so the
    /// frame can't derive this from the string length).
    pub title_w: usize,
    /// Interior padding (cells) between the frame and the content.
    pub pad: Padding,
}

/// Paint the whole pane: our rounded frame (the mode glyph shown as the
/// top-border label, tinted by its mode color), the body lines, and — when
/// present — a separator + footer pinned just above the bottom border.
/// `rows`/`cols` are the (borderless) pane size Zellij hands to `render`. The
/// footer carries its own colors; the page counter is pinned to the right edge.
pub fn paint(
    body: &[String],
    footer: Option<&Footer>,
    rows: usize,
    cols: usize,
    frame: &Frame,
) -> String {
    let Frame {
        theme,
        title,
        title_w,
        pad,
    } = *frame;
    let mut out = String::new();
    if rows == 0 || cols == 0 {
        return out;
    }
    let dim = &theme.dim;
    let reset = &theme.reset;

    let blank = " ".repeat(cols);
    for row in 1..=rows {
        out.push_str(&format!("\u{1b}[{row};1H{blank}"));
    }

    // Top border with the mode breadcrumb (pre-colored): ╭ <a » b> ─╮
    out.push_str(&format!("\u{1b}[1;1H{dim}{TL} {reset}{title}{dim} "));
    let used = 2 + title_w + 1; // "╭ " + breadcrumb + " "
    let dashes = cols.saturating_sub(used + 1); // +1 for the ╮
    out.push_str(&HBAR.repeat(dashes));
    out.push_str(&format!("{TR}{reset}"));

    // Bottom border: ╰─────╯
    if rows >= 2 {
        out.push_str(&format!(
            "\u{1b}[{rows};1H{dim}{BL}{}{BR}{reset}",
            HBAR.repeat(cols.saturating_sub(2))
        ));
    }

    // Side borders down the interior rows.
    for row in 2..rows {
        out.push_str(&format!("\u{1b}[{row};1H{dim}{VBAR}{reset}"));
        out.push_str(&format!("\u{1b}[{row};{cols}H{dim}{VBAR}{reset}"));
    }

    // Interior: the inner `pad` insets content from the frame. Content starts
    // past the left border + left pad; body starts past the top border + top
    // pad. The separator + footer sit just above the bottom pad + border.
    let content_col = 1 + pad.left + 1;
    let body_top = 1 + pad.top + 1;
    let (sep_row, footer_row, body_last) = match footer {
        Some(_) => {
            let f = rows.saturating_sub(1 + pad.bottom);
            let s = f.saturating_sub(1);
            (s, f, s.saturating_sub(1))
        }
        None => (0, 0, rows.saturating_sub(1 + pad.bottom)),
    };
    let body_cap = (body_last + 1).saturating_sub(body_top);
    for (i, line) in body.iter().take(body_cap).enumerate() {
        out.push_str(&format!("\u{1b}[{};{}H{}", body_top + i, content_col, line));
    }
    if let Some(footer) = footer {
        if sep_row >= 2 {
            // Span the content area (inset by the side borders + horizontal pad),
            // so the rule lines up with the body and footer rather than the frame.
            let sep_w = cols.saturating_sub(2 + pad.left + pad.right);
            out.push_str(&format!(
                "\u{1b}[{sep_row};{content_col}H{dim}{}{reset}",
                SEP_CHAR.repeat(sep_w)
            ));
        }
        // Left run flush against the left pad; both runs are pre-colored.
        out.push_str(&format!(
            "\u{1b}[{footer_row};{content_col}H{}",
            footer.left
        ));
        // Page counter pinned to the right edge: its last char sits one cell
        // (the right border) plus `pad.right` in from `cols`.
        if let Some(counter) = &footer.counter {
            let start = cols
                .saturating_sub(pad.right + footer.counter_w)
                .max(content_col);
            out.push_str(&format!("\u{1b}[{footer_row};{start}H{counter}"));
        }
    }

    out
}

/// Body lines for `page` (0-based, clamped), or the placeholder when empty.
pub fn page_lines(layout: &Layout, page: usize) -> Vec<String> {
    if layout.pages.is_empty() {
        return vec![NO_BINDINGS.to_string()];
    }
    let idx = page.min(layout.page_count().saturating_sub(1));
    layout.pages[idx].clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whichkey::config::SortBy;
    use crate::whichkey::grid::{lay_out, Columns};

    #[test]
    fn empty_layout_is_placeholder() {
        let layout = lay_out(&[], 40, Columns::Auto, SortBy::Row, 8, "-", None);
        assert_eq!(page_lines(&layout, 0), vec![NO_BINDINGS.to_string()]);
    }

    fn footer(left: &str, counter: Option<&str>) -> Footer {
        Footer {
            left: left.to_string(),
            left_w: left.chars().count(),
            counter: counter.map(|c| c.to_string()),
            counter_w: counter.map(|c| c.chars().count()).unwrap_or(0),
        }
    }

    /// Default interior padding (`0,1,0,1`): 1-cell horizontal inset, no
    /// vertical inset, so content starts at col 3 / row 2.
    fn pad() -> Padding {
        Padding {
            top: 0,
            right: 1,
            bottom: 0,
            left: 1,
        }
    }

    fn frame(theme: &Theme, pad: Padding) -> Frame<'_> {
        Frame {
            theme,
            title: "M",
            title_w: 1,
            pad,
        }
    }

    #[test]
    fn paint_draws_frame_and_body_inside_it() {
        let theme = Theme::default();
        // 4 rows: top border (1), body a (2), body b (3), bottom border (4).
        let body = vec!["a".to_string(), "b".to_string()];
        let out = paint(&body, None, 4, 12, &frame(&theme, pad()));
        assert!(out.contains(TL)); // top-left corner
        assert!(out.contains("M")); // mode symbol title
        assert!(out.contains("\u{1b}[2;3Ha")); // body starts on row 2, past border+pad
        assert!(out.contains("\u{1b}[3;3Hb"));
        assert!(out.contains(&format!("\u{1b}[4;1H{}{BL}", theme.dim))); // bottom border
                                                                         // No bare newlines.
        assert!(!out.contains('\n'));
    }

    #[test]
    fn paint_pins_separator_and_footer_above_bottom_border() {
        let theme = Theme::default();
        // 5 rows: top(1), body a(2), separator(3), footer(4), bottom border(5).
        let body = vec!["a".to_string()];
        let out = paint(
            &body,
            Some(&footer("x close", None)),
            5,
            12,
            &frame(&theme, pad()),
        );
        assert!(out.contains("\u{1b}[2;3Ha")); // body row 2
        assert!(out.contains("\u{1b}[3;3H")); // separator row 3 (inset by left pad)
        assert!(out.contains("\u{1b}[4;3H")); // footer row 4
        assert!(out.contains("x close"));
        assert!(out.contains(BR)); // bottom-right corner present
    }

    #[test]
    fn paint_right_aligns_page_counter() {
        let theme = Theme::default();
        let body = vec!["a".to_string()];
        // cols=12, pad.right=1: counter "2/3" (w=3) ends at col 10 (border col
        // 12, one pad cell at 11) → start col 12-(1+3)=8.
        let out = paint(
            &body,
            Some(&footer("x close", Some("2/3"))),
            5,
            12,
            &frame(&theme, pad()),
        );
        assert!(out.contains("\u{1b}[4;8H")); // counter pinned to the right pad
        assert!(out.contains("2/3"));
    }

    #[test]
    fn paint_honors_vertical_and_extra_horizontal_padding() {
        let theme = Theme::default();
        // pad top=1,left=2: body starts at row 3 (1 border + 1 pad + 1), col 4
        // (1 border + 2 pad + 1). 6 rows so there's room below.
        let body = vec!["a".to_string()];
        let p = Padding {
            top: 1,
            right: 1,
            bottom: 1,
            left: 2,
        };
        let out = paint(&body, None, 6, 14, &frame(&theme, p));
        assert!(out.contains("\u{1b}[3;4Ha"));
    }
}
