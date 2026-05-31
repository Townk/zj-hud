# zj-statusbar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Zellij WASI plugin that renders a status bar with tabs on the left and mode/session/battery/wifi/time segments on the right.

**Architecture:** Event-driven state machine. Each plugin instance holds an `AppState` updated on Zellij events. `render()` reads pre-computed state and emits raw ANSI. Pure-logic modules (color, truncation, layout) are unit-tested; integration testing requires the Zellij runtime.

**Tech Stack:** Rust, `zellij-tile` 0.44, `unicode-width` 0.2, `chrono` 0.4 (clock feature only), compiled to `wasm32-wasip1`.

---

## File Map

| File | Responsibility |
|---|---|
| `Cargo.toml` | Dependencies, wasm target config |
| `src/main.rs` | Plugin entry point — `load`, `update`, `render`, event dispatch |
| `src/color.rs` | Color struct, hex parsing, HSL darken/lighten, Oklab gradient, WCAG contrast, ANSI helpers |
| `src/icons.rs` | NerdFont codepoint constants, process icon map, shell detection, `IconLibrary` |
| `src/config.rs` | `Config` struct, parse from `BTreeMap<String, String>`, defaults |
| `src/state.rs` | `AppState`, `CachedValue<T>`, `BatteryInfo`, `BatteryState`, `ScrollState` |
| `src/truncation.rs` | `truncated_text()` algorithm from spec §5.3.1 |
| `src/tabs.rs` | Tab title composition — priority chain, process/dir/home icons, zoom suffix |
| `src/layout.rs` | Tab width equalization (§5.4.4), scrolling/visible-window (§5.4.5) |
| `src/segments.rs` | Right-side segment functions — mode, session, battery, wifi, time, `format_segment` |
| `src/system.rs` | Battery/WiFi async queries via `run_command()`, result parsing |
| `src/render.rs` | Final bar assembly — left tabs + gap + right segments, ANSI output, caching |

Dependencies flow downward (no cycles):
```
main.rs → config.rs, state.rs, render.rs, system.rs
render.rs → tabs.rs, layout.rs, segments.rs, color.rs, state.rs, config.rs
segments.rs → color.rs, icons.rs, state.rs, config.rs
tabs.rs → icons.rs, truncation.rs, state.rs, config.rs
layout.rs → tabs.rs, state.rs, config.rs
system.rs → state.rs
config.rs → color.rs, icons.rs
```

---

## Task 1: Project Scaffolding

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "zj-statusbar"
version = "0.1.0"
edition = "2021"
description = "A Zellij status bar plugin"
license = "MIT"

[dependencies]
zellij-tile = "0.44"
unicode-width = "0.2"
chrono = { version = "0.4", default-features = false, features = ["clock"] }

[profile.release]
opt-level = "s"
lto = true
strip = true
codegen-units = 1
```

- [ ] **Step 2: Create minimal main.rs**

```rust
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

register_plugin!(State);

#[derive(Default)]
struct State;

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        set_selectable(false);
    }

    fn update(&mut self, _event: Event) -> bool {
        false
    }

    fn render(&mut self, _rows: usize, _cols: usize) {
        print!("zj-statusbar");
    }
}
```

- [ ] **Step 3: Verify it compiles to WASM**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles successfully. If the `wasm32-wasip1` target is not installed, run `rustup target add wasm32-wasip1` first.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: scaffold project with minimal Zellij plugin"
```

---

## Task 2: Color Module

**Files:**
- Create: `src/color.rs`
- Modify: `src/main.rs` (add `mod color;`)

This is the largest pure-logic module. It has no dependencies on other project modules.

- [ ] **Step 1: Write tests for hex parsing**

Add to `src/color.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn parse_hex(s: &str) -> Option<Color> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_6digit() {
        assert_eq!(Color::parse_hex("#ff6666"), Some(Color::new(255, 102, 102)));
    }

    #[test]
    fn parse_hex_3digit() {
        assert_eq!(Color::parse_hex("#f66"), Some(Color::new(255, 102, 102)));
    }

    #[test]
    fn parse_hex_invalid() {
        assert_eq!(Color::parse_hex("not-a-color"), None);
        assert_eq!(Color::parse_hex("#gg0000"), None);
        assert_eq!(Color::parse_hex(""), None);
    }

    #[test]
    fn parse_hex_no_hash() {
        assert_eq!(Color::parse_hex("ff6666"), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib color`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement parse_hex**

Replace `todo!()` in `parse_hex`:

```rust
pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    match s.len() {
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()?;
            let g = u8::from_str_radix(&s[1..2], 16).ok()?;
            let b = u8::from_str_radix(&s[2..3], 16).ok()?;
            Some(Color::new(r * 17, g * 17, b * 17))
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Color::new(r, g, b))
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib color`
Expected: All 4 tests PASS.

- [ ] **Step 5: Write tests for ANSI helpers**

```rust
#[test]
fn ansi_fg() {
    let c = Color::new(255, 102, 102);
    assert_eq!(c.to_ansi_fg(), "\x1b[38;2;255;102;102m");
}

#[test]
fn ansi_bg() {
    let c = Color::new(255, 102, 102);
    assert_eq!(c.to_ansi_bg(), "\x1b[48;2;255;102;102m");
}
```

- [ ] **Step 6: Implement ANSI helpers**

```rust
impl Color {
    pub fn to_ansi_fg(&self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.r, self.g, self.b)
    }

    pub fn to_ansi_bg(&self) -> String {
        format!("\x1b[48;2;{};{};{}m", self.r, self.g, self.b)
    }
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib color`
Expected: All 6 tests PASS.

- [ ] **Step 8: Write tests for HSL darken/lighten**

The spec says `darken(0.8)` reduces lightness by 80%, `lighten(0.6)` increases by 60%. These operate in HSL space.

```rust
#[test]
fn darken_reduces_lightness() {
    let c = Color::new(255, 102, 102); // hsl(0, 100%, 70%)
    let d = c.darken(0.8);
    // L = 0.7, darken(0.8) -> L = 0.7 - 0.8 = clamped to 0.0 -> near black
    assert!(d.r < 10 && d.g < 10 && d.b < 10);
}

#[test]
fn darken_moderate() {
    let c = Color::new(128, 128, 255); // blueish, L ~ 0.75
    let d = c.darken(0.3);
    // Lightness should decrease; color should be darker
    assert!(d.r < 128 && d.b < 255);
}

#[test]
fn lighten_increases_lightness() {
    let c = Color::new(100, 50, 50); // dark red, L ~ 0.29
    let l = c.lighten(0.6);
    // L = 0.29 + 0.6 = 0.89 -> much lighter
    assert!(l.r > 200);
}

#[test]
fn lighten_clamps_to_white() {
    let c = Color::new(200, 200, 200); // L ~ 0.78
    let l = c.lighten(0.6);
    // L = 0.78 + 0.6 = 1.38 -> clamped to 1.0 -> white
    assert!(l.r >= 250 && l.g >= 250 && l.b >= 250);
}
```

- [ ] **Step 9: Implement HSL conversion and darken/lighten**

```rust
impl Color {
    fn to_hsl(&self) -> (f32, f32, f32) {
        let r = self.r as f32 / 255.0;
        let g = self.g as f32 / 255.0;
        let b = self.b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;

        if (max - min).abs() < 1e-6 {
            return (0.0, 0.0, l);
        }

        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };

        let h = if (max - r).abs() < 1e-6 {
            let mut h = (g - b) / d;
            if g < b {
                h += 6.0;
            }
            h
        } else if (max - g).abs() < 1e-6 {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };

        (h / 6.0, s, l)
    }

    fn from_hsl(h: f32, s: f32, l: f32) -> Color {
        if s.abs() < 1e-6 {
            let v = (l * 255.0).round() as u8;
            return Color::new(v, v, v);
        }

        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;

        fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
            if t < 0.0 { t += 1.0; }
            if t > 1.0 { t -= 1.0; }
            if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
            if t < 0.5 { return q; }
            if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
            p
        }

        let r = (hue_to_rgb(p, q, h + 1.0 / 3.0) * 255.0).round() as u8;
        let g = (hue_to_rgb(p, q, h) * 255.0).round() as u8;
        let b = (hue_to_rgb(p, q, h - 1.0 / 3.0) * 255.0).round() as u8;
        Color::new(r, g, b)
    }

    pub fn darken(&self, factor: f32) -> Color {
        let (h, s, l) = self.to_hsl();
        Color::from_hsl(h, s, (l - factor).max(0.0))
    }

    pub fn lighten(&self, factor: f32) -> Color {
        let (h, s, l) = self.to_hsl();
        Color::from_hsl(h, s, (l + factor).min(1.0))
    }
}
```

- [ ] **Step 10: Run tests to verify they pass**

Run: `cargo test --lib color`
Expected: All 10 tests PASS.

- [ ] **Step 11: Write tests for WCAG contrast ratio**

```rust
#[test]
fn contrast_black_white() {
    let black = Color::new(0, 0, 0);
    let white = Color::new(255, 255, 255);
    let ratio = contrast_ratio(black, white);
    assert!((ratio - 21.0).abs() < 0.1);
}

#[test]
fn contrast_same_color() {
    let c = Color::new(128, 128, 128);
    let ratio = contrast_ratio(c, c);
    assert!((ratio - 1.0).abs() < 0.01);
}

#[test]
fn contrast_threshold_check() {
    // A dark-on-medium scenario that should fail the 3.8 threshold
    let bg = Color::new(100, 100, 200);
    let fg = bg.darken(0.8);
    let ratio = contrast_ratio(bg, fg);
    // If ratio < 3.8, the design spec says switch to lighten(0.6)
    if ratio < 3.8 {
        let alt_fg = bg.lighten(0.6);
        let alt_ratio = contrast_ratio(bg, alt_fg);
        assert!(alt_ratio > ratio);
    }
}
```

- [ ] **Step 12: Implement contrast ratio**

```rust
pub fn contrast_ratio(c1: Color, c2: Color) -> f32 {
    let l1 = relative_luminance(c1);
    let l2 = relative_luminance(c2);
    let lighter = l1.max(l2);
    let darker = l1.min(l2);
    (lighter + 0.05) / (darker + 0.05)
}

fn relative_luminance(c: Color) -> f32 {
    fn linearize(v: u8) -> f32 {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(c.r) + 0.7152 * linearize(c.g) + 0.0722 * linearize(c.b)
}
```

