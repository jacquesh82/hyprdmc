//! Description of a monitor layout, its validation and automatic
//! arrangement.
//!
//! This entire module is purely computational: no I/O, no dependency on
//! Hyprland. This is where the rules live that keep us from sending the
//! compositor a configuration that would leave the user staring at a black
//! screen.

use std::collections::HashMap;
use std::fmt;

use anyhow::{Result, bail};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::monitor::{Mode, Monitor, Transform};

/// Hyprland works in steps of 1/120 for fractional scales.
const SCALE_STEP: f64 = 1.0 / 120.0;
/// Rounding tolerance used to decide whether a logical size is an integer.
const EPSILON: f64 = 1e-3;

/// Desired configuration for a monitor.
///
/// This is the pivot type of the application: the CLI, the web UI, profiles
/// and the live state all translate into `OutputState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    /// Connector name (`eDP-1`, `DP-3`…).
    pub name: String,
    pub enabled: bool,
    /// `None` = let Hyprland choose the preferred mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_of: Option<String>,
    #[serde(default)]
    pub vrr: bool,
}

impl OutputState {
    /// Desired configuration reflecting a monitor's current state.
    ///
    /// `all` is used to resolve the mirror target, which Hyprland designates
    /// by its numeric id.
    pub fn from_monitor(m: &Monitor, all: &[Monitor]) -> Self {
        Self {
            name: m.name.clone(),
            enabled: !m.disabled,
            mode: if m.width > 0 && m.height > 0 {
                Some(m.mode())
            } else {
                None
            },
            x: m.x,
            y: m.y,
            scale: if m.scale > 0.0 { m.scale } else { 1.0 },
            transform: m.transform(),
            mirror_of: m.mirror_target(all),
            vrr: m.vrr,
        }
    }

    /// Size occupied in the workspace, in logical pixels.
    ///
    /// Rotation swaps the axes; scale divides. Returns `None` as long as the
    /// mode hasn't been resolved.
    pub fn logical_size(&self) -> Option<(f64, f64)> {
        let m = self.mode?;
        let (w, h) = if self.transform.swaps_axes() {
            (m.height, m.width)
        } else {
            (m.width, m.height)
        };
        Some((f64::from(w) / self.scale, f64::from(h) / self.scale))
    }

    /// Rounded logical size, as Hyprland will reserve it.
    pub fn logical_size_rounded(&self) -> (i32, i32) {
        match self.logical_size() {
            Some((w, h)) => (w.round() as i32, h.round() as i32),
            None => (0, 0),
        }
    }

    /// Occupied rectangle: `(x1, y1, x2, y2)`, right/bottom edge excluded.
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        let (w, h) = self.logical_size_rounded();
        (self.x, self.y, self.x + w, self.y + h)
    }

    /// Does it take up space in the workspace?
    ///
    /// A mirrored output deliberately overlaps its target: it is excluded
    /// from overlap detection.
    pub fn occupies_space(&self) -> bool {
        self.enabled && self.mirror_of.is_none()
    }
}

/// Formats a scale without trailing noise: `1`, `1.5`, `1.333333`.
pub fn format_scale(scale: f64) -> String {
    let s = format!("{scale:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "1".to_string()
    } else {
        s.to_string()
    }
}

/// Severity of a problem detected in a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Blocking: Hyprland would refuse the configuration, or it would
    /// render an output unusable.
    Error,
    /// Suspicious but still applicable.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    /// Outputs involved, so the UI can highlight them.
    pub outputs: Vec<String>,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.severity {
            Severity::Error => t!("layout.severity.error"),
            Severity::Warning => t!("layout.severity.warning"),
        };
        write!(f, "[{tag}] {}", self.message)
    }
}

/// A complete layout, ready to be validated then applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub outputs: Vec<OutputState>,
    /// Connector name of the main screen, when the user has designated one.
    ///
    /// Hyprland has no notion of a "primary" output, so this is ours: the main
    /// screen anchors the workspace at (0, 0), opens the row when the outputs
    /// are arranged automatically, and takes the focus once a layout is
    /// applied. `None` restores the plain behaviour — the layout is anchored on
    /// its own top-left corner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

