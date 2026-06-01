//! Color module for zj-hud.
//!
//! Provides hex parsing, ANSI escape codes, HSL darken/lighten,
//! WCAG contrast ratio, and Oklab-based gradient generation.

// ─── Color struct ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Construct a `Color` from explicit RGB byte values.
    pub fn new(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }

    /// Parse `#RGB` or `#RRGGBB` hex strings. Returns `None` on any error.
    pub fn parse_hex(s: &str) -> Option<Color> {
        let s = s.strip_prefix('#')?;
        match s.len() {
            3 => {
                let r = u8::from_str_radix(&s[0..1], 16).ok()?;
                let g = u8::from_str_radix(&s[1..2], 16).ok()?;
                let b = u8::from_str_radix(&s[2..3], 16).ok()?;
                // Expand nibble: 0xA -> 0xAA
                Some(Color {
                    r: r << 4 | r,
                    g: g << 4 | g,
                    b: b << 4 | b,
                })
            }
            6 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Color { r, g, b })
            }
            _ => None,
        }
    }

    // ─── ANSI helpers ─────────────────────────────────────────────────────────

    /// Returns the ANSI 24-bit foreground escape sequence.
    pub fn to_ansi_fg(self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.r, self.g, self.b)
    }

    /// Returns the ANSI 24-bit background escape sequence.
    pub fn to_ansi_bg(self) -> String {
        format!("\x1b[48;2;{};{};{}m", self.r, self.g, self.b)
    }

    // ─── HSL helpers ─────────────────────────────────────────────────────────

    /// Convert to HSL. Returns `(h, s, l)` with h in [0, 360), s and l in [0, 1].
    fn to_hsl(self) -> (f32, f32, f32) {
        let r = self.r as f32 / 255.0;
        let g = self.g as f32 / 255.0;
        let b = self.b as f32 / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let l = (max + min) / 2.0;

        if delta == 0.0 {
            return (0.0, 0.0, l);
        }

        let s = delta / (1.0 - (2.0 * l - 1.0).abs());

        let h = if max == r {
            60.0 * (((g - b) / delta) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };

        let h = if h < 0.0 { h + 360.0 } else { h };

        (h, s, l)
    }

    /// Convert from HSL back to `Color`. h in [0, 360), s and l in [0, 1].
    fn from_hsl(h: f32, s: f32, l: f32) -> Color {
        if s == 0.0 {
            let v = (l * 255.0).round() as u8;
            return Color { r: v, g: v, b: v };
        }

        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = l - c / 2.0;

        let (r1, g1, b1) = match (h / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };

        Color {
            r: ((r1 + m) * 255.0).round() as u8,
            g: ((g1 + m) * 255.0).round() as u8,
            b: ((b1 + m) * 255.0).round() as u8,
        }
    }

    /// Darken the color by subtracting `factor` from HSL lightness (clamped to 0.0).
    pub fn darken(&self, factor: f32) -> Color {
        let (h, s, l) = self.to_hsl();
        let l = (l - factor).max(0.0);
        Color::from_hsl(h, s, l)
    }

    /// Lighten the color by adding `factor` to HSL lightness (clamped to 1.0).
    pub fn lighten(&self, factor: f32) -> Color {
        let (h, s, l) = self.to_hsl();
        let l = (l + factor).min(1.0);
        Color::from_hsl(h, s, l)
    }
}

// ─── WCAG contrast ratio ─────────────────────────────────────────────────────

