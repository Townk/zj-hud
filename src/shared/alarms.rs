//! Per-pane alarm state for the status bar's background-tab notifications.
//!
//! A user arms an alarm on a focused terminal pane; the active bar instance
//! then watches that pane on its timer tick and fires an OS notification when
//! the pane goes idle ([`AlarmKind::Idle`]) or produces fresh output
//! ([`AlarmKind::Activity`]). The arm-set and each pane's monitoring baseline
//! live in this session-scoped file rather than in any one bar instance, so
//! when the user switches tabs the newly-active instance reads the file and
//! continues from the same baseline — the work follows the active instance
//! instead of being handed off.
//!
//! Kept deliberately separate from [`crate::shared::state::SharedState`]: alarm
//! baselines refresh on every output change (sub-second), and folding them into
//! the mode-sync broadcast/generation channel would make it needlessly noisy.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::shared::state::sanitize_path_component;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlarmKind {
    /// Notify when the pane stops producing output for the idle timeout
    /// (e.g. a long compile that just finished).
    Idle,
    /// Notify when the pane produces any new output (e.g. a process that was
    /// blocked waiting on a signal/network and finally printed something).
    Activity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmEntry {
    pub kind: AlarmKind,
    /// Set once the alarm has fired. Monitoring stops, but the tab indicator
    /// stays until the user switches into the tab (which drops the entry).
    #[serde(default)]
    pub fired: bool,
    /// Epoch seconds of the last observed viewport change — the base for the
    /// idle countdown.
    #[serde(default)]
    pub last_change_epoch: u64,
    /// Fingerprint of the last observed viewport, used to detect change.
    #[serde(default)]
    pub content_hash: u64,
}

impl AlarmEntry {
    /// A freshly armed entry baselined against the pane's current content.
    pub fn armed(kind: AlarmKind, now_epoch: u64, content_hash: u64) -> Self {
        Self {
            kind,
            fired: false,
            last_change_epoch: now_epoch,
            content_hash,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmStore {
    /// Keyed by terminal pane id.
    #[serde(default)]
    pub entries: BTreeMap<u32, AlarmEntry>,
}

/// What the monitor should do with an armed entry after sampling its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmOutcome {
    /// Condition met: fire the notification and mark the entry fired.
    Fire,
    /// Content changed but the alarm did not fire: refresh the baseline
    /// (`content_hash` + `last_change_epoch`).
    UpdateBaseline,
    /// Nothing to do this tick.
    None,
}

/// Decide what to do with an armed entry given a freshly sampled content hash.
///
/// `Activity` fires the moment the content differs from the recorded baseline.
/// `Idle` resets its countdown whenever the content changes, and fires once the
/// content has been unchanged for at least `idle_timeout_secs`. A `fired` entry
/// is inert (the indicator lingers until the user visits the tab).
pub fn evaluate(
    entry: &AlarmEntry,
    now_epoch: u64,
    new_hash: u64,
    idle_timeout_secs: u64,
) -> AlarmOutcome {
    if entry.fired {
        return AlarmOutcome::None;
    }
    let changed = new_hash != entry.content_hash;
    match entry.kind {
        AlarmKind::Activity => {
            if changed {
                AlarmOutcome::Fire
            } else {
                AlarmOutcome::None
            }
        }
        AlarmKind::Idle => {
            if changed {
                AlarmOutcome::UpdateBaseline
            } else if now_epoch.saturating_sub(entry.last_change_epoch) >= idle_timeout_secs {
                AlarmOutcome::Fire
            } else {
                AlarmOutcome::None
            }
        }
    }
}

/// Session-scoped alarm-state file path. Mirrors the sanitization of
/// [`crate::shared::state::state_path`] so every bar instance agrees on it.
pub fn path(zellij_pid: u32, session: &str) -> String {
    let session = if session.is_empty() {
        "unknown".to_string()
    } else {
        sanitize_path_component(session)
    };
    format!("/tmp/zj-hud-alarms-{zellij_pid}-{session}.json")
}

pub fn read_from(path: impl AsRef<Path>) -> io::Result<AlarmStore> {
    let contents = fs::read_to_string(path)?;
    serde_json::from_str(&contents).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid alarm json: {err}"),
        )
    })
}

pub fn write_to(path: impl AsRef<Path>, store: &AlarmStore) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string(store).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("serialize alarms: {err}"),
        )
    })?;
    // Stage into a unique temp file before the atomic rename so a write can
    // never be observed half-applied. Only the active instance writes, so
    // contention is effectively nil; the `create_new` (O_EXCL) loop is just
    // belt-and-braces against a stale temp name.
    let mut suffix: u32 = 0;
    let tmp = loop {
        let candidate = path.with_extension(format!("{suffix}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use io::Write;
                file.write_all(contents.as_bytes())?;
                break candidate;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                suffix = suffix.wrapping_add(1);
            }
            Err(err) => return Err(err),
        }
    };
    fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_fires_on_change_only() {
        let entry = AlarmEntry::armed(AlarmKind::Activity, 100, 0xABCD);
        assert_eq!(evaluate(&entry, 101, 0xABCD, 5), AlarmOutcome::None);
        assert_eq!(evaluate(&entry, 101, 0x1234, 5), AlarmOutcome::Fire);
    }

    #[test]
    fn idle_resets_countdown_on_change() {
        let entry = AlarmEntry::armed(AlarmKind::Idle, 100, 0xABCD);
        // Changed content -> refresh baseline, do not fire.
        assert_eq!(
            evaluate(&entry, 103, 0x1234, 5),
            AlarmOutcome::UpdateBaseline
        );
    }

    #[test]
    fn idle_fires_after_timeout_without_change() {
        let entry = AlarmEntry::armed(AlarmKind::Idle, 100, 0xABCD);
        // Same content, not enough time elapsed yet.
        assert_eq!(evaluate(&entry, 104, 0xABCD, 5), AlarmOutcome::None);
        // Same content, timeout reached.
        assert_eq!(evaluate(&entry, 105, 0xABCD, 5), AlarmOutcome::Fire);
    }

    #[test]
    fn fired_entry_is_inert() {
        let mut entry = AlarmEntry::armed(AlarmKind::Activity, 100, 0xABCD);
        entry.fired = true;
        assert_eq!(evaluate(&entry, 200, 0x9999, 5), AlarmOutcome::None);
    }

    #[test]
    fn store_round_trips_through_json() {
        let dir = std::env::temp_dir().join(format!("zj-hud-alarms-{}.json", std::process::id()));
        let mut store = AlarmStore::default();
        store
            .entries
            .insert(7, AlarmEntry::armed(AlarmKind::Idle, 42, 0xDEADBEEF));
        store
            .entries
            .insert(9, AlarmEntry::armed(AlarmKind::Activity, 43, 0x1234));

        write_to(&dir, &store).unwrap();
        assert_eq!(read_from(&dir).unwrap(), store);
        let _ = fs::remove_file(dir);
    }
}