impl Layout {
    pub fn new(outputs: Vec<OutputState>) -> Self {
        Self {
            outputs,
            primary: None,
        }
    }

    /// Same, designating the main screen by connector name.
    pub fn with_primary(mut self, primary: Option<String>) -> Self {
        self.primary = primary;
        self
    }

    /// Layout reflecting the live state.
    ///
    /// The main screen is left unset: it is a choice of the user's, recorded in
    /// `config.toml`, and nothing in what the compositor reports could tell us
    /// what it is — see [`crate::config::Config::primary_output`].
    pub fn from_monitors(monitors: &[Monitor]) -> Self {
        Self::new(
            monitors
                .iter()
                .map(|m| OutputState::from_monitor(m, monitors))
                .collect(),
        )
    }

    /// The main screen, when one is set *and* it is usable as an anchor.
    ///
    /// A screen that is off — or mirroring another one, and therefore sitting
    /// on top of it — takes up no space of its own and cannot anchor anything.
    pub fn primary_output(&self) -> Option<&OutputState> {
        self.primary
            .as_deref()
            .and_then(|name| self.get(name))
            .filter(|o| o.occupies_space())
    }

    pub fn get(&self, name: &str) -> Option<&OutputState> {
        self.outputs.iter().find(|o| o.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut OutputState> {
        self.outputs.iter_mut().find(|o| o.name == name)
    }

    pub fn active(&self) -> impl Iterator<Item = &OutputState> {
        self.outputs.iter().filter(|o| o.occupies_space())
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|i| i.severity == Severity::Error)
    }

    /// Reviews every known pitfall.
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();

        // Duplicate connector: the last directive would overwrite the first.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for o in &self.outputs {
            *seen.entry(o.name.as_str()).or_insert(0) += 1;
        }
        for (name, count) in seen.iter().filter(|(_, c)| **c > 1) {
            issues.push(Issue {
                severity: Severity::Error,
                outputs: vec![(*name).to_string()],
                message: t!(
                    "layout.issue.duplicate_output",
                    name = *name,
                    count = count.to_string()
                )
                .to_string(),
            });
        }