- [ ] **Step 13: Run tests to verify they pass**

Run: `cargo test --lib color`
Expected: All 13 tests PASS.

- [ ] **Step 14: Write tests for Oklab gradient**

```rust
#[test]
fn gradient_two_stops() {
    let red = Color::new(255, 0, 0);
    let blue = Color::new(0, 0, 255);
    let g = gradient(red, blue, 2);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0], red);
    assert_eq!(g[1], blue);
}

#[test]
fn gradient_five_stops() {
    let c1 = Color::new(255, 102, 102);
    let c2 = Color::new(40, 40, 40);
    let g = gradient(c1, c2, 5);
    assert_eq!(g.len(), 5);
    assert_eq!(g[0], c1);
    assert_eq!(g[4], c2);
    // Middle stops should be between the two
    for stop in &g[1..4] {
        assert!(stop.r > c2.r && stop.r < c1.r);
    }
}

#[test]
fn gradient_single_stop() {
    let c = Color::new(100, 100, 100);
    let g = gradient(c, c, 1);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0], c);
}
```

- [ ] **Step 15: Implement Oklab gradient**

```rust
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

fn srgb_to_linear(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

fn color_to_oklab(c: Color) -> Oklab {
    let r = srgb_to_linear(c.r);
    let g = srgb_to_linear(c.g);
    let b = srgb_to_linear(c.b);

    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    Oklab {
        l: 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        a: 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        b: 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    }
}

fn oklab_to_color(lab: &Oklab) -> Color {
    let l_ = lab.l + 0.3963377774 * lab.a + 0.2158037573 * lab.b;
    let m_ = lab.l - 0.1055613458 * lab.a - 0.0638541728 * lab.b;
    let s_ = lab.l - 0.0894841775 * lab.a - 1.2914855480 * lab.b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;

    Color::new(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b))
}

pub fn gradient(from: Color, to: Color, steps: usize) -> Vec<Color> {
    if steps <= 1 {
        return vec![from];
    }
    let a = color_to_oklab(from);
    let b = color_to_oklab(to);
    (0..steps)
        .map(|i| {
            let t = i as f32 / (steps - 1) as f32;
            oklab_to_color(&Oklab {
                l: a.l + (b.l - a.l) * t,
                a: a.a + (b.a - a.a) * t,
                b: a.b + (b.b - a.b) * t,
            })
        })
        .collect()
}
```

- [ ] **Step 16: Run tests to verify they pass**

Run: `cargo test --lib color`
Expected: All 16 tests PASS.

- [ ] **Step 17: Add `mod color;` to main.rs and verify full build**

Add `mod color;` at the top of `src/main.rs`.

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles successfully.

- [ ] **Step 18: Commit**

```bash
git add src/color.rs src/main.rs
git commit -m "feat: add color module with hex parsing, HSL, Oklab gradient, WCAG contrast"
```

---

## Task 3: Icons Module

**Files:**
- Create: `src/icons.rs`
- Modify: `src/main.rs` (add `mod icons;`)

All constants and pure functions. No dependencies on other project modules.

- [ ] **Step 1: Create icons.rs with glyph constants**

```rust
// Powerline / UI glyphs
pub const PLE_LOWER_RIGHT_TRIANGLE: &str = "\u{E0BA}";
pub const PLE_RIGHT_HALF_CIRCLE_THICK: &str = "\u{E0B6}";
pub const LEFT_HALF_BLOCK: &str = "\u{258C}";
pub const FA_CHEVRON_LEFT: &str = "\u{F053}";
pub const FA_CHEVRON_RIGHT: &str = "\u{F054}";

// Tab icons
pub const TAB_DIR: &str = "\u{F0770}";     // md_folder_open
pub const TAB_HOME: &str = "\u{F02DC}";    // md_home
pub const TAB_PROCESS: &str = "\u{F070E}"; // md_run
pub const TAB_ICON: &str = "\u{F04E9}";    // md_tab
pub const ZOOM_ICON: &str = "\u{F1120}";   // md_magnify_plus_outline

// Mode icons
pub const MODE_LOCKED: &str = "\u{F033E}";      // md_lock
pub const MODE_RESIZE: &str = "\u{F0A68}";      // md_resize
pub const MODE_PANE: &str = "\u{F0535}";         // md_view_column
pub const MODE_TAB: &str = "\u{F04E9}";          // md_tab
pub const MODE_SCROLL: &str = "\u{F04D6}";       // md_unfold_more_horizontal
pub const MODE_SEARCH: &str = "\u{F002}";        // fa_search
pub const MODE_RENAME: &str = "\u{F07B0}";       // md_rename
pub const MODE_SESSION: &str = "\u{F0640}";      // md_collage
pub const MODE_MOVE: &str = "\u{F0655}";         // md_cursor_move
pub const MODE_PROMPT: &str = "\u{F0638}";       // md_console
pub const MODE_TMUX: &str = "\u{F0633}";         // md_apple_keyboard_command

// System segment icons
pub const WIFI_ACTIVE: &str = "\u{F05A9}";       // md_wifi
pub const WIFI_INACTIVE: &str = "\u{F092E}";     // md_wifi_strength_off_outline
pub const HOST_ICON: &str = "\u{F07C0}";         // md_desktop_classic
pub const CALENDAR: &str = "\u{F00F0}";          // md_calendar_clock

// Battery icons: [outline/low, low, medium, high]
pub const BATTERY_CHARGING: [&str; 4] = [
    "\u{F08DF}", // md_battery_charging_outline
    "\u{F12A4}", // md_battery_charging_low
    "\u{F12A5}", // md_battery_charging_medium
    "\u{F12A6}", // md_battery_charging_high
];

pub const BATTERY_DISCHARGING: [&str; 4] = [
    "\u{F008E}", // md_battery_outline
    "\u{F12A1}", // md_battery_low
    "\u{F12A2}", // md_battery_medium
    "\u{F12A3}", // md_battery_high
];

pub fn is_shell(name: &str) -> bool {
    matches!(name, "bash" | "zsh" | "fish" | "nu")
}

pub fn process_icon(name: &str) -> Option<&'static str> {
    match name {
        "bash" => Some("\u{E795}"),          // cod_terminal_bash
        "btm" => Some("\u{E224}"),           // mdi_chart_donut_variant
        "btop" => Some("\u{F0531}"),         // md_chart_areaspline
        "brew" => Some("\u{F007B}"),         // md_beer_outline
        "cargo" => Some("\u{E7A8}"),         // dev_rust
        "curl" => Some("\u{E241}"),          // mdi_flattr
        "docker" => Some("\u{E77D}"),        // linux_docker
        "docker-compose" => Some("\u{E77D}"),// linux_docker
        "fish" => Some("\u{F0BA5}"),         // md_fish
        "gh" => Some("\u{E709}"),            // dev_github_badge
        "git" => Some("\u{E65D}"),           // dev_git
        "go" => Some("\u{E627}"),            // seti_go
        "htop" => Some("\u{F0531}"),         // md_chart_areaspline
        "kubectl" => Some("\u{E77D}"),       // linux_docker
        "kuberlr" => Some("\u{E77D}"),       // linux_docker
        "lazydocker" => Some("\u{E77D}"),    // linux_docker
        "lazygit" => Some("\u{E702}"),       // cod_github
        "lua" => Some("\u{E620}"),           // seti_lua
        "make" => Some("\u{E673}"),          // seti_makefile
        "node" => Some("\u{E24F}"),          // mdi_hexagon
        "nvim" => Some("\u{E62B}"),          // custom_vim
        "pacman" => Some("\u{F0BAF}"),       // literal glyph
        "paru" => Some("\u{F0BAF}"),         // literal glyph
        "psql" => Some("\u{E76E}"),          // dev_postgresql
        "ruby" => Some("\u{E739}"),          // cod_ruby
        "sudo" => Some("\u{F292}"),          // fa_hashtag
        "vim" => Some("\u{E62B}"),           // dev_vim
        "wget" => Some("\u{E260}"),          // mdi_arrow_down_box
        "zsh" => Some("\u{E795}"),           // dev_terminal
        _ => None,
    }
}
```

