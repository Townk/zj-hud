//! Anchor/padding geometry: turn a desired content size + placement policy
//! into a concrete floating-pane rectangle (fixed cells) within a screen.
//!
//! Pure and host-testable — no shim calls. The binary feeds it the screen size
//! (from `TabInfo::display_area_*`) and the natural content size, and applies
//! the resulting [`Rect`] via `FloatingPaneCoordinates`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Where the panel sits on screen. Any axis the user omits defaults to center.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub v: VAlign,
    pub h: HAlign,
}

impl Default for Anchor {
    fn default() -> Self {
        Self {
            v: VAlign::Bottom,
            h: HAlign::Right,
        }
    }
}

/// Edge insets in cells (CSS order: top, right, bottom, left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    pub left: usize,
}

/// How the panel's width is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidthMode {
    /// Render one binding column and shrink to the natural content width.
    #[default]
    Single,
    /// Span the full padded width.
    Fill,
    /// Use a percentage of the full screen width, then clamp to the padded area.
    Percent(u16),
    /// A fixed number of cells.
    Fixed(usize),
}

/// A concrete floating-pane rectangle, in fixed cells from the screen origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Body rows to pack per page so a page never overflows the screen and gets
/// clipped. A clipped row is invisible yet still widens the snug key column, so
/// the grid must not pack more rows than will display. Reserves the frame
/// (`frame` rows — the Zellij pane chrome, self-calibrated by the binary), the
/// vertical padding, and — when the content will paginate — the separator +
/// footer rows. `pad_v` is `padding.top + padding.bottom`.
pub fn body_row_budget(
    screen_h: usize,
    pad_v: usize,
    frame: usize,
    max_height: usize,
    entry_count: usize,
) -> usize {
    const FOOTER: usize = 2;
    if screen_h == 0 {
        return max_height.max(1);
    }
    let interior = screen_h.saturating_sub(frame + pad_v).max(1);
    let single = max_height.min(interior);
    if entry_count <= single {
        // Everything fits on one (footer-less) page.
        single.max(1)
    } else {
        // Paginating: leave room for the separator + footer.
        max_height.min(interior.saturating_sub(FOOTER)).max(1)
    }
}