        for o in &self.outputs {
            if !o.enabled {
                continue;
            }

            if o.scale <= 0.0 {
                issues.push(Issue {
                    severity: Severity::Error,
                    outputs: vec![o.name.clone()],
                    message: t!(
                        "layout.issue.invalid_scale",
                        name = o.name,
                        scale = o.scale.to_string()
                    )
                    .to_string(),
                });
                continue;
            }

            // A scale that doesn't yield an integral logical size isn't
            // rejected by Hyprland: it silently rounds it. Might as well warn
            // about the value that will actually be used.
            if let Some((w, h)) = o.logical_size()
                && (!is_integral(w) || !is_integral(h))
            {
                let suggestion = nearest_valid_scale(o.mode.unwrap(), o.transform, o.scale);
                issues.push(Issue {
                    severity: Severity::Warning,
                    outputs: vec![o.name.clone()],
                    message: t!(
                        "layout.issue.non_integral_scale",
                        scale = format_scale(o.scale),
                        width = format!("{w:.3}"),
                        height = format!("{h:.3}"),
                        name = o.name,
                        suggestion = format_scale(suggestion)
                    )
                    .to_string(),
                });
            }

            // A missing mirror target makes the directive fail.
            if let Some(target) = &o.mirror_of {
                match self.get(target) {
                    None => issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![o.name.clone()],
                        message: t!(
                            "layout.issue.mirror_missing",
                            name = o.name,
                            target = target
                        )
                        .to_string(),
                    }),
                    Some(t) if !t.enabled => issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![o.name.clone(), target.clone()],
                        message: t!(
                            "layout.issue.mirror_disabled",
                            name = o.name,
                            target = target
                        )
                        .to_string(),
                    }),
                    Some(_) => {}
                }
            }
        }

        // Two outputs that overlap: the common area becomes unreachable by
        // the mouse on one of them.
        let active: Vec<&OutputState> = self.active().collect();
        for (i, a) in active.iter().enumerate() {
            for b in &active[i + 1..] {
                if rects_overlap(a.rect(), b.rect()) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![a.name.clone(), b.name.clone()],
                        message: t!("layout.issue.overlap", a = a.name, b = b.name).to_string(),
                    });
                }
            }
        }

        // An isolated output stays reachable via the keyboard but the mouse
        // can't reach it: that's a warning, not an error.
        if active.len() > 1 {
            for a in &active {
                let touches = active
                    .iter()
                    .any(|b| b.name != a.name && rects_touch(a.rect(), b.rect()));
                if !touches {
                    issues.push(Issue {
                        severity: Severity::Warning,
                        outputs: vec![a.name.clone()],
                        message: t!("layout.issue.unreachable", name = a.name).to_string(),
                    });
                }
            }
        }

        if !self.outputs.is_empty() && active.is_empty() {
            issues.push(Issue {
                severity: Severity::Error,
                outputs: Vec::new(),
                message: t!("layout.issue.all_disabled").to_string(),
            });
        }

        // A main screen that is gone, off, or mirroring another one anchors
        // nothing: the layout still works, but not around the screen the user
        // asked for, and that is worth saying out loud.
        if let Some(name) = &self.primary
            && self.primary_output().is_none()
        {
            issues.push(Issue {
                severity: Severity::Warning,
                outputs: vec![name.clone()],
                message: t!("layout.issue.primary_unavailable", name = name).to_string(),
            });
        }

        issues
    }

    /// Brings the layout back to (0, 0).
    ///
    /// Without a main screen, that means its top-left corner. With one, it
    /// means the main screen itself: it becomes the origin of the workspace and
    /// everything to its left or above it takes negative coordinates, which
    /// Hyprland accepts. That is the whole point of designating a main screen —
    /// the coordinate space is built around it, so whatever reads the output at
    /// (0, 0) lands on the one the user calls their main one.
    pub fn normalize(&mut self) {
        let (dx, dy) = match self.primary_output() {
            Some(anchor) => (anchor.x, anchor.y),
            None => (
                self.active().map(|o| o.x).min().unwrap_or(0),
                self.active().map(|o| o.y).min().unwrap_or(0),
            ),
        };
        if dx == 0 && dy == 0 {
            return;
        }
        for o in self.outputs.iter_mut().filter(|o| o.occupies_space()) {
            o.x -= dx;
            o.y -= dy;
        }
    }

    /// Lines up the active outputs side by side, left to right, top-aligned.
    ///
    /// This is the fallback when no profile matches the connected hardware. The
    /// main screen opens the row: "left to right" has to start somewhere, and
    /// starting anywhere else would put the user's main screen off to the side.
    pub fn auto_arrange(&mut self) {
        let primary = self.primary.clone();
        let mut order: Vec<usize> = (0..self.outputs.len())
            .filter(|&i| self.outputs[i].occupies_space())
            .collect();
        // Stable sort: everything that is not the main screen keeps its
        // declaration order, which is the order Hyprland reports the outputs in.
        order.sort_by_key(|&i| usize::from(Some(&self.outputs[i].name) != primary.as_ref()));

        let mut cursor = 0;
        for i in order {
            let o = &mut self.outputs[i];
            o.x = cursor;
            o.y = 0;
            cursor += o.logical_size_rounded().0;
        }
    }

    /// Applies a placement relation: `subject` is placed against `anchor`.
    pub fn place(&mut self, subject: &str, relation: Relation, anchor: &str) -> Result<()> {
        let (ax, ay, aw, ah) = match self.get(anchor) {
            Some(a) => {
                let (w, h) = a.logical_size_rounded();
                (a.x, a.y, w, h)
            }
            None => bail!(t!("layout.unknown_anchor", name = anchor).to_string()),
        };
        let (sw, sh) = match self.get(subject) {
            Some(s) => s.logical_size_rounded(),
            None => bail!(t!("layout.unknown_output", name = subject).to_string()),
        };
        if subject == anchor {
            bail!(t!("layout.self_reference", name = subject).to_string());
        }

        let (x, y) = match relation {
            Relation::LeftOf => (ax - sw, ay),
            Relation::RightOf => (ax + aw, ay),
            Relation::Above => (ax, ay - sh),
            Relation::Below => (ax, ay + ah),
            Relation::SameAs => (ax, ay),
        };

        let s = self.get_mut(subject).expect("checked above");
        s.x = x;
        s.y = y;
        Ok(())
    }
}

