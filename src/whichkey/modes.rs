//! `InputMode` name handling: parsing config tokens, expanding mode *groups*,
//! and human-facing display names.

use zellij_tile::prelude::InputMode;

/// Mode *groups*: a canonical alias name that stands for several real modes.
///
/// Today the only group is the two-phase native search: `EnterSearch` (you are
/// typing the term) followed by `Search` (you are navigating matches). When
/// scoping labels/groups to modes (e.g. `mode="search"`), the friendly token
/// `search` expands to both phases; the individual phase names (`search` /
/// `entersearch`) remain usable as literals for fine control.
///
/// Note `"search"` is intentionally both this group's canonical name and the
/// name of the `Search` mode: as a group token it always means "the whole
/// search activity" (both phases).
pub const MODE_GROUPS: &[(&str, &[InputMode])] =
    &[("search", &[InputMode::Search, InputMode::EnterSearch])];

/// If `name` (case-insensitive) is a group's canonical alias, return its
/// members. Otherwise `None` (the caller treats it as a literal mode name).
pub fn group_members(name: &str) -> Option<&'static [InputMode]> {
    let lc = name.to_ascii_lowercase();
    MODE_GROUPS
        .iter()
        .find(|(canonical, _)| *canonical == lc)
        .map(|(_, members)| *members)
}

/// Parse a single mode name (case-insensitive). `lock` is accepted as an alias
/// for `locked`. Returns `None` for unknown names.
pub fn str_to_mode(name: &str) -> Option<InputMode> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Some(InputMode::Normal),
        "locked" | "lock" => Some(InputMode::Locked),
        "resize" => Some(InputMode::Resize),
        "pane" => Some(InputMode::Pane),
        "tab" => Some(InputMode::Tab),
        "scroll" => Some(InputMode::Scroll),
        "entersearch" | "enter_search" => Some(InputMode::EnterSearch),
        "search" => Some(InputMode::Search),
        "renametab" | "rename_tab" => Some(InputMode::RenameTab),
        "renamepane" | "rename_pane" => Some(InputMode::RenamePane),
        "session" => Some(InputMode::Session),
        "move" => Some(InputMode::Move),
        "prompt" => Some(InputMode::Prompt),
        "tmux" => Some(InputMode::Tmux),
        _ => None,
    }
}

/// Stable, machine-readable mode name. The unified `shared_state` module owns
/// the serialised form now; kept here for parity with the other mode helpers.
#[allow(dead_code)]
pub fn mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Normal => "Normal",
        InputMode::Locked => "Locked",
        InputMode::Resize => "Resize",
        InputMode::Pane => "Pane",
        InputMode::Tab => "Tab",
        InputMode::Scroll => "Scroll",
        InputMode::EnterSearch => "EnterSearch",
        InputMode::Search => "Search",
        InputMode::RenameTab => "RenameTab",
        InputMode::RenamePane => "RenamePane",
        InputMode::Session => "Session",
        InputMode::Move => "Move",
        InputMode::Prompt => "Prompt",
        InputMode::Tmux => "Tmux",
    }
}

/// NerdFont glyph for a mode, shown as the panel's frame title. Mirrors the
/// `zj-statusbar` icon set (`icons.rs` / `MODE_STYLES`) so both surfaces agree.
/// `Normal` has no dedicated glyph (the panel is never shown in it) and falls
/// back to the command glyph.
pub fn mode_icon(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Locked => "\u{F033E}",                         // md_lock
        InputMode::Resize => "\u{F0A68}",                         // md_resize
        InputMode::Pane => "\u{EBC8}",                            // md_view_column
        InputMode::Tab => "\u{F04E9}",                            // md_tab
        InputMode::Scroll => "\u{F0A64}",                         // md_unfold_more_horizontal
        InputMode::EnterSearch | InputMode::Search => "\u{F002}", // fa_search
        InputMode::RenameTab | InputMode::RenamePane => "\u{F0455}", // md_rename
        InputMode::Session => "\u{F0640}",                        // md_collage
        InputMode::Move => "\u{F0655}",                           // md_cursor_move
        InputMode::Prompt => "\u{EA85}",                          // md_console
        InputMode::Tmux | InputMode::Normal => "\u{F0633}",       // md_apple_keyboard_command
    }
}

/// Default symbol color for a mode, as a hex string. Mirrors `zj-statusbar`'s
/// `MODE_STYLES` palette so both surfaces tint a given mode the same way. Used
/// when the user hasn't set a per-mode `color` in the `modes` config block.
pub fn mode_color(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Locked => "#FF6666",
        InputMode::Resize => "#FFCC66",
        InputMode::Pane => "#66CCFF",
        InputMode::Tab => "#CC99FF",
        InputMode::Scroll => "#99FFCC",
        InputMode::EnterSearch | InputMode::Search => "#FFFF66",
        InputMode::RenameTab | InputMode::RenamePane => "#FFCC99",
        InputMode::Session => "#FF99CC",
        InputMode::Move => "#66FFCC",
        InputMode::Prompt => "#99CCFF",
        InputMode::Tmux | InputMode::Normal => "#CC66FF",
    }
}

/// Human-facing label for a mode.
pub fn mode_display_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Normal => "Normal",
        InputMode::Locked => "Locked",
        InputMode::Resize => "Resize",
        InputMode::Pane => "Pane",
        InputMode::Tab => "Tab",
        InputMode::Scroll => "Scroll",
        // Both search phases read as "Search" to the user.
        InputMode::EnterSearch | InputMode::Search => "Search",
        InputMode::RenameTab => "Rename Tab",
        InputMode::RenamePane => "Rename Pane",
        InputMode::Session => "Session",
        InputMode::Move => "Move",
        InputMode::Prompt => "Prompt",
        InputMode::Tmux => "Tmux",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_alias_for_locked() {
        assert_eq!(str_to_mode("lock"), Some(InputMode::Locked));
        assert_eq!(str_to_mode("LOCKED"), Some(InputMode::Locked));
    }

    #[test]
    fn unknown_mode_is_none() {
        assert_eq!(str_to_mode("bogus"), None);
    }

    #[test]
    fn search_is_a_group_entersearch_is_not() {
        assert_eq!(
            group_members("search"),
            Some(&[InputMode::Search, InputMode::EnterSearch][..])
        );
        assert!(group_members("entersearch").is_none());
        assert!(group_members("pane").is_none());
    }
}