- [ ] **Step 2: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_detection() {
        assert!(is_shell("bash"));
        assert!(is_shell("zsh"));
        assert!(is_shell("fish"));
        assert!(is_shell("nu"));
        assert!(!is_shell("nvim"));
        assert!(!is_shell("cargo"));
        assert!(!is_shell(""));
    }

    #[test]
    fn known_process_icons() {
        assert!(process_icon("nvim").is_some());
        assert!(process_icon("git").is_some());
        assert!(process_icon("cargo").is_some());
    }

    #[test]
    fn unknown_process_icon() {
        assert!(process_icon("my_custom_app").is_none());
    }

    #[test]
    fn battery_arrays_have_four_entries() {
        assert_eq!(BATTERY_CHARGING.len(), 4);
        assert_eq!(BATTERY_DISCHARGING.len(), 4);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib icons`
Expected: All 4 tests PASS.

- [ ] **Step 4: Add `mod icons;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/icons.rs src/main.rs
git commit -m "feat: add icons module with NerdFont constants and process icon map"
```

---

## Task 4: Truncation Module

**Files:**
- Create: `src/truncation.rs`
- Modify: `src/main.rs` (add `mod truncation;`)

Implements spec §5.3.1 exactly. Depends only on `unicode-width`.

- [ ] **Step 1: Write tests for truncation**

```rust
use unicode_width::UnicodeWidthStr;

pub fn truncated_text(text: &str, max_length: usize, truncation_point: f32) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_unchanged() {
        assert_eq!(truncated_text("hello", 10, 0.4), "hello");
    }

    #[test]
    fn exact_fit_unchanged() {
        assert_eq!(truncated_text("hello", 5, 0.4), "hello");
    }

    #[test]
    fn truncation_with_default_point() {
        // "hello world!" is 12 cells, max 8
        // ellipsis = " ... " (5 cells), available = 3
        // prefix_length = round(3 * 0.4) = round(1.2) = 1
        // suffix_length = 3 - 1 = 2
        let result = truncated_text("hello world!", 8, 0.4);
        assert_eq!(result, "h ... d!");
    }

    #[test]
    fn truncation_point_zero() {
        // truncation_point clamped to 0 -> ellipsis = "... " (4 cells)
        // "hello world!" max 8, available = 4, prefix = 0, suffix = 4
        let result = truncated_text("hello world!", 8, 0.0);
        assert_eq!(result, "... rld!");
    }

    #[test]
    fn truncation_point_one() {
        // truncation_point clamped to 1 -> ellipsis = " ..." (4 cells)
        // "hello world!" max 8, available = 4, prefix = 4, suffix = 0
        let result = truncated_text("hello world!", 8, 1.0);
        assert_eq!(result, "hell ...");
    }

    #[test]
    fn empty_text() {
        assert_eq!(truncated_text("", 10, 0.4), "");
    }

    #[test]
    fn max_length_zero() {
        assert_eq!(truncated_text("hello", 0, 0.4), "");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib truncation`
Expected: FAIL — `todo!()`.

- [ ] **Step 3: Implement truncated_text**

```rust
use unicode_width::UnicodeWidthStr;

pub fn truncated_text(text: &str, max_length: usize, truncation_point: f32) -> String {
    if max_length == 0 {
        return String::new();
    }

    let text_width = UnicodeWidthStr::width(text);
    if text_width <= max_length {
        return text.to_string();
    }

    let max_f = max_length as f32;
    let min_multiplier = 1.0 / max_f;

    let tp = if truncation_point > 1.0 - min_multiplier {
        1.0
    } else if truncation_point < min_multiplier {
        0.0
    } else {
        truncation_point
    };

    let ellipsis = if tp == 0.0 {
        "... "
    } else if tp == 1.0 {
        " ..."
    } else {
        " ... "
    };

    let ell_w = UnicodeWidthStr::width(ellipsis);
    if ell_w >= max_length {
        return ellipsis[..max_length.min(ellipsis.len())].to_string();
    }

    let available = max_length - ell_w;
    let prefix_length = (available as f32 * tp + 0.5) as usize;
    let suffix_length = available - prefix_length;

    let left = truncate_right(text, prefix_length);
    let right = truncate_left(text, suffix_length);

    format!("{}{}{}", left, ellipsis, right)
}

fn truncate_right(text: &str, max_cols: usize) -> &str {
    let mut width = 0;
    for (i, c) in text.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > max_cols {
            return &text[..i];
        }
        width += w;
    }
    text
}

fn truncate_left(text: &str, max_cols: usize) -> &str {
    let mut width = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for &(i, c) in chars.iter().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > max_cols {
            let next_idx = chars.iter().find(|&&(idx, _)| idx > i).map(|&(idx, _)| idx).unwrap_or(text.len());
            return &text[next_idx..];
        }
        width += w;
    }
    text
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib truncation`
Expected: All 7 tests PASS.

- [ ] **Step 5: Add `mod truncation;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 6: Commit**

```bash
git add src/truncation.rs src/main.rs
git commit -m "feat: add truncation module implementing spec §5.3.1"
```

---

## Task 5: Config Module

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

Depends on: `color.rs`, `icons.rs`

- [ ] **Step 1: Write the Config struct, IconLibrary, and defaults**

```rust
use std::collections::BTreeMap;
use zellij_tile::prelude::InputMode;

use crate::color::Color;

pub const DEFAULT_TAB_MAX_WIDTH: usize = 40;
pub const DEFAULT_TRUNCATION_POINT: f32 = 0.4;
pub const DEFAULT_FULLSCREEN_MIN_COLS: usize = 120;
pub const DEFAULT_SESSION_NAME: &str = "main";
pub const MIN_TAB_WIDTH: usize = 12;

#[derive(Clone, Debug)]
pub struct IconLibrary {
    pub tab_dir: String,
    pub tab_home: String,
    pub tab_process: String,
    pub tab_icon: String,
    pub zoom_icon: String,
    pub wifi_active: String,
    pub wifi_inactive: String,
    pub host_icon: String,
    pub calendar: String,
    pub battery_charging: [String; 4],
    pub battery_discharging: [String; 4],
}

impl Default for IconLibrary {
    fn default() -> Self {
        Self {
            tab_dir: crate::icons::TAB_DIR.to_string(),
            tab_home: crate::icons::TAB_HOME.to_string(),
            tab_process: crate::icons::TAB_PROCESS.to_string(),
            tab_icon: crate::icons::TAB_ICON.to_string(),
            zoom_icon: crate::icons::ZOOM_ICON.to_string(),
            wifi_active: crate::icons::WIFI_ACTIVE.to_string(),
            wifi_inactive: crate::icons::WIFI_INACTIVE.to_string(),
            host_icon: crate::icons::HOST_ICON.to_string(),
            calendar: crate::icons::CALENDAR.to_string(),
            battery_charging: crate::icons::BATTERY_CHARGING.map(|s| s.to_string()),
            battery_discharging: crate::icons::BATTERY_DISCHARGING.map(|s| s.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub tab_max_width: usize,
    pub tab_truncation_point: f32,
    pub tab_hide_single: bool,
    pub fullscreen_min_cols: usize,
    pub default_session_name: String,
    pub mode_colors: Vec<(InputMode, Color)>,
    pub icons: IconLibrary,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_max_width: DEFAULT_TAB_MAX_WIDTH,
            tab_truncation_point: DEFAULT_TRUNCATION_POINT,
            tab_hide_single: false,
            fullscreen_min_cols: DEFAULT_FULLSCREEN_MIN_COLS,
            default_session_name: DEFAULT_SESSION_NAME.to_string(),
            mode_colors: default_mode_colors(),
            icons: IconLibrary::default(),
        }
    }
}

fn default_mode_colors() -> Vec<(InputMode, Color)> {
    vec![
        (InputMode::Locked, Color::new(255, 102, 102)),
        (InputMode::Resize, Color::new(255, 204, 102)),
        (InputMode::Pane, Color::new(102, 204, 255)),
        (InputMode::Tab, Color::new(204, 153, 255)),
        (InputMode::Scroll, Color::new(153, 255, 204)),
        (InputMode::EnterSearch, Color::new(255, 255, 102)),
        (InputMode::Search, Color::new(255, 255, 102)),
        (InputMode::RenameTab, Color::new(255, 204, 153)),
        (InputMode::RenamePane, Color::new(255, 204, 153)),
        (InputMode::Session, Color::new(255, 153, 204)),
        (InputMode::Move, Color::new(102, 255, 204)),
        (InputMode::Prompt, Color::new(153, 204, 255)),
        (InputMode::Tmux, Color::new(204, 102, 255)),
    ]
}

impl Config {
    pub fn mode_color(&self, mode: InputMode) -> Option<Color> {
        self.mode_colors.iter().find(|(m, _)| *m == mode).map(|(_, c)| *c)
    }
}
```

- [ ] **Step 2: Write from_map parser and tests**

```rust
impl Config {
    pub fn from_map(map: BTreeMap<String, String>) -> Self {
        let mut config = Config::default();

        if let Some(v) = map.get("tab_max_width").and_then(|s| s.parse().ok()) {
            config.tab_max_width = v;
        }
        if let Some(v) = map.get("tab_truncation_point").and_then(|s| s.parse().ok()) {
            config.tab_truncation_point = v;
        }
        if let Some(v) = map.get("tab_hide_single") {
            config.tab_hide_single = v == "true";
        }
        if let Some(v) = map.get("fullscreen_min_cols").and_then(|s| s.parse().ok()) {
            config.fullscreen_min_cols = v;
        }
        if let Some(v) = map.get("default_session_name") {
            config.default_session_name = v.clone();
        }

        let mode_keys: &[(&str, InputMode)] = &[
            ("mode_color_locked", InputMode::Locked),
            ("mode_color_resize", InputMode::Resize),
            ("mode_color_pane", InputMode::Pane),
            ("mode_color_tab", InputMode::Tab),
            ("mode_color_scroll", InputMode::Scroll),
            ("mode_color_search", InputMode::Search),
            ("mode_color_session", InputMode::Session),
            ("mode_color_move", InputMode::Move),
            ("mode_color_tmux", InputMode::Tmux),
            ("mode_color_rename_tab", InputMode::RenameTab),
            ("mode_color_rename_pane", InputMode::RenamePane),
            ("mode_color_prompt", InputMode::Prompt),
        ];

        for (key, mode) in mode_keys {
            if let Some(color) = map.get(*key).and_then(|s| Color::parse_hex(s)) {
                if let Some(entry) = config.mode_colors.iter_mut().find(|(m, _)| *m == *mode) {
                    entry.1 = color;
                }
            }
        }

        let icon_keys: &[(&str, fn(&mut IconLibrary) -> &mut String)] = &[
            ("icon_wifi_active", |i: &mut IconLibrary| &mut i.wifi_active),
            ("icon_wifi_inactive", |i: &mut IconLibrary| &mut i.wifi_inactive),
            ("icon_host", |i: &mut IconLibrary| &mut i.host_icon),
            ("icon_tab_dir", |i: &mut IconLibrary| &mut i.tab_dir),
            ("icon_tab_home", |i: &mut IconLibrary| &mut i.tab_home),
            ("icon_tab_process", |i: &mut IconLibrary| &mut i.tab_process),
            ("icon_tab_icon", |i: &mut IconLibrary| &mut i.tab_icon),
            ("icon_calendar", |i: &mut IconLibrary| &mut i.calendar),
        ];

        for (key, accessor) in icon_keys {
            if let Some(v) = map.get(*key) {
                *accessor(&mut config.icons) = parse_icon_value(v);
            }
        }

        config
    }
}

fn parse_icon_value(s: &str) -> String {
    if let Some(hex) = s.strip_prefix("U+") {
        if let Ok(codepoint) = u32::from_str_radix(hex, 16) {
            if let Some(c) = char::from_u32(codepoint) {
                return c.to_string();
            }
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert_eq!(config.tab_max_width, 40);
        assert!((config.tab_truncation_point - 0.4).abs() < f32::EPSILON);
        assert!(!config.tab_hide_single);
        assert_eq!(config.fullscreen_min_cols, 120);
        assert_eq!(config.default_session_name, "main");
        assert_eq!(config.mode_colors.len(), 13);
    }

    #[test]
    fn parse_overrides() {
        let mut map = BTreeMap::new();
        map.insert("tab_max_width".to_string(), "50".to_string());
        map.insert("tab_hide_single".to_string(), "true".to_string());
        map.insert("mode_color_locked".to_string(), "#00ff00".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 50);
        assert!(config.tab_hide_single);
        assert_eq!(config.mode_color(InputMode::Locked), Some(Color::new(0, 255, 0)));
    }

    #[test]
    fn invalid_values_use_defaults() {
        let mut map = BTreeMap::new();
        map.insert("tab_max_width".to_string(), "not_a_number".to_string());
        map.insert("mode_color_locked".to_string(), "invalid".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 40);
        assert_eq!(config.mode_color(InputMode::Locked), Some(Color::new(255, 102, 102)));
    }

    #[test]
    fn unknown_keys_ignored() {
        let mut map = BTreeMap::new();
        map.insert("nonexistent_key".to_string(), "value".to_string());
        let config = Config::from_map(map);
        assert_eq!(config.tab_max_width, 40);
    }

    #[test]
    fn parse_icon_codepoint() {
        assert_eq!(parse_icon_value("U+F05A9"), "\u{F05A9}");
    }

    #[test]
    fn parse_icon_literal() {
        assert_eq!(parse_icon_value("X"), "X");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib config`
Expected: All 6 tests PASS.

- [ ] **Step 4: Add `mod config;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: add config module with KDL map parsing and defaults"
```

---

## Task 6: State Module

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

Depends on: `config.rs` (for `MIN_TAB_WIDTH`)

- [ ] **Step 1: Create state.rs with AppState and CachedValue**

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use zellij_tile::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum BatteryState {
    Charging,
    Discharging,
    Full,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub state: BatteryState,
}

#[derive(Debug, Clone)]
pub struct CachedValue<T> {
    pub value: Option<T>,
    pub last_updated: Option<Instant>,
    pub ttl: Duration,
    pub in_flight: bool,
}

impl<T> CachedValue<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            value: None,
            last_updated: None,
            ttl,
            in_flight: false,
        }
    }

    pub fn is_expired(&self) -> bool {
        if self.in_flight {
            return false;
        }
        match self.last_updated {
            None => true,
            Some(t) => t.elapsed() >= self.ttl,
        }
    }

    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.last_updated = Some(Instant::now());
        self.in_flight = false;
    }
}

