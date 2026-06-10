//! Footer line: navigation affordances.
//!
//! Segments:
//!   * **close** — the key bound to `SwitchToMode(base_mode)` (returns to the
//!     resting mode, which dismisses the panel), discovered from the keymap.
//!     `Esc` is preferred when several keys close (e.g. both `Esc` and `Enter`
//!     are bound to it).
//!   * **back** — the `wk_go_back` key, shown only when the `back_key` config
//!     is set *and* there's a previous mode to return to (a non-empty synthetic
//!     mode trail). Its pipe name is stripped from the keymap, so it can't be
//!     auto-discovered.
//!   * **hide** — the `wk_toggle_pane` key, shown only when the `toggle_key`
//!     config is set (its pipe name is stripped from the keymap, so it can't be
//!     auto-discovered).
//!   * **scroll** — the paging keys, shown only when there's more than one
//!     page, with the current page indicator. These come from the
//!     `prev_page_key` / `next_page_key` config: the host strips pipe names
//!     from the keymap, so the paging pipes can't be auto-discovered.

use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::{BareKey, InputMode, KeyWithModifier};

use crate::whichkey::config::Config;
use crate::whichkey::labels::format_key_compact;
use crate::whichkey::theme::Theme;

/// Spaces between footer segments.
const GAP: usize = 2;

/// A laid-out footer: a left run of colored navigation hints and an optional
/// page counter the renderer pins to the right edge. Strings carry SGR colors;
/// the `_w` fields are the **visible** widths (used for sizing + right-align).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    /// Close + scroll hints, already colored (key glyphs bright, labels grey).
    pub left: String,
    /// Visible width of `left`.
    pub left_w: usize,
    /// `"N/M"` page counter, colored as footer chrome — right-aligned by the renderer.
    pub counter: Option<String>,
    /// Visible width of the counter.
    pub counter_w: usize,
}

impl Footer {
    /// Minimum interior width to fit both runs without overlap.
    pub fn width(&self) -> usize {
        if self.counter_w == 0 {
            self.left_w
        } else if self.left_w == 0 {
            self.counter_w
        } else {
            self.left_w + GAP + self.counter_w
        }
    }
}

/// Build the footer, or `None` if there's nothing to show. Binding glyphs use
/// the bright key color; the label words and page counter use the footer grey.
pub fn build(
    keybinds: &[(KeyWithModifier, Vec<Action>)],
    base_mode: InputMode,
    page: usize,
    page_count: usize,
    can_go_back: bool,
    config: &Config,
    theme: &Theme,
) -> Option<Footer> {
    // (binding glyphs, trailing word) pairs for the left run.
    let mut segs: Vec<(String, &str)> = Vec::new();

    if let Some(close) = find_close_key(keybinds, base_mode) {
        segs.push((format_key_compact(&close), "close"));
    }

    // The back key can't be auto-discovered (its pipe name is stripped from the
    // keymap); surface it only when configured and there's a mode to return to.
    if can_go_back {
        if let Some(back) = &config.back_key {
            segs.push((back.clone(), "back"));
        }
    }

    // The toggle key can't be auto-discovered (its pipe name is stripped from
    // the keymap), so it's surfaced only when configured. Use the spaced label
    // form here so multi-glyph chords read like the status-bar hint.
    if let Some(toggle) = &config.toggle_key_label {
        segs.push((toggle.clone(), "hide"));
    }

    if page_count > 1 {
        let prev = config
            .prev_page_key
            .clone()
            .or_else(|| find_pipe_key(keybinds, "wk_prev_page").map(|k| format_key_compact(&k)));
        let next = config
            .next_page_key
            .clone()
            .or_else(|| find_pipe_key(keybinds, "wk_next_page").map(|k| format_key_compact(&k)));
        if let (Some(p), Some(n)) = (prev, next) {
            segs.push((format!("{p}/{n}"), "scroll"));
        }
    }

    let (key, footer, reset) = (&theme.key, &theme.footer, &theme.reset);
    let mut left = String::new();
    let mut left_w = 0usize;
    for (i, (glyphs, word)) in segs.iter().enumerate() {
        if i > 0 {
            left.push_str(&" ".repeat(GAP));
            left_w += GAP;
        }
        left.push_str(&format!("{key}{glyphs}{reset}{footer} {word}{reset}"));
        left_w += glyphs.chars().count() + 1 + word.chars().count();
    }

    let (counter, counter_w) = if page_count > 1 {
        let text = format!("{}/{}", page + 1, page_count);
        let w = text.chars().count();
        (Some(format!("{footer}{text}{reset}")), w)
    } else {
        (None, 0)
    };

    if left.is_empty() && counter.is_none() {
        return None;
    }
    Some(Footer {
        left,
        left_w,
        counter,
        counter_w,
    })
}

/// The key that returns to the base (resting) mode. When several keys do (the
/// common `Esc` + `Enter` pairing), `Esc` wins; otherwise the first one.
fn find_close_key(
    keybinds: &[(KeyWithModifier, Vec<Action>)],
    base_mode: InputMode,
) -> Option<KeyWithModifier> {
    let is_close = |actions: &[Action]| {
        actions.len() == 1
            && matches!(
                &actions[0],
                Action::SwitchToMode { input_mode }
                    | Action::SwitchModeForAllClients { input_mode }
                    if *input_mode == base_mode
            )
    };
    keybinds
        .iter()
        .find(|(key, actions)| is_close(actions) && key.bare_key == BareKey::Esc)
        .or_else(|| keybinds.iter().find(|(_, actions)| is_close(actions)))
        .map(|(key, _)| key.clone())
}

