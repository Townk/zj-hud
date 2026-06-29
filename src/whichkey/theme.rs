//! Colors for the panel interior.
//!
//! The plugin draws its own frame and interior, so it owns every color it
//! paints. A few roles are fixed by design (per the user's spec) and a few are
//! palette-driven so the chrome tracks the active theme:
//!
//!   * **keys** — body chord glyphs, always bright white so bindings pop;
//!   * **switch labels** — labels for bindings that enter another mode, blue;
//!   * **labels** — every other binding's label, a soft pink;
//!   * **border** — the panel frame, Catppuccin Blue;
//!   * **footer** — the separator rule + footer key labels, grey;
//!   * **dim** — secondary body chrome rendered faint (palette-driven, from
//!     `ModeInfo::style`).
//!
//! Mode *symbol* colors are configured per-mode (see `config`/`modes`) and
//! passed into the renderer separately, not stored here.

use zellij_tile::prelude::{PaletteColor, Style};

/// SGR reset (all attributes off).
pub const RESET: &str = "\u{1b}[0m";
/// SGR faint/dim attribute.
const FAINT: &str = "\u{1b}[2m";

/// Chord keys: bright white.
const KEY_WHITE: &str = "\u{1b}[38;2;255;255;255m";
/// Mode-switch labels: blue (Catppuccin Blue `#89B4FA`).
const SWITCH_BLUE: &str = "\u{1b}[38;2;137;180;250m";
/// Regular labels: soft pink (Catppuccin Pink `#F5C2E7`).
const LABEL_PINK: &str = "\u{1b}[38;2;245;194;231m";
/// Panel border: Catppuccin Blue `#89B4FA`.
const BORDER_BLUE: &str = SWITCH_BLUE;
/// Footer chrome: grey (`#6C7086`).
const FOOTER_GREY: &str = "\u{1b}[38;2;108;112;134m";

/// Optional chrome-color overrides parsed from the which-key config. Each is a
/// ready-to-emit SGR foreground sequence (as produced by [`parse_color`]); a
/// `None` keeps the fixed default for that role. `dim` is intentionally absent:
/// it always tracks the live palette (see [`Theme::from_style`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChromeColors {
    pub key: Option<String>,
    pub label: Option<String>,
    pub switch: Option<String>,
    pub border: Option<String>,
    pub footer: Option<String>,
}

/// SGR foreground sequences for each interior region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Body chord keys (bright white).
    pub key: String,
    /// Labels for ordinary bindings (pink).
    pub label: String,
    /// Labels for bindings that switch mode (blue).
    pub switch: String,
    /// Panel frame (blue).
    pub border: String,
    /// Footer separator + key labels (grey).
    pub footer: String,
    /// Secondary body chrome (faint foreground).
    pub dim: String,
    /// Attribute reset to close any colored span.
    pub reset: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            key: KEY_WHITE.to_string(),
            label: LABEL_PINK.to_string(),
            switch: SWITCH_BLUE.to_string(),
            border: BORDER_BLUE.to_string(),
            footer: FOOTER_GREY.to_string(),
            dim: FAINT.to_string(),
            reset: RESET.to_string(),
        }
    }
}

impl Theme {
    /// Derive the interior colors from the live [`Style`]. Only secondary dim
    /// chrome tracks the palette (the body foreground rendered faint); keys,
    /// labels, switch labels, border, and footer are fixed roles.
    pub fn from_style(style: &Style) -> Self {
        let text = style.colors.text_unselected;
        Self {
            dim: format!("{FAINT}{}", sgr_fg(text.base)),
            ..Self::default()
        }
    }

    /// Like [`Self::from_style`], but overlays any configured chrome overrides
    /// on top of the fixed defaults. Each provided override replaces its
    /// default; `dim` still tracks the live palette.
    pub fn from_style_and_colors(style: &Style, colors: &ChromeColors) -> Self {
        let mut theme = Self::from_style(style);
        theme.apply_chrome(colors);
        theme
    }

    fn apply_chrome(&mut self, colors: &ChromeColors) {
        if let Some(key) = &colors.key {
            self.key = key.clone();
        }
        if let Some(label) = &colors.label {
            self.label = label.clone();
        }
        if let Some(switch) = &colors.switch {
            self.switch = switch.clone();
        }
        if let Some(border) = &colors.border {
            self.border = border.clone();
        }
        if let Some(footer) = &colors.footer {
            self.footer = footer.clone();
        }
    }
}

