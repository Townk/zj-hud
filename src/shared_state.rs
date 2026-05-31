//! Shared session state for coordinating per-tab statusbar instances.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zellij_tile::prelude::InputMode;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedState {
    pub schema_version: u32,
    pub generation: u64,
    pub writer: u32,
    pub mode: String,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generation: 0,
            writer: 0,
            mode: mode_name(InputMode::Normal).to_string(),
        }
    }
}

impl SharedState {
    pub fn mode(&self) -> InputMode {
        str_to_mode(&self.mode).unwrap_or(InputMode::Normal)
    }

    pub fn publish_mode_update(mut self, mode: InputMode, writer: u32) -> Self {
        let next_mode = mode_name(mode).to_string();
        if self.mode == next_mode {
            return self;
        }

        self.schema_version = SCHEMA_VERSION;
        self.mode = next_mode;
        self.bump(writer);
        self
    }

    fn bump(&mut self, writer: u32) {
        self.generation = self.generation.saturating_add(1);
        self.writer = writer;
    }
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
