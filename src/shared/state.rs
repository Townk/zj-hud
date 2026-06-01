//! Shared session state for coordinating the per-tab statusbar/which-key
//! instances and the floating search pane.
//!
//! All roles of the plugin (Bar, Search, WhichKey) read and write a single
//! session-scoped JSON file plus a broadcast pipe. To keep concurrent writers
//! from clobbering each other's fields, each writer reads the latest state,
//! mutates only the fields it owns, and bumps the monotonic `generation` (the
//! highest generation wins on apply). Field ownership:
//!
//! - `mode` / `base_mode` / `backstack` / `palette` / `which_key_config` /
//!   `search_config` — the active **Bar** instance. `palette` lets the Bar be
//!   the single source of truth for the mode glyph/color/label set;
//!   `which_key_config` and `search_config` carry the raw `which_key { … }` /
//!   `search { … }` blocks (authored once on the bar) to the panel and the
//!   search dialog, so those roles need no geometry config of their own.
//! - `search_active` — the **Search** role.
//! - `suppressed` / `page` — the **WhichKey** role.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zellij_tile::prelude::InputMode;

/// Bumped whenever the on-disk schema changes incompatibly. Old files that fail
/// to parse fall back to `Default` in the readers, and the per-field
/// `#[serde(default)]` lets a partial (older) file still deserialize.
pub const SCHEMA_VERSION: u32 = 2;

/// Broadcast pipe name used by every role to push a fresh `SharedState` to all
/// instances of the plugin URL. The single channel (rather than per-purpose
/// pipes) keeps the cross-instance contract to one type.
pub const SYNC_PIPE: &str = "__zj_statusbar_sync_state";

/// A single mode's display style, published by the Bar so the WhichKey panel
/// can render glyphs/colors/labels identical to the bar without a duplicate
/// `modes` config block. `color` is a ready-to-emit ANSI SGR foreground
/// sequence (the Bar resolves its `Color` to ANSI before publishing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModePalette {
    pub icon: String,
    pub color: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedState {
    pub schema_version: u32,
    pub generation: u64,
    pub writer: u32,
    pub mode: String,
    #[serde(default = "default_mode")]
    pub base_mode: String,
    #[serde(default)]
    pub backstack: Vec<String>,
    #[serde(default)]
    pub suppressed: bool,
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    pub search_active: bool,
    /// Mode-name -> display style, owned by the active Bar (see module docs).
    #[serde(default)]
    pub palette: BTreeMap<String, ModePalette>,
    /// Raw KDL of the bar's `which_key { … }` block, owned by the active Bar and
    /// parsed by the WhichKey panel. Empty when the bar configures no block.
    #[serde(default)]
    pub which_key_config: String,
    /// Raw KDL of the bar's `search { … }` block, owned by the active Bar and
    /// parsed by the Search dialog for its placement geometry. Empty when the
    /// bar configures no block.
    #[serde(default)]
    pub search_config: String,
}

/// Modes treated as "resting": leaving one never pushes it onto the mode-trail
/// and the WhichKey panel hides in them. The session base mode is always
/// implicitly resting too. Hardcoded on purpose — the plugin owns the whole
/// mode lifecycle, and `Normal`/`Locked` are the only sensible resting modes,
/// so this is not user-configurable.
pub const RESTING_MODES: [InputMode; 2] = [InputMode::Normal, InputMode::Locked];

fn default_mode() -> String {
    mode_name(InputMode::Normal).to_string()
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generation: 0,
            writer: 0,
            mode: default_mode(),
            base_mode: default_mode(),
            backstack: Vec::new(),
            suppressed: false,
            page: 0,
            search_active: false,
            palette: BTreeMap::new(),
            which_key_config: String::new(),
            search_config: String::new(),
        }
    }
}

impl SharedState {
    pub fn mode(&self) -> InputMode {
        str_to_mode(&self.mode).unwrap_or(InputMode::Normal)
    }

    // Accessor kept for symmetry with `mode()`/`backstack()`; the stored
    // `base_mode` is currently informational (consumers track base mode locally
    // from `ModeUpdate`).
    #[allow(dead_code)]
    pub fn base_mode(&self) -> InputMode {
        str_to_mode(&self.base_mode).unwrap_or(InputMode::Normal)
    }

    pub fn backstack(&self) -> Vec<InputMode> {
        self.backstack
            .iter()
            .filter_map(|mode| str_to_mode(mode))
            .collect()
    }