/// First key that pipes to us with the given message `name` (a `MessagePlugin`
/// binding compiles to `Action::KeybindPipe { name, .. }`).
fn find_pipe_key(
    keybinds: &[(KeyWithModifier, Vec<Action>)],
    name: &str,
) -> Option<KeyWithModifier> {
    keybinds
        .iter()
        .find(|(_, actions)| {
            actions.iter().any(|a| {
                matches!(
                    a,
                    Action::KeybindPipe { name: Some(n), .. } if n == name
                )
            })
        })
        .map(|(key, _)| key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_tile::prelude::BareKey;

    fn esc() -> KeyWithModifier {
        KeyWithModifier::new(BareKey::Esc)
    }

    #[test]
    fn close_segment_from_switch_to_base() {
        let kb = vec![(
            esc(),
            vec![Action::SwitchToMode {
                input_mode: InputMode::Normal,
            }],
        )];
        let theme = Theme::default();
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &Config::default(),
            &theme,
        )
        .unwrap();
        assert!(f.left.contains("close"));
        assert!(f.left.contains('\u{F12B7}')); // Esc glyph 󱊷
        assert!(f.left.contains(&format!(
            "{}{}{}{} close{}",
            theme.key, '\u{F12B7}', theme.reset, theme.footer, theme.reset
        )));
        assert!(f.counter.is_none()); // single page → no counter
    }

    #[test]
    fn close_prefers_esc_over_enter() {
        // Both Enter (first) and Esc return to base; Esc should win.
        let kb = vec![
            (
                KeyWithModifier::new(BareKey::Enter),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Normal,
                }],
            ),
            (
                esc(),
                vec![Action::SwitchToMode {
                    input_mode: InputMode::Normal,
                }],
            ),
        ];
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &Config::default(),
            &Theme::default(),
        )
        .unwrap();
        assert!(f.left.contains('\u{F12B7}')); // Esc glyph 󱊷
        assert!(!f.left.contains('\u{F0311}')); // not the Enter glyph 󰌑
    }

    #[test]
    fn hide_segment_only_when_toggle_key_set() {
        let kb = vec![(
            esc(),
            vec![Action::SwitchToMode {
                input_mode: InputMode::Normal,
            }],
        )];
        // Unset → no hide affordance.
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &Config::default(),
            &Theme::default(),
        )
        .unwrap();
        assert!(!f.left.contains("hide"));
        // Set → `<glyph> hide` shows.
        let config = Config {
            toggle_key: Some("\u{F0635}.".into()),        // 󰘵.
            toggle_key_label: Some("\u{F0635} .".into()), // 󰘵 .
            ..Config::default()
        };
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &config,
            &Theme::default(),
        )
        .unwrap();
        assert!(f.left.contains("hide"));
        assert!(f.left.contains("\u{F0635} ."));
        assert!(!f.left.contains("\u{F0635}."));
    }

    #[test]
    fn back_segment_only_when_key_set_and_can_go_back() {
        let kb = vec![(
            esc(),
            vec![Action::SwitchToMode {
                input_mode: InputMode::Normal,
            }],
        )];
        let config = Config {
            back_key: Some("\u{F006E}".into()), // 󰁮
            ..Config::default()
        };
        // Key set but nowhere to go back → no affordance.
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &config,
            &Theme::default(),
        )
        .unwrap();
        assert!(!f.left.contains("back"));
        // Key set and a previous mode exists → `<glyph> back` shows.
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            true,
            &config,
            &Theme::default(),
        )
        .unwrap();
        assert!(f.left.contains("back"));
        assert!(f.left.contains('\u{F006E}'));
        // Can go back but no key configured → still nothing.
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            true,
            &Config::default(),
            &Theme::default(),
        )
        .unwrap();
        assert!(!f.left.contains("back"));
    }

    #[test]
    fn no_scroll_segment_for_single_page() {
        let kb = vec![(
            esc(),
            vec![Action::SwitchToMode {
                input_mode: InputMode::Normal,
            }],
        )];
        let f = build(
            &kb,
            InputMode::Normal,
            0,
            1,
            false,
            &Config::default(),
            &Theme::default(),
        )
        .unwrap();
        assert!(!f.left.contains("scroll"));
    }

    #[test]
    fn scroll_segment_and_counter_when_paging() {
        let config = Config {
            prev_page_key: Some("\u{F0634}U".into()),
            next_page_key: Some("\u{F0634}D".into()),
            ..Config::default()
        };
        let f = build(
            &[],
            InputMode::Normal,
            1,
            3,
            false,
            &config,
            &Theme::default(),
        )
        .unwrap();
        assert!(f.left.contains("scroll"));
        assert!(f.left.contains("\u{F0634}U/\u{F0634}D"));
        // The page counter is a separate, right-aligned run.
        assert_eq!(f.counter_w, 3); // "2/3"
        assert!(f.counter.as_deref().unwrap().contains("2/3"));
    }

    #[test]
    fn empty_when_nothing_to_show() {
        assert!(build(
            &[],
            InputMode::Normal,
            0,
            1,
            false,
            &Config::default(),
            &Theme::default()
        )
        .is_none());
    }
}