/// A `PaletteColor` as an SGR set-foreground sequence.
fn sgr_fg(color: PaletteColor) -> String {
    match color {
        PaletteColor::Rgb((r, g, b)) => format!("\u{1b}[38;2;{r};{g};{b}m"),
        PaletteColor::EightBit(n) => format!("\u{1b}[38;5;{n}m"),
    }
}

/// Parse a user color into an SGR set-foreground sequence.
///
/// Accepts `#RGB` / `#RRGGBB` hex and a bare 0–255 integer (256-color index).
/// Returns `None` for anything it can't make sense of. The hex case is shared
/// with the bar via [`crate::shared::color::Color`] so both surfaces parse `#RGB`/
/// `#RRGGBB` identically; the truecolor `to_ansi_fg` escape (`\e[38;2;r;g;bm`)
/// is exactly the foreground form this panel paints with.
pub fn parse_color(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.starts_with('#') {
        return crate::shared::color::Color::parse_hex(spec).map(|c| c.to_ansi_fg());
    }
    if let Ok(n) = spec.parse::<u8>() {
        return Some(format!("\u{1b}[38;5;{n}m"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_uses_fixed_roles() {
        let t = Theme::default();
        assert_eq!(t.key, KEY_WHITE);
        assert_eq!(t.label, LABEL_PINK);
        assert_eq!(t.switch, SWITCH_BLUE);
        assert_eq!(t.border, BORDER_BLUE);
        assert_eq!(t.footer, FOOTER_GREY);
        assert_eq!(t.dim, FAINT);
    }

    #[test]
    fn from_style_only_overrides_dim() {
        let mut style = Style::default();
        style.colors.text_unselected.base = PaletteColor::EightBit(7);
        let t = Theme::from_style(&style);
        assert_eq!(t.key, KEY_WHITE);
        assert_eq!(t.label, LABEL_PINK);
        assert_eq!(t.switch, SWITCH_BLUE);
        assert_eq!(t.border, BORDER_BLUE);
        assert_eq!(t.footer, FOOTER_GREY);
        assert_eq!(t.dim, "\u{1b}[2m\u{1b}[38;5;7m");
    }

    #[test]
    fn from_style_and_colors_overrides_replace_defaults() {
        let mut style = Style::default();
        style.colors.text_unselected.base = PaletteColor::EightBit(7);
        let colors = ChromeColors {
            key: parse_color("#ffffff"),
            label: parse_color("#F5C2E7"),
            switch: parse_color("#89B4FA"),
            border: parse_color("#a6e3a1"),
            footer: parse_color("5"),
        };
        let t = Theme::from_style_and_colors(&style, &colors);
        assert_eq!(t.key, "\u{1b}[38;2;255;255;255m");
        assert_eq!(t.label, "\u{1b}[38;2;245;194;231m");
        assert_eq!(t.switch, "\u{1b}[38;2;137;180;250m");
        assert_eq!(t.border, "\u{1b}[38;2;166;227;161m");
        assert_eq!(t.footer, "\u{1b}[38;5;5m");
        // dim still tracks the live palette, never the overrides.
        assert_eq!(t.dim, "\u{1b}[2m\u{1b}[38;5;7m");
    }

    #[test]
    fn from_style_and_colors_absent_overrides_keep_defaults() {
        let mut style = Style::default();
        style.colors.text_unselected.base = PaletteColor::EightBit(7);
        // Only override the border; the rest keep their fixed defaults.
        let colors = ChromeColors {
            border: parse_color("#a6e3a1"),
            ..ChromeColors::default()
        };
        let t = Theme::from_style_and_colors(&style, &colors);
        assert_eq!(t.key, KEY_WHITE);
        assert_eq!(t.label, LABEL_PINK);
        assert_eq!(t.switch, SWITCH_BLUE);
        assert_eq!(t.border, "\u{1b}[38;2;166;227;161m");
        assert_eq!(t.footer, FOOTER_GREY);
        assert_eq!(t.dim, "\u{1b}[2m\u{1b}[38;5;7m");
    }

    #[test]
    fn from_style_and_colors_default_matches_from_style() {
        let style = Style::default();
        assert_eq!(
            Theme::from_style_and_colors(&style, &ChromeColors::default()),
            Theme::from_style(&style)
        );
    }

    #[test]
    fn parse_color_hex_and_index() {
        assert_eq!(
            parse_color("#89B4FA"),
            Some("\u{1b}[38;2;137;180;250m".to_string())
        );
        assert_eq!(
            parse_color("#abc"),
            Some("\u{1b}[38;2;170;187;204m".to_string())
        );
        assert_eq!(parse_color("5"), Some("\u{1b}[38;5;5m".to_string()));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12"), None);
    }
}
