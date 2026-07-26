//! What `hyprdmc` remembers between runs.
//!
//! Two distinct things, both living in `state.json` under the XDG state
//! directory — this is derived data the user never edits, so it does not
//! belong next to `config.toml`:
//!
//! * a **history** of the last few layouts actually applied, so a bad change
//!   can be undone even after the confirmation window has closed;
//! * a **recall map** of the last layout used with each set of connected
//!   outputs, so plugging the same screens back in restores what the user had,
//!   without them having to name a profile first.
//!
//! The recall map is what makes the tool useful before any configuration:
//! arrange your screens once, and it comes back on its own next time.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::monitor::Monitor;

/// How many layouts to keep. Enough to walk back out of a bad session,
/// short enough that the list stays readable at a glance.
pub const CAPACITY: usize = 5;

/// A layout that was applied, and the circumstances it was applied in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds. Stored as a number rather than a formatted date to keep
    /// the file stable and avoid a date-formatting dependency.
    pub at: u64,
    /// Which outputs were connected, see [`signature`].
    pub signature: String,
    /// Profile this came from, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub layout: Layout,
}

impl Snapshot {
    pub fn new(layout: Layout, signature: String, profile: Option<String>) -> Self {
        Self {
            at: now(),
            signature,
            profile,
            layout,
        }
    }

    /// Age in seconds, saturating so a clock that moved backwards reads as 0
    /// rather than wrapping around.
    pub fn age_secs(&self) -> u64 {
        now().saturating_sub(self.at)
    }

    /// Human-readable age, in the user's language.
    ///
    /// Relative rather than absolute: in an undo list, "5 min ago" answers the
    /// question better than a timestamp, and it needs no date library.
    pub fn age_label(&self) -> String {
        humanize(self.age_secs())
    }

    /// One-line description of the outputs, for the history listing.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self
            .layout
            .outputs
            .iter()
            .map(|o| {
                if !o.enabled {
                    return format!("{} off", o.name);
                }
                let (w, h) = o.logical_size_rounded();
                let mut line = format!("{} {w}x{h}@{}x{}", o.name, o.x, o.y);
                // Orientation is often the only thing separating two entries:
                // a 90° and a 270° rotation share the same logical size, so
                // without this the undo list shows two identical rows.
                if o.transform != crate::monitor::Transform::default() {
                    line.push_str(&format!(" {}", o.transform));
                }
                if let Some(target) = &o.mirror_of {
                    line.push_str(&format!(" ⧉{target}"));
                }
                line
            })
            .collect();
        parts.sort();
        parts.join(", ")
    }
}