/// Place a panel of natural size `content` (`(width, height)`, chrome already
/// included) on a `screen` of `(cols, rows)`, honoring `width`, `anchor`, and
/// `pad`. The result is always within the screen.
pub fn place(
    screen: (usize, usize),
    content: (usize, usize),
    width: WidthMode,
    anchor: Anchor,
    pad: Padding,
) -> Rect {
    let (screen_w, screen_h) = screen;
    let avail_w = screen_w.saturating_sub(pad.left + pad.right);
    let avail_h = screen_h.saturating_sub(pad.top + pad.bottom);

    let w = match width {
        WidthMode::Fill => avail_w,
        WidthMode::Percent(percent) => {
            (screen_w.saturating_mul(percent as usize) / 100).min(avail_w)
        }
        WidthMode::Fixed(n) => n.min(avail_w),
        WidthMode::Single => content.0.min(avail_w),
    }
    .max(1);
    let h = content.1.min(avail_h).max(1);

    let x = match anchor.h {
        HAlign::Left => pad.left,
        HAlign::Right => screen_w.saturating_sub(w + pad.right),
        HAlign::Center => pad.left + avail_w.saturating_sub(w) / 2,
    };
    let y = match anchor.v {
        VAlign::Top => pad.top,
        VAlign::Bottom => screen_h.saturating_sub(h + pad.bottom),
        VAlign::Center => pad.top + avail_h.saturating_sub(h) / 2,
    };

    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_PAD: Padding = Padding {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    #[test]
    fn bottom_right_auto() {
        let r = place(
            (100, 30),
            (20, 10),
            WidthMode::Single,
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Right,
            },
            NO_PAD,
        );
        assert_eq!(
            r,
            Rect {
                x: 80,
                y: 20,
                width: 20,
                height: 10
            }
        );
    }

    #[test]
    fn bottom_right_with_padding() {
        let r = place(
            (100, 30),
            (20, 10),
            WidthMode::Single,
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Right,
            },
            Padding {
                top: 0,
                right: 2,
                bottom: 1,
                left: 0,
            },
        );
        // x = 100 - (20 + 2) = 78 ; y = 30 - (10 + 1) = 19
        assert_eq!(r.x, 78);
        assert_eq!(r.y, 19);
        assert_eq!(r.width, 20);
    }

    #[test]
    fn bottom_center_fill() {
        let r = place(
            (100, 30),
            (20, 4),
            WidthMode::Fill,
            Anchor {
                v: VAlign::Bottom,
                h: HAlign::Center,
            },
            Padding {
                top: 0,
                right: 2,
                bottom: 1,
                left: 2,
            },
        );
        // fill width = 100 - 4 = 96 ; x = pad.left = 2 ; y = 30 - (4 + 1) = 25
        assert_eq!(r.width, 96);
        assert_eq!(r.x, 2);
        assert_eq!(r.y, 25);
    }

    #[test]
    fn center_centers_on_both_axes() {
        let r = place(
            (100, 30),
            (20, 10),
            WidthMode::Single,
            Anchor {
                v: VAlign::Center,
                h: HAlign::Center,
            },
            NO_PAD,
        );
        assert_eq!(r.x, 40); // (100-20)/2
        assert_eq!(r.y, 10); // (30-10)/2
    }

    #[test]
    fn body_budget_uses_full_height_when_tall() {
        // Tall screen: the config max_height is the limit, footer reserved.
        assert_eq!(body_row_budget(30, 0, 2, 8, 50), 8);
        // Fits on one page: no footer reservation needed.
        assert_eq!(body_row_budget(30, 0, 2, 8, 5), 8);
    }

    #[test]
    fn body_budget_shrinks_to_avoid_clipping() {
        // Interior = 11 - 2(frame) = 9; paginating reserves 2 → 7 rows fit.
        assert_eq!(body_row_budget(11, 0, 2, 8, 50), 7);
        // Vertical padding eats into it too: interior = 11 - 2 - 2 = 7 → 5.
        assert_eq!(body_row_budget(11, 2, 2, 8, 50), 5);
        // A larger (calibrated) frame overhead shrinks it further: interior =
        // 12 - 3 = 9, paginating reserves 2 → 7.
        assert_eq!(body_row_budget(12, 0, 3, 8, 50), 7);
    }

    #[test]
    fn body_budget_falls_back_when_screen_unknown() {
        assert_eq!(body_row_budget(0, 0, 2, 8, 50), 8);
    }

    #[test]
    fn content_wider_than_screen_is_clamped() {
        let r = place(
            (10, 5),
            (50, 50),
            WidthMode::Single,
            Anchor::default(),
            NO_PAD,
        );
        assert!(r.x + r.width <= 10);
        assert!(r.y + r.height <= 5);
    }

    #[test]
    fn fixed_width_capped_to_available() {
        let r = place(
            (40, 20),
            (10, 5),
            WidthMode::Fixed(1000),
            Anchor {
                v: VAlign::Top,
                h: HAlign::Left,
            },
            Padding {
                top: 1,
                right: 3,
                bottom: 0,
                left: 2,
            },
        );
        assert_eq!(r.width, 40 - 2 - 3);
        assert_eq!(r.x, 2);
        assert_eq!(r.y, 1);
    }

    #[test]
    fn percent_width_uses_screen_width_and_clamps_to_margin() {
        let r = place(
            (100, 20),
            (10, 5),
            WidthMode::Percent(50),
            Anchor {
                v: VAlign::Top,
                h: HAlign::Left,
            },
            Padding {
                top: 0,
                right: 30,
                bottom: 0,
                left: 30,
            },
        );
        assert_eq!(r.width, 40);
        assert_eq!(r.x, 30);
    }
}