/// Placement relations accepted by `hyprdmc arrange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    LeftOf,
    RightOf,
    Above,
    Below,
    SameAs,
}

impl std::str::FromStr for Relation {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "left-of" | "gauche-de" | "left" => Relation::LeftOf,
            "right-of" | "droite-de" | "right" => Relation::RightOf,
            "above" | "au-dessus-de" | "up" => Relation::Above,
            "below" | "en-dessous-de" | "down" => Relation::Below,
            "same-as" | "mirror-position" => Relation::SameAs,
            other => bail!(t!("layout.unknown_relation", value = other).to_string()),
        })
    }
}

fn is_integral(v: f64) -> bool {
    (v - v.round()).abs() < EPSILON
}

fn rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// Do the rectangles share an edge over a non-zero length?
fn rects_touch(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    let x_overlap = a.0 < b.2 && b.0 < a.2;
    let y_overlap = a.1 < b.3 && b.1 < a.3;
    let x_adjacent = a.2 == b.0 || b.2 == a.0;
    let y_adjacent = a.3 == b.1 || b.3 == a.1;
    (x_adjacent && y_overlap) || (y_adjacent && x_overlap)
}

/// Finds the valid scale closest to the one requested.
///
/// Hyprland requires that `size / scale` land on an integer; it works in
/// steps of 1/120. We sweep these steps around the desired value and keep
/// the first acceptable candidate.
pub fn nearest_valid_scale(mode: Mode, transform: Transform, wanted: f64) -> f64 {
    let (w, h) = if transform.swaps_axes() {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    };
    let valid = |s: f64| s > 0.0 && is_integral(f64::from(w) / s) && is_integral(f64::from(h) / s);
    if valid(wanted) {
        return wanted;
    }

    let base = (wanted / SCALE_STEP).round();
    // ±0.5 around the requested value, i.e. 60 steps on each side.
    for step in 1..=60 {
        for candidate in [(base + f64::from(step)), (base - f64::from(step))] {
            let s = candidate * SCALE_STEP;
            if valid(s) {
                return (s * 120.0).round() / 120.0;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Rotation;

    fn out(name: &str, w: i32, h: i32, x: i32, y: i32) -> OutputState {
        OutputState {
            name: name.into(),
            enabled: true,
            mode: Some(Mode::new(w, h, 60.0)),
            x,
            y,
            scale: 1.0,
            transform: Transform::default(),
            mirror_of: None,
            vrr: false,
        }
    }

    #[test]
    fn logical_size_swaps_axes_when_rotated() {
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
        o.transform = Transform::new(Rotation::R90, false);
        assert_eq!(o.logical_size_rounded(), (1080, 1920));
        o.transform = Transform::new(Rotation::R180, false);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
    }

    #[test]
    fn logical_size_divides_by_scale() {
        let mut o = out("DP-1", 3840, 2160, 0, 0);
        o.scale = 1.5;
        assert_eq!(o.logical_size_rounded(), (2560, 1440));
    }

    #[test]
    fn flipping_does_not_change_logical_size() {
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        o.transform = Transform::new(Rotation::R0, true);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
    }

    #[test]
    fn scale_formatting_drops_noise() {
        assert_eq!(format_scale(1.0), "1");
        assert_eq!(format_scale(1.5), "1.5");
        assert_eq!(format_scale(1.6666666666), "1.666667");
    }

    #[test]
    fn overlap_is_an_error() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1000, 0),
        ]);
        let issues = layout.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("overlap"))
        );
    }

    #[test]
    fn adjacent_screens_are_clean() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1920, 0),
        ]);
        assert_eq!(layout.validate(), Vec::new());
    }

    #[test]
    fn isolated_screen_warns_but_does_not_block() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 5000, 0),
        ]);
        let issues = layout.validate();
        assert!(!layout.has_errors());
        assert!(issues.iter().any(|i| i.severity == Severity::Warning));
    }

    #[test]
    fn corner_contact_does_not_count_as_touching() {
        // Corner against corner: no shared edge, the cursor can't cross.
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1920, 1080),
        ]);
        assert!(
            layout
                .validate()
                .iter()
                .any(|i| i.severity == Severity::Warning)
        );
    }

    #[test]
    fn mirrored_output_may_overlap_its_target() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("A".into());
        let layout = Layout::new(vec![out("A", 1920, 1080, 0, 0), b]);
        assert!(!layout.has_errors());
    }

    #[test]
    fn mirroring_a_missing_or_disabled_output_is_an_error() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("ABSENT".into());
        assert!(Layout::new(vec![out("A", 1920, 1080, 0, 0), b]).has_errors());

        let mut a = out("A", 1920, 1080, 0, 0);
        a.enabled = false;
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("A".into());
        assert!(Layout::new(vec![a, b]).has_errors());
    }

    #[test]
    fn duplicate_connector_is_an_error() {
        let layout = Layout::new(vec![out("A", 1920, 1080, 0, 0), out("A", 1280, 720, 0, 0)]);
        assert!(
            layout
                .validate()
                .iter()
                .any(|i| i.message.contains("defined 2 times"))
        );
    }

    #[test]
    fn disabling_everything_is_an_error() {
        let mut a = out("A", 1920, 1080, 0, 0);
        a.enabled = false;
        assert!(Layout::new(vec![a]).has_errors());
    }

    #[test]
    fn non_integral_scale_warns_without_blocking() {
        // Hyprland accepts 1.37 and silently rounds it to 1.33: that's a
        // warning, not a refusal.
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        o.scale = 1.37;
        let layout = Layout::new(vec![o]);
        let issues = layout.validate();
        let issue = issues
            .iter()
            .find(|i| i.message.contains("non-integral"))
            .expect("the wonky scale should be reported");
        assert_eq!(issue.severity, Severity::Warning);
        assert!(!layout.has_errors());
    }

    #[test]
    fn nearest_valid_scale_finds_a_usable_value() {
        let mode = Mode::new(1920, 1080, 60.0);
        let s = nearest_valid_scale(mode, Transform::default(), 1.37);
        assert!((1920.0 / s - (1920.0 / s).round()).abs() < 1e-3);
        assert!((1080.0 / s - (1080.0 / s).round()).abs() < 1e-3);
        assert!((s - 1.37).abs() < 0.5, "scale too far off: {s}");
    }

    #[test]
    fn nearest_valid_scale_keeps_already_valid_values() {
        let mode = Mode::new(3840, 2160, 60.0);
        assert_eq!(nearest_valid_scale(mode, Transform::default(), 1.5), 1.5);
    }

    #[test]
    fn auto_arrange_lines_screens_up() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 500, 500),
            out("B", 2560, 1440, 0, 0),
        ]);
        layout.auto_arrange();
        assert_eq!(
            (layout.get("A").unwrap().x, layout.get("A").unwrap().y),
            (0, 0)
        );
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (1920, 0)
        );
        assert!(!layout.has_errors());
    }

    #[test]
    fn auto_arrange_accounts_for_rotation() {
        let mut a = out("A", 1920, 1080, 0, 0);
        a.transform = Transform::new(Rotation::R90, false);
        let mut layout = Layout::new(vec![a, out("B", 1920, 1080, 0, 0)]);
        layout.auto_arrange();
        // A rotated occupies 1080 of width.
        assert_eq!(layout.get("B").unwrap().x, 1080);
        assert!(!layout.has_errors());
    }

    #[test]
    fn auto_arrange_skips_disabled_screens() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.enabled = false;
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            b,
            out("C", 1280, 720, 0, 0),
        ]);
        layout.auto_arrange();
        assert_eq!(layout.get("C").unwrap().x, 1920);
    }

    #[test]
    fn place_positions_relative_to_anchor() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1280, 720, 9999, 9999),
        ]);
        layout.place("B", Relation::LeftOf, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (-1280, 0)
        );

        layout.place("B", Relation::Below, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, 1080)
        );

        layout.place("B", Relation::Above, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, -720)
        );
    }

    #[test]
    fn place_rejects_unknown_and_self_reference() {
        let mut layout = Layout::new(vec![out("A", 1920, 1080, 0, 0)]);
        assert!(layout.place("A", Relation::LeftOf, "ABSENT").is_err());
        assert!(layout.place("A", Relation::LeftOf, "A").is_err());
    }

    #[test]
    fn normalize_moves_layout_back_to_origin() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, -1920, -100),
            out("B", 1920, 1080, 0, -100),
        ]);
        layout.normalize();
        assert_eq!(
            (layout.get("A").unwrap().x, layout.get("A").unwrap().y),
            (0, 0)
        );
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (1920, 0)
        );
    }

    #[test]
    fn normalize_anchors_the_layout_on_the_main_screen() {
        // The main screen becomes the origin, so its neighbour to the left ends
        // up at a negative coordinate instead of pushing it to the right.
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1920, 0),
        ])
        .with_primary(Some("B".into()));
        layout.normalize();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, 0)
        );
        assert_eq!(layout.get("A").unwrap().x, -1920);
        assert!(!layout.has_errors());
    }

    #[test]
    fn a_disabled_main_screen_falls_back_to_the_corner_and_warns() {
        let mut a = out("A", 1920, 1080, 500, 500);
        a.enabled = false;
        let mut layout =
            Layout::new(vec![a, out("B", 1920, 1080, 500, 500)]).with_primary(Some("A".into()));
        layout.normalize();
        // Nothing was anchored on the screen that is off; B is at the corner.
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, 0)
        );
        assert!(!layout.has_errors());
        assert!(
            layout
                .validate()
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("main screen"))
        );
    }

    #[test]
    fn an_unknown_main_screen_only_warns() {
        let layout =
            Layout::new(vec![out("A", 1920, 1080, 0, 0)]).with_primary(Some("ABSENT".into()));
        assert!(!layout.has_errors());
        assert_eq!(layout.validate().len(), 1);
    }

    #[test]
    fn auto_arrange_starts_with_the_main_screen() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 2560, 1440, 0, 0),
            out("C", 1280, 720, 0, 0),
        ])
        .with_primary(Some("B".into()));
        layout.auto_arrange();
        assert_eq!(layout.get("B").unwrap().x, 0);
        // The others keep their reported order behind it.
        assert_eq!(layout.get("A").unwrap().x, 2560);
        assert_eq!(layout.get("C").unwrap().x, 2560 + 1920);
        assert!(!layout.has_errors());
    }

    #[test]
    fn relation_parsing_is_forgiving() {
        assert_eq!("left-of".parse::<Relation>().unwrap(), Relation::LeftOf);
        assert_eq!("RIGHT_OF".parse::<Relation>().unwrap(), Relation::RightOf);
        assert_eq!("droite-de".parse::<Relation>().unwrap(), Relation::RightOf);
        assert!("diagonale".parse::<Relation>().is_err());
    }
}