#[derive(Debug, Clone)]
pub struct ScrollState {
    pub left: usize,
    pub right: usize,
    pub has_left: bool,
    pub has_right: bool,
}

pub struct AppState {
    pub mode: InputMode,
    pub tabs: Vec<TabInfo>,
    pub panes: HashMap<usize, Vec<PaneInfo>>,
    pub session_name: String,
    pub battery: CachedValue<BatteryInfo>,
    pub wifi: CachedValue<bool>,
    pub cols: usize,
    pub dirty: bool,
    pub got_permissions: bool,
    pub pending_events: Vec<Event>,
    pub scroll_state: Option<ScrollState>,
    pub last_output: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            mode: InputMode::Normal,
            tabs: Vec::new(),
            panes: HashMap::new(),
            session_name: String::new(),
            battery: CachedValue::new(Duration::from_secs(30)),
            wifi: CachedValue::new(Duration::from_secs(10)),
            cols: 0,
            dirty: true,
            got_permissions: false,
            pending_events: Vec::new(),
            scroll_state: None,
            last_output: String::new(),
        }
    }
}

impl AppState {
    pub fn active_tab_index(&self) -> Option<usize> {
        self.tabs.iter().position(|t| t.active)
    }

    pub fn panes_for_tab(&self, tab_position: usize) -> &[PaneInfo] {
        self.panes.get(&tab_position).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn focused_pane_for_tab(&self, tab_position: usize) -> Option<&PaneInfo> {
        self.panes_for_tab(tab_position).iter().find(|p| p.is_focused && !p.is_plugin)
    }

    pub fn any_pane_zoomed(&self, tab_position: usize) -> bool {
        self.panes_for_tab(tab_position).iter().any(|p| p.is_fullscreen && !p.is_plugin)
    }
}
```

- [ ] **Step 2: Write tests for CachedValue**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_value_starts_expired() {
        let cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        assert!(cv.is_expired());
        assert!(cv.value.is_none());
    }

    #[test]
    fn cached_value_not_expired_after_set() {
        let mut cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        cv.set(true);
        assert!(!cv.is_expired());
        assert_eq!(cv.value, Some(true));
    }

    #[test]
    fn cached_value_in_flight_prevents_expiry() {
        let mut cv: CachedValue<bool> = CachedValue::new(Duration::from_secs(10));
        cv.in_flight = true;
        assert!(!cv.is_expired());
    }

    #[test]
    fn default_app_state() {
        let state = AppState::default();
        assert_eq!(state.mode, InputMode::Normal);
        assert!(state.tabs.is_empty());
        assert!(state.dirty);
        assert!(!state.got_permissions);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib state`
Expected: All 4 tests PASS.

- [ ] **Step 4: Add `mod state;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/main.rs
git commit -m "feat: add state module with AppState and CachedValue"
```

---

## Task 7: Tabs Module

**Files:**
- Create: `src/tabs.rs`
- Modify: `src/main.rs` (add `mod tabs;`)

Depends on: `icons.rs`, `truncation.rs`, `config.rs`, `state.rs`

- [ ] **Step 1: Write tab title composition**

```rust
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::icons;
use crate::state::AppState;
use crate::truncation::truncated_text;

pub struct TabTitle {
    pub index_str: String,
    pub body: String,
    pub extra_icons: String,
}

pub fn compose_tab_title(
    tab_position: usize,
    tab_name: &str,
    state: &AppState,
    config: &Config,
) -> TabTitle {
    let index_str = format!("{} ", tab_position + 1);

    let pane = state.focused_pane_for_tab(tab_position);
    let is_zoomed = state.any_pane_zoomed(tab_position);

    let body = choose_body(tab_name, pane, config);

    let mut extra_icons = String::new();
    if is_zoomed {
        extra_icons.push_str(&format!(" {}", config.icons.zoom_icon));
    }

    TabTitle {
        index_str,
        body,
        extra_icons,
    }
}

fn choose_body(tab_name: &str, pane: Option<&zellij_tile::prelude::PaneInfo>, config: &Config) -> String {
    if !tab_name.is_empty() && !is_default_tab_name(tab_name) {
        return format!("{} {}", config.icons.tab_icon, tab_name);
    }

    if let Some(pane) = pane {
        if let Some(ref cmd) = pane.terminal_command {
            let proc = basename(cmd);
            if !proc.is_empty() && !icons::is_shell(&proc) {
                let icon = icons::process_icon(&proc)
                    .unwrap_or(&config.icons.tab_process);
                return format!("{} {}", icon, proc);
            }
        }

        let title = &pane.title;
        let dir = extract_cwd_from_title(title);
        if !dir.is_empty() {
            let base = basename(&dir);
            if base == "~" || is_home_dir(&dir) {
                return format!("{} ~", config.icons.tab_home);
            }
            return format!("{} {}", config.icons.tab_dir, base);
        }
    }

    format!("{} {}", config.icons.tab_process, tab_name)
}

fn is_default_tab_name(name: &str) -> bool {
    name.starts_with("Tab #")
}

fn basename(path: &str) -> String {
    path.rsplit(&['/', '\\'][..]).next().unwrap_or(path).to_string()
}

fn extract_cwd_from_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('~') {
        return trimmed.to_string();
    }
    if let Some((_target, dir)) = trimmed.split_once(": ") {
        return dir.to_string();
    }
    String::new()
}

fn is_home_dir(path: &str) -> bool {
    path == "~" || path.ends_with("/~")
}

pub fn render_tab_title(title: &TabTitle, max_width: usize, truncation_point: f32) -> String {
    let edge_width = 1;
    let padding = 2;
    let index_width = UnicodeWidthStr::width(title.index_str.as_str());

    let extra_width = UnicodeWidthStr::width(title.extra_icons.as_str());
    let max_body = max_width.saturating_sub(padding + index_width + edge_width + extra_width);

    let body = truncated_text(&title.body, max_body, truncation_point);

    format!("{}{}{}", title.index_str, body, title.extra_icons)
}
```

- [ ] **Step 2: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_unix_path() {
        assert_eq!(basename("/home/user/projects"), "projects");
    }

    #[test]
    fn basename_just_name() {
        assert_eq!(basename("nvim"), "nvim");
    }

    #[test]
    fn is_default_tab_name_true() {
        assert!(is_default_tab_name("Tab #1"));
        assert!(is_default_tab_name("Tab #12"));
    }

    #[test]
    fn is_default_tab_name_false() {
        assert!(!is_default_tab_name("my-project"));
        assert!(!is_default_tab_name(""));
    }

    #[test]
    fn extract_cwd_absolute_path() {
        assert_eq!(extract_cwd_from_title("/home/user/code"), "/home/user/code");
    }

    #[test]
    fn extract_cwd_tilde() {
        assert_eq!(extract_cwd_from_title("~"), "~");
        assert_eq!(extract_cwd_from_title("~/projects"), "~/projects");
    }

    #[test]
    fn extract_cwd_from_colon_title() {
        assert_eq!(extract_cwd_from_title("user@host: /home/user"), "/home/user");
    }

    #[test]
    fn extract_cwd_no_path() {
        assert_eq!(extract_cwd_from_title("nvim"), "");
    }

