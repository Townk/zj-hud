// Powerline / UI glyphs
pub const PLE_LOWER_RIGHT_TRIANGLE: &str = "\u{E0BA}";
pub const LEFT_HALF_BLOCK: &str = "\u{258C}";

// Scroll-indicator arrows (styled as non-active tabs: " ◂▌" / " ▸▌")
pub const SCROLL_LEFT_ARROW: &str = "\u{25C2}"; // ◂ BLACK LEFT-POINTING SMALL TRIANGLE
pub const SCROLL_RIGHT_ARROW: &str = "\u{25B8}"; // ▸ BLACK RIGHT-POINTING SMALL TRIANGLE

// Tab icons
pub const TAB_DIR: &str = "\u{F0770}"; // md_folder_open
pub const TAB_HOME: &str = "\u{F02DC}"; // md_home
pub const TAB_PROCESS: &str = "\u{F070E}"; // md_run
pub const TAB_ICON: &str = "\u{F04E9}"; // md_tab
pub const ZOOM_ICON: &str = "\u{F05AF}"; // md_magnify_plus_outline
pub const INPUT_SYNC_ICON: &str = "\u{F43C}"; // input sync (synced panes)

// Mode icons
pub const MODE_LOCKED: &str = "\u{F033E}"; // md_lock
pub const MODE_RESIZE: &str = "\u{F0A68}"; // md_resize
pub const MODE_PANE: &str = "\u{EBC8}"; // md_view_column
pub const MODE_TAB: &str = "\u{F04E9}"; // md_tab
pub const MODE_SCROLL: &str = "\u{F0A64}"; // md_unfold_more_horizontal
pub const MODE_SEARCH: &str = "\u{F002}"; // fa_search
pub const MODE_RENAME: &str = "\u{F0455}"; // md_rename
pub const MODE_SESSION: &str = "\u{F0640}"; // md_collage
pub const MODE_MOVE: &str = "\u{F0655}"; // md_cursor_move
pub const MODE_PROMPT: &str = "\u{EA85}"; // md_console
pub const MODE_TMUX: &str = "\u{F0633}"; // md_apple_keyboard_command

// Date/time segment icon (default for the built-in `date_time` segment).
pub const CALENDAR: &str = "\u{F00F0}"; // md_calendar_clock

/// Detects if a process name is a shell.
pub fn is_shell(name: &str) -> bool {
    matches!(name, "bash" | "zsh" | "fish" | "nu")
}

/// Returns a NerdFont icon for a process name, if one is defined.
pub fn process_icon(name: &str) -> Option<&'static str> {
    match name {
        "agent" | "cursor-agent" => Some("\u{10FB00}"),
        "bash" => Some("\u{E795}"),
        "brew" => Some("\u{F007B}"),
        "cargo" => Some("\u{E7A8}"),
        "claude" => Some("\u{10E861}"),
        "curl" => Some("\u{E241}"),
        "docker" | "docker-compose" => Some("\u{E77D}"),
        "fish" => Some("\u{F0BA5}"),
        "gh" => Some("\u{E709}"),
        "git" => Some("\u{E65D}"),
        "go" => Some("\u{E627}"),
        "htop" | "btop" => Some("\u{F0531}"),
        "kubectl" | "kuberlr" | "lazydocker" => Some("\u{E77D}"),
        "lazygit" => Some("\u{E702}"),
        "lua" => Some("\u{E620}"),
        "make" => Some("\u{E673}"),
        "node" => Some("\u{E24F}"),
        "nvim" => Some("\u{E6AE}"),
        "pacman" | "paru" => Some("\u{F0BAF}"),
        "pi" | "pi-coding-agent" => Some("\u{10FB02}"),
        "psql" => Some("\u{E76E}"),
        "ruby" => Some("\u{E739}"),
        "sudo" => Some("\u{F292}"),
        "vim" => Some("\u{E7C5}"),
        "wget" => Some("\u{E260}"),
        "zsh" => Some("\u{E795}"),
        _ => None,
    }
}

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
}
