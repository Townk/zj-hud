//! Click region tracking for the status bar.
//!
//! `render::render_bar` populates a `ClickMap` while it lays out the bar.
//! The plugin's mouse handler looks up the clicked column in this map to
//! decide which action (if any) to take.

use zellij_tile::prelude::InputMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    /// Switch focus to the given 1-based tab index (the argument is what
    /// `switch_tab_to` expects).
    SwitchToTab(u32),
    /// Switch the editor to the given input mode. Used by the mode segment
    /// to reset back to `Normal`.
    SwitchToMode(InputMode),
    /// Run a user-configured shell command (the argument indexes
    /// `Config::click_commands`). Used by `info` widget and `date_time`
    /// `on_click` handlers.
    RunCommand(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickRegion {
    /// Inclusive start column.
    pub start: usize,
    /// Exclusive end column.
    pub end: usize,
    pub action: ClickAction,
}

#[derive(Debug, Clone, Default)]
pub struct ClickMap {
    pub regions: Vec<ClickRegion>,
}

impl ClickMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, start: usize, end: usize, action: ClickAction) {
        if end > start {
            self.regions.push(ClickRegion { start, end, action });
        }
    }

    /// Translate every region by `offset` columns. Used to convert
    /// side-local coordinates (e.g. relative to the right side's start)
    /// into absolute bar columns.
    pub fn shift(&mut self, offset: usize) {
        if offset == 0 {
            return;
        }
        for region in &mut self.regions {
            region.start += offset;
            region.end += offset;
        }
    }

    pub fn extend(&mut self, other: ClickMap) {
        self.regions.extend(other.regions);
    }

    pub fn lookup(&self, col: usize) -> Option<ClickAction> {
        self.regions
            .iter()
            .find(|r| col >= r.start && col < r.end)
            .map(|r| r.action)
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_matching_region() {
        let mut map = ClickMap::new();
        map.push(0, 10, ClickAction::SwitchToTab(1));
        map.push(10, 20, ClickAction::SwitchToTab(2));

        assert_eq!(map.lookup(0), Some(ClickAction::SwitchToTab(1)));
        assert_eq!(map.lookup(9), Some(ClickAction::SwitchToTab(1)));
        assert_eq!(map.lookup(10), Some(ClickAction::SwitchToTab(2)));
        assert_eq!(map.lookup(19), Some(ClickAction::SwitchToTab(2)));
        assert_eq!(map.lookup(20), None);
    }

    #[test]
    fn empty_region_is_not_stored() {
        let mut map = ClickMap::new();
        map.push(5, 5, ClickAction::SwitchToTab(1));
        assert!(map.is_empty());
    }

    #[test]
    fn shift_translates_regions() {
        let mut map = ClickMap::new();
        map.push(0, 5, ClickAction::SwitchToMode(InputMode::Normal));
        map.shift(10);

        assert_eq!(map.lookup(0), None);
        assert_eq!(
            map.lookup(10),
            Some(ClickAction::SwitchToMode(InputMode::Normal))
        );
        assert_eq!(
            map.lookup(14),
            Some(ClickAction::SwitchToMode(InputMode::Normal))
        );
        assert_eq!(map.lookup(15), None);
    }

    #[test]
    fn shift_zero_is_noop() {
        let mut map = ClickMap::new();
        map.push(0, 5, ClickAction::SwitchToTab(1));
        let before = map.regions.clone();
        map.shift(0);
        assert_eq!(map.regions, before);
    }

    #[test]
    fn extend_concatenates_regions() {
        let mut a = ClickMap::new();
        a.push(0, 5, ClickAction::SwitchToTab(1));
        let mut b = ClickMap::new();
        b.push(5, 10, ClickAction::SwitchToTab(2));
        a.extend(b);
        assert_eq!(a.regions.len(), 2);
        assert_eq!(a.lookup(7), Some(ClickAction::SwitchToTab(2)));
    }
}