/// Linearize a single sRGB channel value (in [0, 1]).
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Compute relative luminance for a color (WCAG definition).
fn relative_luminance(c: Color) -> f32 {
    let r = srgb_to_linear(c.r as f32 / 255.0);
    let g = srgb_to_linear(c.g as f32 / 255.0);
    let b = srgb_to_linear(c.b as f32 / 255.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between two colors. Black/white ≈ 21.0.
pub fn contrast_ratio(c1: Color, c2: Color) -> f32 {
    let l1 = relative_luminance(c1);
    let l2 = relative_luminance(c2);
    let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

// ─── Oklab gradient ───────────────────────────────────────────────────────────

/// Apply sRGB gamma encoding to a linear value.
fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert linear sRGB to Oklab. All inputs in [0, 1].
#[allow(clippy::excessive_precision)]
fn linear_rgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    // RGB → LMS
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;

    // Cube root
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    // LMS → Oklab
    let ok_l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let ok_a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let ok_b = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;

    (ok_l, ok_a, ok_b)
}

/// Convert Oklab back to linear sRGB.
#[allow(clippy::excessive_precision)]
fn oklab_to_linear_rgb(ok_l: f32, ok_a: f32, ok_b: f32) -> (f32, f32, f32) {
    // Oklab → LMS (cubed)
    let l_ = ok_l + 0.3963377774 * ok_a + 0.2158037573 * ok_b;
    let m_ = ok_l - 0.1055613458 * ok_a - 0.0638541728 * ok_b;
    let s_ = ok_l - 0.0894841775 * ok_a - 1.2914855480 * ok_b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    // LMS → linear RGB
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;

    (r, g, b)
}

/// Convert `Color` to Oklab.
fn color_to_oklab(c: Color) -> (f32, f32, f32) {
    let r = srgb_to_linear(c.r as f32 / 255.0);
    let g = srgb_to_linear(c.g as f32 / 255.0);
    let b = srgb_to_linear(c.b as f32 / 255.0);
    linear_rgb_to_oklab(r, g, b)
}

/// Convert Oklab back to `Color`.
fn oklab_to_color(ok_l: f32, ok_a: f32, ok_b: f32) -> Color {
    let (r, g, b) = oklab_to_linear_rgb(ok_l, ok_a, ok_b);
    let r = linear_to_srgb(r.clamp(0.0, 1.0));
    let g = linear_to_srgb(g.clamp(0.0, 1.0));
    let b = linear_to_srgb(b.clamp(0.0, 1.0));
    Color {
        r: (r * 255.0).round() as u8,
        g: (g * 255.0).round() as u8,
        b: (b * 255.0).round() as u8,
    }
}

/// Generate a perceptually-uniform gradient in Oklab space.
///
/// - `steps == 0` returns an empty vec.
/// - `steps == 1` returns `[from]`.
/// - `steps == 2` returns `[from, to]`.
/// - Otherwise returns `steps` evenly spaced stops from `from` to `to`.
pub fn gradient(from: Color, to: Color, steps: usize) -> Vec<Color> {
    match steps {
        0 => vec![],
        1 => vec![from],
        2 => vec![from, to],
        _ => {
            let (l0, a0, b0) = color_to_oklab(from);
            let (l1, a1, b1) = color_to_oklab(to);
            (0..steps)
                .map(|i| {
                    let t = i as f32 / (steps - 1) as f32;
                    let l = l0 + (l1 - l0) * t;
                    let a = a0 + (a1 - a0) * t;
                    let b = b0 + (b1 - b0) * t;
                    oklab_to_color(l, a, b)
                })
                .collect()
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_hex ────────────────────────────────────────────────────────────

    #[test]
    fn parse_hex_6digit() {
        let c = Color::parse_hex("#1a2b3c").unwrap();
        assert_eq!(
            c,
            Color {
                r: 0x1a,
                g: 0x2b,
                b: 0x3c
            }
        );
    }

    #[test]
    fn parse_hex_6digit_uppercase() {
        let c = Color::parse_hex("#AABBCC").unwrap();
        assert_eq!(
            c,
            Color {
                r: 0xAA,
                g: 0xBB,
                b: 0xCC
            }
        );
    }

    #[test]
    fn parse_hex_3digit() {
        // #F0A should expand to #FF00AA
        let c = Color::parse_hex("#F0A").unwrap();
        assert_eq!(
            c,
            Color {
                r: 0xFF,
                g: 0x00,
                b: 0xAA
            }
        );
    }

    #[test]
    fn parse_hex_3digit_lowercase() {
        let c = Color::parse_hex("#abc").unwrap();
        assert_eq!(
            c,
            Color {
                r: 0xAA,
                g: 0xBB,
                b: 0xCC
            }
        );
    }

    #[test]
    fn parse_hex_no_hash_returns_none() {
        assert!(Color::parse_hex("aabbcc").is_none());
        assert!(Color::parse_hex("1a2b3c").is_none());
    }

    #[test]
    fn parse_hex_invalid_chars_returns_none() {
        assert!(Color::parse_hex("#GGHHII").is_none());
        assert!(Color::parse_hex("#zz1234").is_none());
    }

    #[test]
    fn parse_hex_wrong_length_returns_none() {
        assert!(Color::parse_hex("#12").is_none());
        assert!(Color::parse_hex("#1234").is_none());
        assert!(Color::parse_hex("#12345").is_none());
        assert!(Color::parse_hex("#1234567").is_none());
        assert!(Color::parse_hex("#").is_none());
        assert!(Color::parse_hex("").is_none());
    }

    // ── ANSI ─────────────────────────────────────────────────────────────────

    #[test]
    fn ansi_fg_format() {
        let c = Color {
            r: 255,
            g: 128,
            b: 0,
        };
        assert_eq!(c.to_ansi_fg(), "\x1b[38;2;255;128;0m");
    }

    #[test]
    fn ansi_bg_format() {
        let c = Color {
            r: 0,
            g: 64,
            b: 255,
        };
        assert_eq!(c.to_ansi_bg(), "\x1b[48;2;0;64;255m");
    }

    // ── darken / lighten ─────────────────────────────────────────────────────

    #[test]
    fn darken_moderate() {
        // A mid-gray darkened should produce a darker gray
        let c = Color {
            r: 128,
            g: 128,
            b: 128,
        };
        let darkened = c.darken(0.2);
        // Lightness ~0.502 minus 0.2 → ~0.302; all channels should be roughly equal and lower
        assert!(darkened.r < c.r);
        assert_eq!(darkened.r, darkened.g);
        assert_eq!(darkened.g, darkened.b);
    }

    #[test]
    fn darken_clamps_to_black() {
        let c = Color {
            r: 30,
            g: 30,
            b: 30,
        };
        let darkened = c.darken(1.0);
        assert_eq!(darkened, Color { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn lighten_moderate() {
        let c = Color {
            r: 100,
            g: 100,
            b: 100,
        };
        let lightened = c.lighten(0.2);
        assert!(lightened.r > c.r);
        assert_eq!(lightened.r, lightened.g);
        assert_eq!(lightened.g, lightened.b);
    }

    #[test]
    fn lighten_clamps_to_white() {
        let c = Color {
            r: 230,
            g: 230,
            b: 230,
        };
        let lightened = c.lighten(1.0);
        assert_eq!(
            lightened,
            Color {
                r: 255,
                g: 255,
                b: 255
            }
        );
    }

    // ── contrast_ratio ───────────────────────────────────────────────────────

    #[test]
    fn contrast_black_white() {
        let black = Color { r: 0, g: 0, b: 0 };
        let white = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        let ratio = contrast_ratio(black, white);
        // WCAG specifies exactly 21:1 for black on white
        assert!((ratio - 21.0).abs() < 0.1, "Expected ~21.0, got {ratio}");
    }

    #[test]
    fn contrast_same_color() {
        let c = Color {
            r: 100,
            g: 150,
            b: 200,
        };
        let ratio = contrast_ratio(c, c);
        assert!((ratio - 1.0).abs() < 0.001, "Expected 1.0, got {ratio}");
    }

    #[test]
    fn contrast_is_symmetric() {
        let a = Color { r: 255, g: 0, b: 0 };
        let b = Color { r: 0, g: 0, b: 255 };
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 0.001);
    }

    // ── gradient ─────────────────────────────────────────────────────────────

    #[test]
    fn gradient_0_steps() {
        let from = Color { r: 0, g: 0, b: 0 };
        let to = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        assert!(gradient(from, to, 0).is_empty());
    }

    #[test]
    fn gradient_1_step() {
        let from = Color {
            r: 10,
            g: 20,
            b: 30,
        };
        let to = Color {
            r: 200,
            g: 210,
            b: 220,
        };
        let g = gradient(from, to, 1);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0], from);
    }

    #[test]
    fn gradient_2_steps() {
        let from = Color { r: 0, g: 0, b: 0 };
        let to = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        let g = gradient(from, to, 2);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], from);
        assert_eq!(g[1], to);
    }

    #[test]
    fn gradient_5_steps_endpoints() {
        let from = Color { r: 0, g: 0, b: 0 };
        let to = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        let g = gradient(from, to, 5);
        assert_eq!(g.len(), 5);
        assert_eq!(g[0], from);
        assert_eq!(g[4], to);
    }

    #[test]
    fn gradient_5_steps_middle_between_endpoints() {
        let from = Color { r: 0, g: 0, b: 0 };
        let to = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        let g = gradient(from, to, 5);
        // Middle values should be strictly between endpoints
        for mid in &g[1..4] {
            assert!(mid.r > 0 && mid.r < 255, "r={} out of (0,255)", mid.r);
            assert!(mid.g > 0 && mid.g < 255, "g={} out of (0,255)", mid.g);
            assert!(mid.b > 0 && mid.b < 255, "b={} out of (0,255)", mid.b);
        }
    }

    #[test]
    fn gradient_5_steps_monotone() {
        // Gradient from dark to light should be monotonically increasing in all channels
        let from = Color { r: 0, g: 0, b: 0 };
        let to = Color {
            r: 255,
            g: 255,
            b: 255,
        };
        let g = gradient(from, to, 5);
        for i in 1..g.len() {
            assert!(g[i].r >= g[i - 1].r, "r not monotone at step {i}");
            assert!(g[i].g >= g[i - 1].g, "g not monotone at step {i}");
            assert!(g[i].b >= g[i - 1].b, "b not monotone at step {i}");
        }
    }

    #[test]
    fn gradient_colored() {
        // Gradient from red to blue
        let from = Color { r: 255, g: 0, b: 0 };
        let to = Color { r: 0, g: 0, b: 255 };
        let g = gradient(from, to, 5);
        assert_eq!(g.len(), 5);
        assert_eq!(g[0], from);
        assert_eq!(g[4], to);
    }
}