/// Renders an age in seconds as a short relative label.
pub fn humanize(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    match seconds {
        s if s < MINUTE => t!("time.just_now").to_string(),
        s if s < HOUR => t!("time.minutes_ago", count = s / MINUTE).to_string(),
        s if s < DAY => t!("time.hours_ago", count = s / HOUR).to_string(),
        s => t!("time.days_ago", count = s / DAY).to_string(),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Identifies a set of connected outputs, regardless of order or connector.
///
/// Built from fingerprints so that the same physical screens produce the same
/// signature whichever port they end up on — that is the whole point of
/// recalling a layout.
pub fn signature(monitors: &[Monitor]) -> String {
    let mut ids: Vec<String> = monitors.iter().map(Monitor::fingerprint).collect();
    ids.sort();
    ids.join(" + ")
}

/// Everything persisted between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Newest first, at most [`CAPACITY`] entries.
    pub history: Vec<Snapshot>,
    /// Last layout used with each set of outputs, keyed by [`signature`].
    /// A `BTreeMap` keeps the file diffable and its order stable.
    pub recall: BTreeMap<String, Snapshot>,
    /// Where to write back. `None` means "nowhere": [`Store::save`] becomes a
    /// no-op, which is what tests want and what stops a store of unknown
    /// provenance from overwriting the real one.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Store {
    pub fn load() -> Self {
        Self::load_from(&state_path())
    }

    /// Never fails: a corrupt or unreadable state file must not stop the user
    /// from configuring their screens. It is derived data, we start over.
    pub fn load_from(path: &std::path::Path) -> Self {
        let mut store: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        store.path = Some(path.to_path_buf());
        store
    }

    /// A store backed by nothing, for tests and dry runs.
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// Writes back to wherever this store came from.
    pub fn save(&self) -> Result<()> {
        match &self.path {
            Some(path) => self.save_to(path),
            None => Ok(()),
        }
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("could not serialize the state")?;
        crate::emit::write_atomic(path, &body)
    }

    /// Records a layout that was just applied.
    ///
    /// Consecutive identical layouts are collapsed: reconciling on every
    /// hotplug event would otherwise fill the history with five copies of the
    /// same thing and push the entry the user actually wants to undo out of
    /// reach.
    pub fn record(&mut self, snapshot: Snapshot) {
        self.recall
            .insert(snapshot.signature.clone(), snapshot.clone());

        if self
            .history
            .first()
            .is_some_and(|last| last.layout == snapshot.layout)
        {
            return;
        }
        self.history.insert(0, snapshot);
        self.history.truncate(CAPACITY);
    }

    /// Layout previously used with exactly these outputs.
    pub fn recall_for(&self, monitors: &[Monitor]) -> Option<&Snapshot> {
        self.recall.get(&signature(monitors))
    }

    /// History entry by position, `0` being the most recent.
    pub fn entry(&self, index: usize) -> Option<&Snapshot> {
        self.history.get(index)
    }

    /// Forgets everything. Used by `hyprdmc history clear`.
    pub fn clear(&mut self) {
        self.history.clear();
        self.recall.clear();
    }
}

/// `$XDG_STATE_HOME/hyprdmc/state.json`, or the usual fallback.
pub fn state_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".local").join("state"))
        .join("hyprdmc")
        .join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::OutputState;
    use crate::monitor::{Mode, Transform};

    fn monitor(name: &str, make: &str, serial: &str) -> Monitor {
        Monitor {
            id: 0,
            name: name.into(),
            description: String::new(),
            make: make.into(),
            model: "M".into(),
            serial: serial.into(),
            width: 1920,
            height: 1080,
            refresh_rate: 60.0,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
            focused: false,
            disabled: false,
            mirror_of: "none".into(),
            vrr: false,
            available_modes: vec!["1920x1080@60.00Hz".into()],
        }
    }

    fn layout_at(x: i32) -> Layout {
        Layout::new(vec![OutputState {
            name: "DP-1".into(),
            enabled: true,
            mode: Some(Mode::new(1920, 1080, 60.0)),
            x,
            y: 0,
            scale: 1.0,
            transform: Transform::default(),
            mirror_of: None,
            vrr: false,
        }])
    }

    fn snap(x: i32, signature: &str) -> Snapshot {
        Snapshot::new(layout_at(x), signature.to_string(), None)
    }

    #[test]
    fn signature_ignores_order_and_connector() {
        let a = vec![monitor("DP-1", "Dell", "A"), monitor("eDP-1", "AU", "B")];
        let b = vec![monitor("DP-3", "AU", "B"), monitor("HDMI-A-1", "Dell", "A")];
        assert_eq!(signature(&a), signature(&b));
    }

    #[test]
    fn signature_distinguishes_different_sets() {
        let solo = vec![monitor("eDP-1", "AU", "B")];
        let docked = vec![monitor("eDP-1", "AU", "B"), monitor("DP-1", "Dell", "A")];
        assert_ne!(signature(&solo), signature(&docked));
    }

    #[test]
    fn history_keeps_only_the_last_five() {
        let mut store = Store::default();
        for x in 0..8 {
            store.record(snap(x * 100, "sig"));
        }
        assert_eq!(store.history.len(), CAPACITY);
        // Newest first.
        assert_eq!(store.history[0].layout, layout_at(700));
        assert_eq!(store.history[4].layout, layout_at(300));
    }

    #[test]
    fn identical_consecutive_layouts_are_collapsed() {
        // Every hotplug triggers a reconcile; without this the history would
        // fill up with copies of the same layout.
        let mut store = Store::default();
        store.record(snap(0, "sig"));
        store.record(snap(0, "sig"));
        store.record(snap(0, "sig"));
        assert_eq!(store.history.len(), 1);

        store.record(snap(1920, "sig"));
        assert_eq!(store.history.len(), 2);
    }

    #[test]
    fn recall_returns_the_layout_used_with_those_outputs() {
        let solo = vec![monitor("eDP-1", "AU", "B")];
        let docked = vec![monitor("eDP-1", "AU", "B"), monitor("DP-1", "Dell", "A")];

        let mut store = Store::default();
        store.record(snap(0, &signature(&solo)));
        store.record(snap(1920, &signature(&docked)));

        assert_eq!(store.recall_for(&solo).unwrap().layout, layout_at(0));
        assert_eq!(store.recall_for(&docked).unwrap().layout, layout_at(1920));
    }

    #[test]
    fn recall_is_updated_not_appended() {
        let solo = vec![monitor("eDP-1", "AU", "B")];
        let sig = signature(&solo);
        let mut store = Store::default();
        store.record(snap(0, &sig));
        store.record(snap(500, &sig));

        assert_eq!(store.recall.len(), 1);
        assert_eq!(store.recall_for(&solo).unwrap().layout, layout_at(500));
    }

    #[test]
    fn recall_survives_a_layout_being_pushed_out_of_the_history() {
        // The recall map is not bounded: a setup used long ago must still be
        // restored even once its history entry is gone.
        let old = vec![monitor("eDP-1", "AU", "OLD")];
        let mut store = Store::default();
        store.record(snap(42, &signature(&old)));
        for x in 1..10 {
            store.record(snap(x * 100, "other"));
        }
        assert_eq!(store.history.len(), CAPACITY);
        assert_eq!(store.recall_for(&old).unwrap().layout, layout_at(42));
    }

    #[test]
    fn unknown_output_set_recalls_nothing() {
        let store = Store::default();
        assert!(store.recall_for(&[monitor("eDP-1", "AU", "B")]).is_none());
    }

    #[test]
    fn an_ephemeral_store_never_writes_anywhere() {
        // Protects the user's real state file from tests and dry runs.
        let mut store = Store::ephemeral();
        store.record(snap(0, "sig"));
        assert!(store.save().is_ok());
    }

    #[test]
    fn a_corrupt_state_file_is_ignored_rather_than_fatal() {
        let dir = std::env::temp_dir().join(format!("hyprdmc-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(Store::load_from(&path).history.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("hyprdmc-rt-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut store = Store::default();
        store.record(Snapshot::new(
            layout_at(1920),
            "sig".into(),
            Some("desk".into()),
        ));
        store.save_to(&path).unwrap();

        let back = Store::load_from(&path);
        assert_eq!(back.history.len(), 1);
        assert_eq!(back.history[0].profile.as_deref(), Some("desk"));
        assert_eq!(back.history[0].layout, layout_at(1920));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ages_are_rendered_from_the_largest_fitting_unit() {
        assert_eq!(humanize(0), "just now");
        assert_eq!(humanize(59), "just now");
        assert_eq!(humanize(60), "1 min ago");
        assert_eq!(humanize(3599), "59 min ago");
        assert_eq!(humanize(3600), "1 h ago");
        assert_eq!(humanize(86_400), "1 d ago");
        assert_eq!(humanize(200_000), "2 d ago");
    }

    #[test]
    fn describe_lists_outputs_and_flags_disabled_ones() {
        let mut layout = layout_at(0);
        layout.outputs[0].enabled = false;
        let s = Snapshot::new(layout, "sig".into(), None);
        assert_eq!(s.describe(), "DP-1 off");
        assert_eq!(snap(0, "sig").describe(), "DP-1 1920x1080@0x0");
    }

    #[test]
    fn describe_distinguishes_rotations_that_share_a_logical_size() {
        // 90° and 270° both turn 1920x1080 into 1080x1920: without the
        // orientation, two different history entries would read the same.
        use crate::monitor::Rotation;

        let rotated = |rotation, flipped| {
            let mut layout = layout_at(0);
            layout.outputs[0].transform = Transform::new(rotation, flipped);
            Snapshot::new(layout, "sig".into(), None).describe()
        };

        let ninety = rotated(Rotation::R90, false);
        let two_seventy = rotated(Rotation::R270, false);
        assert_ne!(ninety, two_seventy);
        assert!(ninety.contains("1080x1920"), "{ninety}");
        assert_ne!(rotated(Rotation::R90, true), ninety);
    }

    #[test]
    fn describe_marks_mirrored_outputs() {
        let mut layout = layout_at(0);
        layout.outputs[0].mirror_of = Some("eDP-1".into());
        let described = Snapshot::new(layout, "sig".into(), None).describe();
        assert!(described.contains("⧉eDP-1"), "{described}");
    }
}