    /// Update the mode and maintain the mode-trail (`backstack`), owned by the
    /// active Bar. Mirrors native which-key behaviour: returning to the base
    /// mode clears the trail; re-entering the last trail mode pops it; otherwise
    /// the mode we left is pushed unless it was a resting mode (see
    /// [`RESTING_MODES`]). Resets `page` on any real mode change. No-ops
    /// (returning `self` unbumped) when nothing observable changed.
    pub fn publish_mode_update(
        mut self,
        new_mode: InputMode,
        base_mode: InputMode,
        writer: u32,
    ) -> Self {
        let old_page = self.page;
        let old_mode = self.mode();
        let mut backstack = self.backstack();

        if new_mode == base_mode {
            backstack.clear();
        } else if backstack.last() == Some(&new_mode) {
            backstack.pop();
        } else if !is_resting(old_mode, base_mode) && old_mode != new_mode {
            backstack.push(old_mode);
        }

        let next_mode = mode_name(new_mode).to_string();
        let next_base_mode = mode_name(base_mode).to_string();
        let next_backstack = modes_to_names(&backstack);
        if self.mode == next_mode
            && self.base_mode == next_base_mode
            && self.backstack == next_backstack
            && old_page == 0
        {
            return self;
        }

        self.schema_version = SCHEMA_VERSION;
        self.mode = next_mode;
        self.base_mode = next_base_mode;
        self.backstack = next_backstack;
        self.page = 0;
        self.bump(writer);
        self
    }

    /// WhichKey manual hide toggle. `visible` is the panel's current visibility
    /// at the moment of toggling, so suppression flips to it (hide when shown).
    pub fn toggle(mut self, visible: bool, writer: u32) -> Self {
        self.bump(writer);
        self.suppressed = visible;
        self
    }

    pub fn next_page(mut self, page_count: usize, writer: u32) -> Self {
        if self.page + 1 < page_count {
            self.bump(writer);
            self.page += 1;
        }
        self
    }

    pub fn prev_page(mut self, writer: u32) -> Self {
        if self.page > 0 {
            self.bump(writer);
            self.page -= 1;
        }
        self
    }

    pub fn bump(&mut self, writer: u32) {
        self.generation = self.generation.saturating_add(1);
        self.writer = writer;
    }
}

/// Session-scoped state-file path, shared by every role of the plugin. The
/// session component is sanitised identically for all roles so they agree on
/// the same file.
pub fn state_path(zellij_pid: u32, session: &str) -> String {
    let session = if session.is_empty() {
        "unknown".to_string()
    } else {
        sanitize_path_component(session)
    };
    format!("/tmp/zj-statusbar-state-{zellij_pid}-{session}.json")
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Read-modify-write a field a role owns, bumping the generation only when the
/// closure actually changed something. Returns the new state (for the caller to
/// broadcast) when changed, else `None`. Used by lightweight roles (Search,
/// WhichKey) that don't carry the Bar's full publish machinery.
pub fn mutate_state_file(
    path: &str,
    writer: u32,
    f: impl FnOnce(&mut SharedState),
) -> Option<SharedState> {
    let mut state = read_state_from(path).unwrap_or_default();
    let before = state.clone();
    f(&mut state);
    if state == before {
        return None;
    }
    state.schema_version = SCHEMA_VERSION;
    state.generation = state.generation.saturating_add(1);
    state.writer = writer;
    let _ = write_state_to(path, &state);
    Some(state)
}

pub fn read_state_from(path: impl AsRef<Path>) -> io::Result<SharedState> {
    let contents = fs::read_to_string(path)?;
    let state = serde_json::from_str(&contents).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid state json: {err}"),
        )
    })?;
    Ok(state)
}

pub fn write_state_to(path: impl AsRef<Path>, state: &SharedState) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let contents = serde_json::to_string(state).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("serialize state: {err}"),
        )
    })?;
    fs::write(&tmp, contents)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn modes_to_names(modes: &[InputMode]) -> Vec<String> {
    modes
        .iter()
        .map(|mode| mode_name(*mode).to_string())
        .collect()
}

fn is_resting(mode: InputMode, base_mode: InputMode) -> bool {
    mode == base_mode || RESTING_MODES.contains(&mode)
}

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