    #[test]
    fn render_tab_title_no_truncation() {
        let title = TabTitle {
            index_str: "1 ".to_string(),
            body: "\u{F0770} code".to_string(),
            extra_icons: String::new(),
        };
        let rendered = render_tab_title(&title, 40, 0.4);
        assert!(rendered.starts_with("1 "));
        assert!(rendered.contains("code"));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib tabs`
Expected: All 9 tests PASS.

- [ ] **Step 4: Add `mod tabs;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/tabs.rs src/main.rs
git commit -m "feat: add tabs module with title composition and priority chain"
```

---

## Task 8: Layout Module

**Files:**
- Create: `src/layout.rs`
- Modify: `src/main.rs` (add `mod layout;`)

Depends on: `tabs.rs`, `config.rs`, `state.rs`

Implements spec §5.4.4 (equalization) and §5.4.5 (scrolling). These are the most algorithmic parts of the project.

- [ ] **Step 1: Write equalize_tab_widths tests**

```rust
#[derive(Debug, Clone)]
pub struct TabWidth {
    pub index: usize,
    pub width: usize,
    pub natural: usize,
}

pub fn equalize_tab_widths(tab_widths: &mut [TabWidth], available: usize) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tabs(widths: &[usize]) -> Vec<TabWidth> {
        widths.iter().enumerate().map(|(i, &w)| TabWidth { index: i, width: w, natural: w }).collect()
    }

    fn total_width(tabs: &[TabWidth]) -> usize {
        tabs.iter().map(|t| t.width).sum()
    }

    #[test]
    fn equalize_no_op_when_fits() {
        let mut tabs = make_tabs(&[10, 15, 20]);
        equalize_tab_widths(&mut tabs, 50);
        assert_eq!(tabs[0].width, 10);
        assert_eq!(tabs[1].width, 15);
        assert_eq!(tabs[2].width, 20);
    }

    #[test]
    fn equalize_shrinks_to_budget() {
        let mut tabs = make_tabs(&[20, 20, 20]);
        equalize_tab_widths(&mut tabs, 30);
        assert_eq!(total_width(&tabs), 30);
    }

    #[test]
    fn equalize_phase2a_redistributes() {
        // Tabs [10, 30, 30], available = 50
        // min = 10, floor all to 10 -> 30
        // remaining = 50 - 30 = 20
        // tab0 gets min(0, 20) = 0, tab1 gets min(20, 20) = 20, tab2 gets min(20, 0) = 0
        let mut tabs = make_tabs(&[10, 30, 30]);
        equalize_tab_widths(&mut tabs, 50);
        assert_eq!(total_width(&tabs), 50);
        assert_eq!(tabs[0].width, 10);
        assert_eq!(tabs[1].width, 30);
        assert_eq!(tabs[2].width, 10);
    }

    #[test]
    fn equalize_phase2b_uniform_shrink() {
        // Tabs [10, 10, 10], available = 21
        // min = 10, equalized_sum = 30 > 21
        // overflow = 9, base_reduction = 3, extra = 0
        let mut tabs = make_tabs(&[10, 10, 10]);
        equalize_tab_widths(&mut tabs, 21);
        assert_eq!(total_width(&tabs), 21);
        assert_eq!(tabs[0].width, 7);
        assert_eq!(tabs[1].width, 7);
        assert_eq!(tabs[2].width, 7);
    }

    #[test]
    fn equalize_phase2b_with_remainder() {
        // Tabs [10, 10, 10], available = 20
        // min = 10, equalized_sum = 30 > 20
        // overflow = 10, base_reduction = 3, extra = 1
        // all get 7, then tab[2] gets -1 more = 6
        let mut tabs = make_tabs(&[10, 10, 10]);
        equalize_tab_widths(&mut tabs, 20);
        assert_eq!(total_width(&tabs), 20);
        assert_eq!(tabs[0].width, 7);
        assert_eq!(tabs[1].width, 7);
        assert_eq!(tabs[2].width, 6);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layout`
Expected: FAIL — `todo!()`.

- [ ] **Step 3: Implement equalize_tab_widths**

```rust
pub fn equalize_tab_widths(tab_widths: &mut [TabWidth], available: usize) {
    let total: usize = tab_widths.iter().map(|t| t.width).sum();
    if total <= available {
        return;
    }

    let min_w = tab_widths.iter().map(|t| t.width).min().unwrap_or(0);
    let n = tab_widths.len();

    for tw in tab_widths.iter_mut() {
        tw.natural = tw.width;
        tw.width = min_w;
    }

    let equalized_sum = min_w * n;

    if equalized_sum <= available {
        let mut remaining = available - equalized_sum;
        for tw in tab_widths.iter_mut() {
            let add = (tw.natural - tw.width).min(remaining);
            tw.width += add;
            remaining -= add;
            if remaining == 0 {
                break;
            }
        }
    } else {
        let overflow = equalized_sum - available;
        let base_reduction = overflow / n;
        let extra = overflow - base_reduction * n;

        for tw in tab_widths.iter_mut() {
            tw.width = min_w - base_reduction;
        }

        for i in 0..extra {
            tab_widths[n - 1 - i].width -= 1;
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib layout`
Expected: All 5 tests PASS.

- [ ] **Step 5: Write visible_tab_range tests**

```rust
pub fn visible_tab_range(
    tab_count: usize,
    active_idx: usize,
    active_width: usize,
    available_total: usize,
    min_tab_width: usize,
) -> (usize, usize) {
    todo!()
}

#[test]
fn scroll_single_tab() {
    let (l, r) = visible_tab_range(1, 0, 30, 80, 12);
    assert_eq!((l, r), (0, 0));
}

#[test]
fn scroll_centers_on_active() {
    // 10 tabs, active = 4, active_width = 30, available = 90, min = 12
    // Should fit: 30 + 5*12 = 90 -> fits 5 inactive + active
    let (l, r) = visible_tab_range(10, 4, 30, 90, 12);
    assert!(l <= 4 && r >= 4);
    assert!(r - l + 1 <= 7); // at most 6 tabs
}

#[test]
fn scroll_active_at_start() {
    let (l, r) = visible_tab_range(10, 0, 30, 66, 12);
    assert_eq!(l, 0);
    assert!(r >= 0);
}

#[test]
fn scroll_active_at_end() {
    let (l, r) = visible_tab_range(10, 9, 30, 66, 12);
    assert_eq!(r, 9);
    assert!(l <= 9);
}
```

- [ ] **Step 6: Implement visible_tab_range**

```rust
pub fn visible_tab_range(
    tab_count: usize,
    active_idx: usize,
    active_width: usize,
    available_total: usize,
    min_tab_width: usize,
) -> (usize, usize) {
    let mut l = active_idx;
    let mut r = active_idx;
    let mut visible_sum = active_width;

    loop {
        let mut expanded = false;
        let left_count = active_idx - l;
        let right_count = r - active_idx;

        if left_count <= right_count {
            if l > 0 && visible_sum + min_tab_width <= available_total {
                l -= 1;
                visible_sum += min_tab_width;
                expanded = true;
            }
            if r + 1 < tab_count && visible_sum + min_tab_width <= available_total {
                r += 1;
                visible_sum += min_tab_width;
                expanded = true;
            }
        } else {
            if r + 1 < tab_count && visible_sum + min_tab_width <= available_total {
                r += 1;
                visible_sum += min_tab_width;
                expanded = true;
            }
            if l > 0 && visible_sum + min_tab_width <= available_total {
                l -= 1;
                visible_sum += min_tab_width;
                expanded = true;
            }
        }

        if !expanded {
            break;
        }
    }

    (l, r)
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib layout`
Expected: All 9 tests PASS.

- [ ] **Step 8: Write compute_tab_layout that ties equalization and scrolling together**

```rust
use crate::config::{Config, MIN_TAB_WIDTH};
use crate::state::ScrollState;

pub struct TabLayout {
    pub widths: Vec<usize>,
    pub scroll: Option<ScrollState>,
}

pub fn compute_tab_layout(
    tab_count: usize,
    active_idx: usize,
    natural_widths: &[usize],
    available_total: usize,
    config: &Config,
) -> TabLayout {
    if tab_count == 0 {
        return TabLayout { widths: vec![], scroll: None };
    }

    let active_width = natural_widths[active_idx].min(config.tab_max_width);
    let available_inactive = available_total.saturating_sub(active_width);

    let mut inactive: Vec<TabWidth> = natural_widths.iter().enumerate()
        .filter(|&(i, _)| i != active_idx)
        .map(|(i, &w)| TabWidth { index: i, width: w, natural: w })
        .collect();

    let total_needed: usize = inactive.iter().map(|t| t.width).sum();

    if total_needed <= available_inactive {
        let mut widths: Vec<usize> = natural_widths.to_vec();
        widths[active_idx] = active_width;
        return TabLayout { widths, scroll: None };
    }

    equalize_tab_widths(&mut inactive, available_inactive);

    let any_below_min = inactive.iter().any(|t| t.width < MIN_TAB_WIDTH);

    if !any_below_min {
        let mut widths = vec![0; tab_count];
        widths[active_idx] = active_width;
        for tw in &inactive {
            widths[tw.index] = tw.width;
        }
        return TabLayout { widths, scroll: None };
    }

    let (mut l, mut r) = visible_tab_range(tab_count, active_idx, active_width, available_total, MIN_TAB_WIDTH);

    let has_left = l > 0;
    let has_right = r < tab_count - 1;
    let indicator_cols = (if has_left { 1 } else { 0 }) + (if has_right { 1 } else { 0 });

    if indicator_cols > 0 {
        let adjusted_available = available_total.saturating_sub(indicator_cols);
        let (nl, nr) = visible_tab_range(tab_count, active_idx, active_width, adjusted_available, MIN_TAB_WIDTH);
        l = nl;
        r = nr;
    }

    let has_left = l > 0;
    let has_right = r < tab_count - 1;
    let indicator_cols = (if has_left { 1 } else { 0 }) + (if has_right { 1 } else { 0 });

    let mut visible_inactive: Vec<TabWidth> = (l..=r)
        .filter(|&i| i != active_idx)
        .map(|i| TabWidth { index: i, width: natural_widths[i], natural: natural_widths[i] })
        .collect();

    let vis_available = available_total.saturating_sub(active_width + indicator_cols);
    equalize_tab_widths(&mut visible_inactive, vis_available);

    let mut widths = vec![0; tab_count];
    widths[active_idx] = active_width;
    for tw in &visible_inactive {
        widths[tw.index] = tw.width.max(MIN_TAB_WIDTH);
    }

    let scroll = Some(ScrollState {
        left: l,
        right: r,
        has_left,
        has_right,
    });

    TabLayout { widths, scroll }
}

#[test]
fn layout_all_fit() {
    let config = Config::default();
    let layout = compute_tab_layout(3, 1, &[20, 25, 15], 100, &config);
    assert!(layout.scroll.is_none());
    assert_eq!(layout.widths[1], 25);
}

#[test]
fn layout_equalization_needed() {
    let config = Config::default();
    let layout = compute_tab_layout(3, 0, &[30, 30, 30], 60, &config);
    assert!(layout.scroll.is_none());
    let total: usize = layout.widths.iter().sum();
    assert!(total <= 60);
}

#[test]
fn layout_scrolling_needed() {
    let config = Config::default();
    // 20 tabs at width 30 each, only 80 cols available
    let widths = vec![30; 20];
    let layout = compute_tab_layout(20, 10, &widths, 80, &config);
    assert!(layout.scroll.is_some());
    let scroll = layout.scroll.unwrap();
    assert!(scroll.left <= 10);
    assert!(scroll.right >= 10);
}
```

- [ ] **Step 9: Run all layout tests**

Run: `cargo test --lib layout`
Expected: All 12 tests PASS.

- [ ] **Step 10: Add `mod layout;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 11: Commit**

```bash
git add src/layout.rs src/main.rs
git commit -m "feat: add layout module with equalization and scrolling algorithms"
```

---

## Task 9: System Module

**Files:**
- Create: `src/system.rs`
- Modify: `src/main.rs` (add `mod system;`)

Depends on: `state.rs`

- [ ] **Step 1: Create system.rs with battery and wifi command dispatch and parsing**

```rust
use std::collections::BTreeMap;
use zellij_tile::prelude::run_command;

use crate::state::{AppState, BatteryInfo, BatteryState};

pub const CTX_KEY: &str = "source";
pub const CTX_BATTERY: &str = "battery";
pub const CTX_WIFI: &str = "wifi";

pub fn maybe_refresh_battery(state: &mut AppState) {
    if !state.battery.is_expired() {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_BATTERY.to_string());

    #[cfg(target_os = "macos")]
    run_command(&["pmset", "-g", "batt"], ctx);

    #[cfg(target_os = "linux")]
    run_command(
        &[
            "sh", "-c",
            "cat /sys/class/power_supply/BAT0/capacity 2>/dev/null || cat /sys/class/power_supply/BAT1/capacity 2>/dev/null; echo; cat /sys/class/power_supply/BAT0/status 2>/dev/null || cat /sys/class/power_supply/BAT1/status 2>/dev/null",
        ],
        ctx,
    );

    state.battery.in_flight = true;
}

pub fn maybe_refresh_wifi(state: &mut AppState) {
    if !state.wifi.is_expired() {
        return;
    }

    let mut ctx = BTreeMap::new();
    ctx.insert(CTX_KEY.to_string(), CTX_WIFI.to_string());

    #[cfg(target_os = "macos")]
    run_command(&["networksetup", "-getairportpower", "en0"], ctx);

    #[cfg(target_os = "linux")]
    run_command(&["nmcli", "radio", "wifi"], ctx);

    state.wifi.in_flight = true;
}

pub fn handle_command_result(
    exit_code: Option<i32>,
    stdout: &[u8],
    _stderr: &[u8],
    context: &BTreeMap<String, String>,
    state: &mut AppState,
) {
    let source = match context.get(CTX_KEY) {
        Some(s) => s.as_str(),
        None => return,
    };

    let output = String::from_utf8_lossy(stdout);

    match source {
        CTX_BATTERY => {
            state.battery.in_flight = false;
            if exit_code != Some(0) {
                return;
            }
            if let Some(info) = parse_battery_output(&output) {
                state.battery.set(info);
            }
        }
        CTX_WIFI => {
            state.wifi.in_flight = false;
            if exit_code != Some(0) {
                state.wifi.set(false);
                return;
            }
            state.wifi.set(parse_wifi_output(&output));
        }
        _ => {}
    }
}

fn parse_battery_output(output: &str) -> Option<BatteryInfo> {
    #[cfg(target_os = "macos")]
    return parse_battery_macos(output);

    #[cfg(target_os = "linux")]
    return parse_battery_linux(output);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}

#[cfg(target_os = "macos")]
fn parse_battery_macos(output: &str) -> Option<BatteryInfo> {
    // pmset output looks like:
    // Now drawing from 'Battery Power'
    //  -InternalBattery-0 (id=...)  85%; charging; 0:42 remaining present: true
    for line in output.lines() {
        let line = line.trim();
        if !line.contains("InternalBattery") {
            continue;
        }
        let pct = line.split(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse::<u8>().ok())?;

        let state = if line.contains("charged") || line.contains("finishing charge") {
            BatteryState::Full
        } else if line.contains("charging") {
            BatteryState::Charging
        } else if line.contains("discharging") {
            BatteryState::Discharging
        } else {
            BatteryState::Unknown
        };

        return Some(BatteryInfo { percentage: pct, state });
    }
    None
}

#[cfg(target_os = "linux")]
fn parse_battery_linux(output: &str) -> Option<BatteryInfo> {
    let mut lines = output.lines();
    let pct: u8 = lines.next()?.trim().parse().ok()?;
    let status_str = lines.next()?.trim();
    let state = match status_str {
        "Full" => BatteryState::Full,
        "Charging" => BatteryState::Charging,
        "Discharging" | "Not charging" => BatteryState::Discharging,
        _ => BatteryState::Unknown,
    };
    Some(BatteryInfo { percentage: pct, state })
}

fn parse_wifi_output(output: &str) -> bool {
    let output = output.trim().to_lowercase();

    #[cfg(target_os = "macos")]
    { output.contains("on") }

    #[cfg(target_os = "linux")]
    { output.starts_with("enabled") }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    false
}
```

- [ ] **Step 2: Write tests for parsing**

The command dispatch functions call `run_command` (Zellij API) so they can't be unit tested. But the parsers can be.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_macos_battery_charging() {
        let output = r#"Now drawing from 'AC Power'
 -InternalBattery-0 (id=4522083)	72%; charging; 1:13 remaining present: true"#;
        let info = parse_battery_macos(output).unwrap();
        assert_eq!(info.percentage, 72);
        assert_eq!(info.state, BatteryState::Charging);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_macos_battery_discharging() {
        let output = r#"Now drawing from 'Battery Power'
 -InternalBattery-0 (id=4522083)	85%; discharging; 3:42 remaining present: true"#;
        let info = parse_battery_macos(output).unwrap();
        assert_eq!(info.percentage, 85);
        assert_eq!(info.state, BatteryState::Discharging);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_macos_battery_full() {
        let output = r#"Now drawing from 'AC Power'
 -InternalBattery-0 (id=4522083)	100%; charged; 0:00 remaining present: true"#;
        let info = parse_battery_macos(output).unwrap();
        assert_eq!(info.percentage, 100);
        assert_eq!(info.state, BatteryState::Full);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_macos_wifi_on() {
        assert!(parse_wifi_output("Wi-Fi Power (en0): On\n"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_macos_wifi_off() {
        assert!(!parse_wifi_output("Wi-Fi Power (en0): Off\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_linux_battery() {
        let output = "85\nDischarging\n";
        let info = parse_battery_linux(output).unwrap();
        assert_eq!(info.percentage, 85);
        assert_eq!(info.state, BatteryState::Discharging);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_linux_wifi_enabled() {
        assert!(parse_wifi_output("enabled\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_linux_wifi_disabled() {
        assert!(!parse_wifi_output("disabled\n"));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib system`
Expected: Platform-appropriate tests PASS.

- [ ] **Step 4: Add `mod system;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles. Note: `run_command` is a Zellij shim that compiles for WASM; the `#[cfg(target_os)]` blocks will resolve at compile time (for wasm32-wasip1, neither macos nor linux match, so the dispatch functions become no-ops — this is fine; the actual target is set in the Zellij host process config or via cross-compilation flags).

- [ ] **Step 5: Commit**

```bash
git add src/system.rs src/main.rs
git commit -m "feat: add system module for async battery/wifi queries via run_command"
```

---

## Task 10: Segments Module

**Files:**
- Create: `src/segments.rs`
- Modify: `src/main.rs` (add `mod segments;`)

Depends on: `color.rs`, `icons.rs`, `config.rs`, `state.rs`

- [ ] **Step 1: Write format_segment and segment functions**

```rust
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::InputMode;

use crate::color::{self, Color};
use crate::config::Config;
use crate::icons;
use crate::state::{AppState, BatteryState};

pub struct Segment {
    pub text: String,
    pub width: usize,
    pub bg: Color,
}

pub fn format_segment(bg: Color, text: &str, is_last: bool) -> Segment {
    let mut fg = bg.darken(0.8);
    if color::contrast_ratio(bg, fg) < 3.8 {
        fg = bg.lighten(0.6);
    }

    let body = if is_last {
        format!(" {}", text)
    } else {
        format!(" {} ", text)
    };

    let width = UnicodeWidthStr::width(body.as_str());
    let styled = format!("{}{}{}", bg.to_ansi_bg(), fg.to_ansi_fg(), body);

    Segment { text: styled, width, bg }
}

pub fn divider(left_bg: Color, right_bg: Color) -> String {
    format!(
        "{}{}{}",
        left_bg.to_ansi_bg(),
        right_bg.to_ansi_fg(),
        icons::PLE_LOWER_RIGHT_TRIANGLE,
    )
}

pub fn divider_width() -> usize {
    UnicodeWidthStr::width(icons::PLE_LOWER_RIGHT_TRIANGLE)
}

pub fn mode_segment(mode: InputMode, bg: Color, is_last: bool, config: &Config) -> Option<Segment> {
    if mode == InputMode::Normal {
        return None;
    }

    let (icon, label) = mode_icon_label(mode);
    let text = format!("{} {}", icon, label);
    Some(format_segment(bg, &text, is_last))
}

fn mode_icon_label(mode: InputMode) -> (&'static str, &'static str) {
    match mode {
        InputMode::Locked => (icons::MODE_LOCKED, "Locked"),
        InputMode::Resize => (icons::MODE_RESIZE, "Resize"),
        InputMode::Pane => (icons::MODE_PANE, "Pane"),
        InputMode::Tab => (icons::MODE_TAB, "Tab"),
        InputMode::Scroll => (icons::MODE_SCROLL, "Scroll"),
        InputMode::EnterSearch => (icons::MODE_SEARCH, "Search"),
        InputMode::Search => (icons::MODE_SEARCH, "Search"),
        InputMode::RenameTab => (icons::MODE_RENAME, "Rename"),
        InputMode::RenamePane => (icons::MODE_RENAME, "Rename"),
        InputMode::Session => (icons::MODE_SESSION, "Session"),
        InputMode::Move => (icons::MODE_MOVE, "Move"),
        InputMode::Prompt => (icons::MODE_PROMPT, "Prompt"),
        InputMode::Tmux => (icons::MODE_TMUX, "Tmux"),
        InputMode::Normal => (icons::MODE_TAB, "Normal"),
    }
}

pub fn session_segment(session_name: &str, bg: Color, is_last: bool, config: &Config) -> Option<Segment> {
    if session_name.is_empty() || session_name == config.default_session_name {
        return None;
    }
    let text = format!("{} {}", icons::MODE_SESSION, session_name);
    Some(format_segment(bg, &text, is_last))
}

pub fn battery_segment(state: &AppState, bg: Color, is_last: bool, config: &Config) -> Segment {
    match &state.battery.value {
        Some(info) => {
            let icon = battery_icon(info.percentage, &info.state, config);
            let text = format!("{}% {}", info.percentage, icon);
            format_segment(bg, &text, is_last)
        }
        None => {
            format_segment(bg, &config.icons.host_icon, is_last)
        }
    }
}

fn battery_icon(pct: u8, batt_state: &BatteryState, config: &Config) -> &str {
    let idx = if pct >= 90 { 3 } else if pct >= 40 { 2 } else if pct > 5 { 1 } else { 0 };

    match batt_state {
        BatteryState::Full => &config.icons.battery_charging[3],
        BatteryState::Charging => &config.icons.battery_charging[idx],
        _ => &config.icons.battery_discharging[idx],
    }
}

pub fn wifi_segment(state: &AppState, bg: Color, is_last: bool, config: &Config) -> Segment {
    let icon = match state.wifi.value {
        Some(true) => &config.icons.wifi_active,
        _ => &config.icons.wifi_inactive,
    };
    format_segment(bg, icon, is_last)
}

pub fn time_segment(bg: Color, is_last: bool, config: &Config) -> Segment {
    let now = chrono::Local::now();
    let time_str = now.format("%a %b %-e %-l:%M%P").to_string();
    let text = format!("{} {}", config.icons.calendar, time_str);
    format_segment(bg, &text, is_last)
}
```

- [ ] **Step 2: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_segment_has_leading_space() {
        let seg = format_segment(Color::new(100, 100, 100), "test", false);
        assert!(seg.width >= 6); // " test " = 6
    }

    #[test]
    fn format_segment_last_no_trailing_space() {
        let seg_last = format_segment(Color::new(100, 100, 100), "test", true);
        let seg_mid = format_segment(Color::new(100, 100, 100), "test", false);
        assert_eq!(seg_last.width, seg_mid.width - 1);
    }

    #[test]
    fn mode_segment_suppressed_for_normal() {
        let config = Config::default();
        assert!(mode_segment(InputMode::Normal, Color::new(100, 100, 100), false, &config).is_none());
    }

    #[test]
    fn mode_segment_present_for_locked() {
        let config = Config::default();
        let seg = mode_segment(InputMode::Locked, Color::new(255, 102, 102), false, &config);
        assert!(seg.is_some());
        assert!(seg.unwrap().width > 0);
    }

    #[test]
    fn session_segment_suppressed_for_main() {
        let config = Config::default();
        assert!(session_segment("main", Color::new(100, 100, 100), false, &config).is_none());
    }

    #[test]
    fn session_segment_shown_for_other() {
        let config = Config::default();
        let seg = session_segment("dev", Color::new(100, 100, 100), false, &config);
        assert!(seg.is_some());
    }

    #[test]
    fn battery_icon_thresholds() {
        let config = Config::default();
        assert_eq!(battery_icon(95, &BatteryState::Discharging, &config), config.icons.battery_discharging[3]);
        assert_eq!(battery_icon(50, &BatteryState::Discharging, &config), config.icons.battery_discharging[2]);
        assert_eq!(battery_icon(10, &BatteryState::Discharging, &config), config.icons.battery_discharging[1]);
        assert_eq!(battery_icon(3, &BatteryState::Discharging, &config), config.icons.battery_discharging[0]);
    }

    #[test]
    fn battery_icon_charging_uses_charging_array() {
        let config = Config::default();
        assert_eq!(battery_icon(50, &BatteryState::Charging, &config), config.icons.battery_charging[2]);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib segments`
Expected: All 8 tests PASS.

- [ ] **Step 4: Add `mod segments;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/segments.rs src/main.rs
git commit -m "feat: add segments module with mode, session, battery, wifi, time segments"
```

---

## Task 11: Render Module

**Files:**
- Create: `src/render.rs`
- Modify: `src/main.rs` (add `mod render;`)

Depends on: `color.rs`, `config.rs`, `state.rs`, `tabs.rs`, `layout.rs`, `segments.rs`, `icons.rs`

This is the integration point — assembles the final status bar from all components.

- [ ] **Step 1: Write render_bar function**

```rust
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::InputMode;

use crate::color::{self, Color};
use crate::config::Config;
use crate::icons;
use crate::layout::{self, compute_tab_layout};
use crate::segments::{self, Segment};
use crate::state::AppState;
use crate::tabs;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";

const TAB_BG: Color = Color { r: 40, g: 40, b: 40 };
const TAB_FG: Color = Color { r: 180, g: 180, b: 180 };
const ACTIVE_TAB_BG: Color = Color { r: 80, g: 80, b: 80 };
const ACTIVE_TAB_FG: Color = Color { r: 255, g: 255, b: 255 };
const BAR_BG: Color = Color { r: 25, g: 25, b: 25 };

pub fn render_bar(state: &AppState, config: &Config, cols: usize) -> String {
    let mut output = String::with_capacity(cols * 4);

    let right_side = build_right_side(state, config, cols);
    let right_width = right_side.width;

    let available_for_tabs = cols.saturating_sub(right_width);

    let left_side = build_left_side(state, config, available_for_tabs);
    let left_width = left_side.width;

    output.push_str(&left_side.text);

    let gap = cols.saturating_sub(left_width + right_width);
    if gap > 0 {
        output.push_str(&BAR_BG.to_ansi_bg());
        for _ in 0..gap {
            output.push(' ');
        }
    }

    output.push_str(&right_side.text);
    output.push_str(ANSI_RESET);

    output
}

struct RenderedSide {
    text: String,
    width: usize,
}

fn build_left_side(state: &AppState, config: &Config, available: usize) -> RenderedSide {
    if state.tabs.is_empty() {
        return RenderedSide { text: String::new(), width: 0 };
    }

    if config.tab_hide_single && state.tabs.len() == 1 {
        return RenderedSide { text: String::new(), width: 0 };
    }

    let is_mode_active = state.mode != InputMode::Normal;

    if is_mode_active {
        return build_mode_active_tab(state, config, available);
    }

    build_normal_tabs(state, config, available)
}

fn build_mode_active_tab(state: &AppState, config: &Config, available: usize) -> RenderedSide {
    let active_idx = match state.active_tab_index() {
        Some(i) => i,
        None => return RenderedSide { text: String::new(), width: 0 },
    };
    let tab = &state.tabs[active_idx];

    let mode_color = config.mode_color(state.mode).unwrap_or(ACTIVE_TAB_BG);

    let mut fg = mode_color.darken(0.8);
    if color::contrast_ratio(mode_color, fg) < 3.8 {
        fg = mode_color.lighten(0.6);
    }

    let title = tabs::compose_tab_title(tab.position, &tab.name, state, config);
    let rendered = tabs::render_tab_title(&title, available.min(config.tab_max_width), config.tab_truncation_point);

    let body = format!(" {} ", rendered);
    let width = UnicodeWidthStr::width(body.as_str()) + 1;

    let text = format!(
        "{}{}{}{}{}{}{}",
        ANSI_BOLD,
        mode_color.to_ansi_bg(),
        fg.to_ansi_fg(),
        body,
        BAR_BG.to_ansi_bg(),
        mode_color.to_ansi_fg(),
        icons::PLE_RIGHT_HALF_CIRCLE_THICK,
    );

    RenderedSide { text, width }
}

fn build_normal_tabs(state: &AppState, config: &Config, available: usize) -> RenderedSide {
    let active_idx = state.active_tab_index().unwrap_or(0);

    let titles: Vec<tabs::TabTitle> = state.tabs.iter().map(|tab| {
        tabs::compose_tab_title(tab.position, &tab.name, state, config)
    }).collect();

    let natural_widths: Vec<usize> = titles.iter().map(|t| {
        let rendered = tabs::render_tab_title(t, config.tab_max_width, config.tab_truncation_point);
        UnicodeWidthStr::width(rendered.as_str()) + 3
    }).collect();

    let tab_layout = compute_tab_layout(
        state.tabs.len(),
        active_idx,
        &natural_widths,
        available,
        config,
    );

    let mut text = String::new();
    let mut total_width = 0;

    let scroll = &tab_layout.scroll;

    if let Some(ref s) = scroll {
        if s.has_left {
            text.push_str(&format!(
                "{}{}{}",
                BAR_BG.to_ansi_bg(),
                TAB_FG.to_ansi_fg(),
                icons::FA_CHEVRON_LEFT,
            ));
            total_width += 1;
        }
    }

    let visible_range = scroll.as_ref().map(|s| s.left..=s.right);

    for (i, tab) in state.tabs.iter().enumerate() {
        if let Some(ref range) = visible_range {
            if !range.contains(&i) {
                continue;
            }
        }

        let allotted = tab_layout.widths[i];
        if allotted == 0 {
            continue;
        }

        let (bg, fg) = if tab.active {
            (ACTIVE_TAB_BG, ACTIVE_TAB_FG)
        } else {
            (TAB_BG, TAB_FG)
        };

        let rendered = tabs::render_tab_title(&titles[i], allotted.saturating_sub(3), config.tab_truncation_point);
        let body = format!(" {} ", rendered);
        let cell_width = UnicodeWidthStr::width(body.as_str());

        text.push_str(&format!(
            "{}{}{}{}",
            ANSI_BOLD,
            bg.to_ansi_bg(),
            fg.to_ansi_fg(),
            body,
        ));

        text.push_str(&format!(
            "{}{}{}",
            BAR_BG.to_ansi_bg(),
            bg.to_ansi_fg(),
            icons::LEFT_HALF_BLOCK,
        ));

        total_width += cell_width + 1;
    }

    if let Some(ref s) = scroll {
        if s.has_right {
            text.push_str(&format!(
                "{}{}{}",
                BAR_BG.to_ansi_bg(),
                TAB_FG.to_ansi_fg(),
                icons::FA_CHEVRON_RIGHT,
            ));
            total_width += 1;
        }
    }

    RenderedSide { text, width: total_width }
}

fn build_right_side(state: &AppState, config: &Config, cols: usize) -> RenderedSide {
    let show_system = cols >= config.fullscreen_min_cols;
    let mode = state.mode;

    let mode_color = config.mode_color(mode).unwrap_or(BAR_BG);
    let base_color = if mode == InputMode::Normal { BAR_BG } else { mode_color };

    let mut active_segments: Vec<Option<Segment>> = Vec::new();

    let mode_seg = segments::mode_segment(mode, Color::new(0, 0, 0), false, config);
    active_segments.push(mode_seg);

    let session_seg = segments::session_segment(&state.session_name, Color::new(0, 0, 0), false, config);
    active_segments.push(session_seg);

    if show_system {
        active_segments.push(Some(segments::battery_segment(state, Color::new(0, 0, 0), false, config)));
        active_segments.push(Some(segments::wifi_segment(state, Color::new(0, 0, 0), false, config)));
        active_segments.push(Some(segments::time_segment(Color::new(0, 0, 0), true, config)));
    }

    let present: Vec<&Segment> = active_segments.iter().filter_map(|s| s.as_ref()).collect();
    let segment_count = present.len();

    if segment_count == 0 {
        return RenderedSide { text: String::new(), width: 0 };
    }

    let gradient_stops = color::gradient(base_color, BAR_BG, segment_count + 1);

    let mut rebuilt_segments: Vec<Segment> = Vec::new();
    let mut seg_idx = 0;
    for opt in &active_segments {
        if opt.is_some() {
            let bg = gradient_stops[segment_count - seg_idx - 1];
            let is_last = seg_idx == segment_count - 1;

            let seg = match seg_idx {
                _ if seg_idx == 0 && mode != InputMode::Normal => {
                    segments::mode_segment(mode, bg, is_last && segment_count == 1, config).unwrap()
                }
                _ => {
                    if let Some(ref original) = opt {
                        rebuild_segment_with_bg(original, bg, is_last)
                    } else {
                        continue;
                    }
                }
            };
            rebuilt_segments.push(seg);
            seg_idx += 1;
        }
    }

    let mut text = String::new();
    let mut total_width = 0;

    let bg_fade = gradient_stops[segment_count];

    for (i, seg) in rebuilt_segments.iter().enumerate() {
        let left_bg = if i == 0 { bg_fade } else { rebuilt_segments[i - 1].bg };
        text.push_str(&segments::divider(left_bg, seg.bg));
        total_width += segments::divider_width();
        text.push_str(&seg.text);
        total_width += seg.width;
    }

    RenderedSide { text, width: total_width }
}

fn rebuild_segment_with_bg(original: &Segment, bg: Color, is_last: bool) -> Segment {
    let plain_text = extract_plain_text(&original.text);
    segments::format_segment(bg, &plain_text, is_last)
}

fn extract_plain_text(ansi_text: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    for c in ansi_text.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        result.push(c);
    }
    result.trim().to_string()
}
```

- [ ] **Step 2: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_text_strips_ansi() {
        let s = "\x1b[48;2;100;100;100m\x1b[38;2;20;20;20m hello \x1b[0m";
        assert_eq!(extract_plain_text(s), "hello");
    }

    #[test]
    fn empty_state_renders() {
        let state = AppState::default();
        let config = Config::default();
        let output = render_bar(&state, &config, 80);
        assert!(!output.is_empty());
        assert!(output.contains(ANSI_RESET));
    }

    #[test]
    fn right_side_empty_in_normal_mode_main_session_narrow() {
        let state = AppState {
            mode: InputMode::Normal,
            session_name: "main".to_string(),
            cols: 80,
            ..AppState::default()
        };
        let config = Config::default(); // fullscreen_min_cols = 120
        let right = build_right_side(&state, &config, 80);
        assert_eq!(right.width, 0);
    }

    #[test]
    fn right_side_has_mode_in_locked() {
        let state = AppState {
            mode: InputMode::Locked,
            session_name: "main".to_string(),
            cols: 80,
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 80);
        assert!(right.width > 0);
    }

    #[test]
    fn right_side_has_system_segments_when_wide() {
        let state = AppState {
            mode: InputMode::Normal,
            session_name: "main".to_string(),
            cols: 200,
            ..AppState::default()
        };
        let config = Config::default();
        let right = build_right_side(&state, &config, 200);
        assert!(right.width > 0); // at least battery + time
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib render`
Expected: All 5 tests PASS.

- [ ] **Step 4: Add `mod render;` to main.rs, verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/main.rs
git commit -m "feat: add render module for final bar assembly with tabs and segments"
```

---

## Task 12: Main Module — Wire Everything Together

**Files:**
- Modify: `src/main.rs`

This task replaces the skeleton `main.rs` with the full event-driven plugin.

- [ ] **Step 1: Rewrite main.rs with full plugin implementation**

```rust
mod color;
mod config;
mod icons;
mod layout;
mod render;
mod segments;
mod state;
mod system;
mod tabs;
mod truncation;

use std::collections::BTreeMap;
use zellij_tile::prelude::*;

use config::Config;
use state::AppState;

register_plugin!(State);

const TIMER_INTERVAL: f64 = 10.0;

#[derive(Default)]
struct State {
    app: AppState,
    config: Config,
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        set_selectable(false);

        self.config = Config::from_map(configuration);

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::RunCommands,
        ]);

        subscribe(&[
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::SessionUpdate,
            EventType::PaneUpdate,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        if !self.app.got_permissions {
            if let Event::PermissionRequestResult(PermissionStatus::Granted) = event {
                self.app.got_permissions = true;
                set_timeout(1.0);
                let pending: Vec<Event> = self.app.pending_events.drain(..).collect();
                for ev in pending {
                    self.process_event(ev);
                }
                self.app.dirty = true;
            } else {
                self.app.pending_events.push(event);
            }
            return self.app.dirty;
        }

        self.process_event(event);
        self.app.dirty
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        self.app.cols = cols;

        if !self.app.dirty && !self.app.last_output.is_empty() {
            print!("{}", self.app.last_output);
            return;
        }

        let output = render::render_bar(&self.app, &self.config, cols);
        print!("{}", output);
        self.app.last_output = output;
        self.app.dirty = false;
    }
}

impl State {
    fn process_event(&mut self, event: Event) {
        match event {
            Event::ModeUpdate(mode_info) => {
                let new_mode = mode_info.mode;
                if new_mode != self.app.mode {
                    self.app.mode = new_mode;
                    self.app.dirty = true;
                }
                if let Some(name) = mode_info.session_name {
                    if name != self.app.session_name {
                        self.app.session_name = name;
                        self.app.dirty = true;
                    }
                }
            }
            Event::TabUpdate(tabs) => {
                self.app.tabs = tabs;
                self.app.dirty = true;
            }
            Event::SessionUpdate(sessions, _) => {
                if let Some(session) = sessions.iter().find(|s| s.is_current_session) {
                    if session.name != self.app.session_name {
                        self.app.session_name = session.name.clone();
                        self.app.dirty = true;
                    }
                }
            }
            Event::PaneUpdate(manifest) => {
                self.app.panes = manifest.panes;
                self.app.dirty = true;
            }
            Event::Timer(_) => {
                if self.app.cols >= self.config.fullscreen_min_cols {
                    system::maybe_refresh_battery(&mut self.app);
                    system::maybe_refresh_wifi(&mut self.app);
                }
                set_timeout(TIMER_INTERVAL);
            }
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                system::handle_command_result(exit_code, &stdout, &stderr, &context, &mut self.app);
                self.app.dirty = true;
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 2: Verify WASM build**

Run: `cargo build --target wasm32-wasip1`
Expected: Compiles with no errors.

- [ ] **Step 3: Verify all tests still pass**

Run: `cargo test --lib`
Expected: All tests across all modules PASS.

- [ ] **Step 4: Build release WASM**

Run: `cargo build --target wasm32-wasip1 --release`
Expected: Produces `target/wasm32-wasip1/release/zj-statusbar.wasm`.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire up full plugin with event dispatch, permissions, and render loop"
```

---

## Task 13: Integration Test in Zellij

**Files:**
- Create: `test-layout.kdl` (temporary, for manual testing)

This is manual integration testing in a real Zellij session.

- [ ] **Step 1: Create a test layout file**

```kdl
layout {
    pane size=1 borderless=true {
        plugin location="file:target/wasm32-wasip1/release/zj-statusbar.wasm" {
            fullscreen_min_cols 80
        }
    }
    pane
}
```

- [ ] **Step 2: Build and run**

Run:
```bash
cargo build --target wasm32-wasip1 --release
zellij --layout test-layout.kdl
```

- [ ] **Step 3: Manual test checklist**

Verify each of these in the running Zellij session:

1. Status bar appears at the top with tab(s) on the left
2. Tab shows 1-based index + icon + title
3. Open multiple tabs — they all appear in the bar
4. Open many tabs (10+) — equalization kicks in, then scrolling with chevrons
5. Switch to a non-normal mode (e.g., press the pane mode key) — only the active tab shows, mode segment appears on the right with color
6. Return to normal mode — all tabs reappear, mode segment disappears
7. Rename a session to something other than "main" — session segment appears
8. If terminal is wide enough (>=80 per test config), battery/wifi/time segments appear
9. Resize terminal narrower — system segments disappear

- [ ] **Step 4: Clean up test layout file and commit any fixes**

```bash
rm test-layout.kdl
git add -A
git commit -m "fix: integration test fixes from manual Zellij testing"
```

This commit only happens if fixes were needed. If everything worked, skip it.

---

## Spec Coverage Check

| Spec Requirement | Task |
|---|---|
| Tab title composition (§5.3) | Task 7 |
| Truncation algorithm (§5.3.1) | Task 4 |
| Tab equalization (§5.4.4) | Task 8 |
| Tab scrolling (§5.4.5) | Task 8 |
| Tab rendering with mode styling (§5.5) | Task 11 |
| Process icon map (§5.6) | Task 3 |
| Mode segment for all 13 modes (§6.4.1) | Task 10 |
| Session segment elision (§6.4.1 workspace) | Task 10 |
| Battery with correct charging icons (§6.4.3) | Task 9, 10 |
| WiFi detection cross-platform (§6.4.4) | Task 9 |
| Date/time segment (§6.4.5) | Task 10 |
| Color gradient 5-stop Oklab (§6.2, §7) | Task 2 |
| WCAG contrast ratio >= 3.8 (§7) | Task 2, 10 |
| Segment elision (§6.3.1) | Task 10, 11 |
| Configuration from KDL BTreeMap (§Config) | Task 5 |
| Debounce via dirty flag | Task 6, 12 |
| Cold-start hydration from TabUpdate | Task 12 |
| WASM compilation | Task 1, 12 |
| Async system queries via run_command | Task 9 |