pub fn str_to_mode(s: &str) -> Option<InputMode> {
    match s {
        "Normal" => Some(InputMode::Normal),
        "Locked" => Some(InputMode::Locked),
        "Resize" => Some(InputMode::Resize),
        "Pane" => Some(InputMode::Pane),
        "Tab" => Some(InputMode::Tab),
        "Scroll" => Some(InputMode::Scroll),
        "EnterSearch" => Some(InputMode::EnterSearch),
        "Search" => Some(InputMode::Search),
        "RenameTab" => Some(InputMode::RenameTab),
        "RenamePane" => Some(InputMode::RenamePane),
        "Session" => Some(InputMode::Session),
        "Move" => Some(InputMode::Move),
        "Prompt" => Some(InputMode::Prompt),
        "Tmux" => Some(InputMode::Tmux),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_json() {
        let dir =
            std::env::temp_dir().join(format!("zj-statusbar-state-{}.json", std::process::id()));
        let state = SharedState {
            generation: 7,
            writer: 42,
            mode: "Pane".to_string(),
            base_mode: "Normal".to_string(),
            backstack: vec!["Tmux".to_string()],
            suppressed: true,
            page: 3,
            search_active: true,
            ..SharedState::default()
        };

        write_state_to(&dir, &state).unwrap();
        assert_eq!(read_state_from(&dir).unwrap(), state);
        let _ = fs::remove_file(dir);
    }

    #[test]
    fn mode_transition_pushes_and_pops_backstack() {
        let state = SharedState::default()
            .publish_mode_update(InputMode::Pane, InputMode::Normal, 1)
            .publish_mode_update(InputMode::Tab, InputMode::Normal, 1);
        assert_eq!(state.mode(), InputMode::Tab);
        assert_eq!(state.backstack(), vec![InputMode::Pane]);

        let state = state.publish_mode_update(InputMode::Pane, InputMode::Normal, 1);
        assert_eq!(state.mode(), InputMode::Pane);
        assert!(state.backstack().is_empty());
    }

    #[test]
    fn same_mode_update_preserves_backstack_and_generation() {
        let state = SharedState::default()
            .publish_mode_update(InputMode::Tmux, InputMode::Normal, 1)
            .publish_mode_update(InputMode::Tab, InputMode::Normal, 1);
        let generation = state.generation;

        let state = state.publish_mode_update(InputMode::Tab, InputMode::Normal, 2);
        assert_eq!(state.mode(), InputMode::Tab);
        assert_eq!(state.backstack(), vec![InputMode::Tmux]);
        assert_eq!(state.generation, generation);
        assert_eq!(state.writer, 1);
    }

    #[test]
    fn base_mode_clears_backstack_and_page() {
        let state = SharedState {
            mode: "Tab".to_string(),
            base_mode: "Normal".to_string(),
            backstack: vec!["Pane".to_string()],
            page: 2,
            ..SharedState::default()
        }
        .publish_mode_update(InputMode::Normal, InputMode::Normal, 1);

        assert_eq!(state.mode(), InputMode::Normal);
        assert!(state.backstack().is_empty());
        assert_eq!(state.page, 0);
    }

    #[test]
    fn resting_mode_is_not_pushed_to_backstack() {
        // Leaving a resting mode (Locked) should not push it onto the trail.
        let state = SharedState {
            mode: "Locked".to_string(),
            base_mode: "Normal".to_string(),
            ..SharedState::default()
        }
        .publish_mode_update(InputMode::Scroll, InputMode::Normal, 1);
        assert_eq!(state.mode(), InputMode::Scroll);
        assert!(state.backstack().is_empty());
    }

    #[test]
    fn toggle_and_paging_update_shared_flags() {
        let state = SharedState::default()
            .toggle(true, 1)
            .next_page(3, 1)
            .next_page(3, 1)
            .next_page(3, 1)
            .prev_page(1);

        assert!(state.suppressed);
        assert_eq!(state.page, 1);
    }

    #[test]
    fn mutate_only_bumps_on_change() {
        let dir =
            std::env::temp_dir().join(format!("zj-statusbar-mutate-{}.json", std::process::id()));
        let path = dir.to_string_lossy().to_string();
        let _ = fs::remove_file(&path);

        let first = mutate_state_file(&path, 9, |s| s.search_active = true);
        assert!(first.is_some());
        assert_eq!(first.as_ref().unwrap().generation, 1);
        assert!(first.as_ref().unwrap().search_active);

        // No change -> no bump, no broadcast payload.
        let again = mutate_state_file(&path, 9, |s| s.search_active = true);
        assert!(again.is_none());

        let _ = fs::remove_file(&path);
    }
}
