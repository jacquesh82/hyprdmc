//! Applying a layout with a safety net.
//!
//! Hyprland replies `ok` even when it hasn't done what was asked: a
//! nonexistent mode is accepted without complaint, an invalid scale is
//! silently rounded. The only reliable way to know what actually happened is
//! to re-read the state afterwards and compare it to what was wanted — that's
//! the role of [`diff`].
//!
//! But the change isn't instantaneous: a rotation takes roughly fifty
//! milliseconds to be reflected in `j/monitors`. Reading it only once, right
//! after the `ok`, would wrongly conclude failure — hence the settling phase
//! in [`observe`].

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::compositor::Compositor;
use crate::layout::{Issue, Layout, Severity, format_scale};
use crate::monitor::Monitor;
use crate::session::Session;

/// Tolerance on the scale before reporting a drift.
const SCALE_TOLERANCE: f64 = 0.005;
/// Tolerance on the refresh rate (Hyprland rounds: 60 → 60.06).
const REFRESH_TOLERANCE: f64 = 1.5;
/// Interval between two reads while settling.
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);
/// Beyond this, we consider that Hyprland won't do anything more.
const SETTLE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Property a drift is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    Presence,
    Enabled,
    Mode,
    Refresh,
    Position,
    Transform,
    Scale,
    Mirror,
}

impl Field {
    /// Can this drift resolve itself if we wait?
    ///
    /// Yes for everything Hyprland applies on the next commit. No for scale
    /// and refresh rate: those are deliberate corrections made by the
    /// compositor, waiting would change nothing and would waste 1.5s on every
    /// application.
    fn converges(self) -> bool {
        !matches!(self, Field::Scale | Field::Refresh)
    }
}

/// A drift between what was requested and what Hyprland actually did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drift {
    pub output: String,
    pub field: Field,
    pub severity: Severity,
    pub message: String,
}

impl Drift {
    fn error(output: &str, field: Field, message: String) -> Self {
        Self {
            output: output.to_string(),
            field,
            severity: Severity::Error,
            message,
        }
    }

    fn warning(output: &str, field: Field, message: String) -> Self {
        Self {
            output: output.to_string(),
            field,
            severity: Severity::Warning,
            message,
        }
    }
}

/// Result of an apply operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Directives sent to the compositor, in its own syntax.
    pub specs: Vec<String>,
    /// Problems detected before sending.
    pub issues: Vec<Issue>,
    /// Drifts observed after sending.
    pub drifts: Vec<Drift>,
    /// The previous state has been restored.
    pub rolled_back: bool,
}

impl ApplyReport {
    pub fn succeeded(&self) -> bool {
        !self.rolled_back && !self.drifts.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Compares the requested layout to the state actually obtained.
pub fn diff(requested: &Layout, actual: &[Monitor]) -> Vec<Drift> {
    let mut drifts = Vec::new();

    for want in &requested.outputs {
        let Some(got) = actual.iter().find(|m| m.name == want.name) else {
            drifts.push(Drift::error(
                &want.name,
                Field::Presence,
                t!("apply.drift.vanished", name = &want.name).to_string(),
            ));
            continue;
        };

        if want.enabled == got.disabled {
            let wanted_state = if want.enabled {
                t!("apply.state.enabled")
            } else {
                t!("apply.state.disabled")
            };
            let got_state = if got.disabled {
                t!("apply.state.disabled")
            } else {
                t!("apply.state.enabled")
            };
            drifts.push(Drift::error(
                &want.name,
                Field::Enabled,
                t!(
                    "apply.drift.enabled_mismatch",
                    name = &want.name,
                    wanted = wanted_state,
                    got = got_state
                )
                .to_string(),
            ));
            continue;
        }

        if !want.enabled {
            continue;
        }

        if let Some(mode) = want.mode {
            if mode.width != got.width || mode.height != got.height {
                drifts.push(Drift::error(
                    &want.name,
                    Field::Mode,
                    t!(
                        "apply.drift.mode_refused",
                        name = &want.name,
                        wanted = format!("{}x{}", mode.width, mode.height),
                        got = format!("{}x{}", got.width, got.height)
                    )
                    .to_string(),
                ));
            } else if mode.refresh > 0.0
                && (mode.refresh - got.refresh_rate).abs() > REFRESH_TOLERANCE
            {
                drifts.push(Drift::warning(
                    &want.name,
                    Field::Refresh,
                    t!(
                        "apply.drift.refresh_adjusted",
                        name = &want.name,
                        wanted = format!("{:.2}", mode.refresh),
                        got = format!("{:.2}", got.refresh_rate)
                    )
                    .to_string(),
                ));
            }
        }

        // A mirrored output snaps to its source's position: comparing its
        // position to the requested one wouldn't make sense.
        if want.mirror_of.is_none() && (want.x != got.x || want.y != got.y) {
            drifts.push(Drift::error(
                &want.name,
                Field::Position,
                t!(
                    "apply.drift.position_refused",
                    name = &want.name,
                    wanted = format!("{}x{}", want.x, want.y),
                    got = format!("{}x{}", got.x, got.y)
                )
                .to_string(),
            ));
        }

        if want.transform.to_u8() != got.transform {
            drifts.push(Drift::error(
                &want.name,
                Field::Transform,
                t!(
                    "apply.drift.transform_refused",
                    name = &want.name,
                    wanted = want.transform,
                    got = got.transform()
                )
                .to_string(),
            ));
        }

        // Hyprland rounds the scale to a value that yields an integral
        // logical size: this is an expected correction, not a failure.
        if (want.scale - got.scale).abs() > SCALE_TOLERANCE {
            drifts.push(Drift::warning(
                &want.name,
                Field::Scale,
                t!(
                    "apply.drift.scale_adjusted",
                    name = &want.name,
                    wanted = format_scale(want.scale),
                    got = format_scale(got.scale)
                )
                .to_string(),
            ));
        }

        let got_mirror = got.mirror_target(actual);
        if want.mirror_of != got_mirror {
            let none = t!("apply.mirror.none");
            drifts.push(Drift::warning(
                &want.name,
                Field::Mirror,
                t!(
                    "apply.drift.mirror_mismatch",
                    name = &want.name,
                    wanted = want.mirror_of.as_deref().unwrap_or(&none),
                    got = got_mirror.as_deref().unwrap_or(&none)
                )
                .to_string(),
            ));
        }
    }

    drifts
}

/// Reads the current state as a layout, so we can go back to it.
pub fn snapshot(session: &dyn Session) -> Result<Layout> {
    Ok(Layout::from_monitors(&session.outputs()?))
}

/// Re-reads the state until it matches the request, or until timeout.
///
/// Hyprland acknowledges immediately but applies on the next commit: a
/// rotation isn't visible in `j/monitors` until roughly fifty milliseconds
/// later. We return as soon as no more blocking drift remains, so there's no
/// needless waiting in the common case.
pub fn observe(session: &dyn Session, layout: &Layout) -> Result<Vec<Drift>> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let drifts = diff(layout, &session.outputs()?);
        let settled = !drifts.iter().any(|d| d.field.converges());
        if settled || Instant::now() >= deadline {
            return Ok(drifts);
        }
        std::thread::sleep(SETTLE_INTERVAL);
    }
}

/// Sends the layout to Hyprland, checks the result, and rolls back if the
/// result is unusable.
///
/// `force` bypasses validation errors *and* observed drifts: it's the escape
/// hatch for when the user knows what they're doing.
pub fn apply(
    session: &dyn Session,
    compositor: &dyn Compositor,
    layout: &Layout,
    force: bool,
) -> Result<ApplyReport> {
    // Refused before anything is read or written: a plugin with no session
    // implementation has no way to apply, and finding that out after the snapshot
    // would be the same answer, later.
    if !compositor.drives_sessions() {
        bail!(
            t!(
                "compositor.no_live_apply",
                name = compositor.label(),
                file = compositor.monitors_file()
            )
            .to_string()
        );
    }

    let issues = layout.validate();
    let blocking: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    if !blocking.is_empty() && !force {
        bail!(
            t!(
                "apply.rejected",
                issues = blocking
                    .iter()
                    .map(|i| format!("  • {}", i.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .to_string()
        );
    }

    let previous = snapshot(session)?;
    let specs = compositor.output_directives(layout);

    let mut report = ApplyReport {
        specs: specs.clone(),
        issues,
        ..Default::default()
    };

    if let Err(err) = session.apply(&specs) {
        // A batch may have been partially applied: restore the last known-good state.
        restore(session, compositor, &previous).ok();
        return Err(err);
    }

    report.drifts = observe(session, layout)?;

    let fatal = report.drifts.iter().any(|d| d.severity == Severity::Error);
    if fatal && !force {
        restore(session, compositor, &previous)?;
        report.rolled_back = true;
    }

    // The main screen takes the focus. Declaring a screen "main" and leaving the
    // keyboard on another one is not what anyone means by it — and after a
    // rearrangement the focus can easily have landed elsewhere.
    //
    // Best-effort on purpose: a refused dispatch is not worth undoing a layout
    // that Hyprland just accepted.
    if !report.rolled_back
        && let Some(name) = layout.primary_output().map(|o| o.name.clone())
        && let Err(err) = session.focus(&name)
    {
        tracing::debug!("could not focus the main screen {name}: {err:#}");
    }

    Ok(report)
}

/// Reapplies a known layout, without validation or verification: we're going
/// back to a state that worked, and this must not fail.
pub fn restore(session: &dyn Session, compositor: &dyn Compositor, layout: &Layout) -> Result<()> {
    session.apply(&compositor.output_directives(layout))
}

/// Asks the user for confirmation and restores the previous state if there
/// is no answer.
///
/// This is the classic safety net for display settings: if the new
/// configuration makes the screen unreadable, doing nothing is enough to
/// revert.
pub fn confirm_or_revert(
    session: &dyn Session,
    compositor: &dyn Compositor,
    previous: &Layout,
    timeout: Duration,
) -> Result<bool> {
    use std::io::{BufRead, Write};
    use std::sync::mpsc;

    if timeout.is_zero() {
        return Ok(true);
    }
    if !stdin_is_tty() {
        // Without a terminal (script, hook), no one can confirm: we keep it.
        return Ok(true);
    }

    print!(
        "{}",
        t!(
            "apply.confirm_prompt",
            seconds = timeout.as_secs().to_string()
        )
    );
    std::io::stdout().flush().ok();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });

    let keep = match rx.recv_timeout(timeout) {
        Ok(line) => {
            let a = line.trim().to_lowercase();
            a.starts_with('o') || a.starts_with('y')
        }
        Err(_) => {
            println!();
            false
        }
    };

    if !keep {
        restore(session, compositor, previous)?;
    }
    Ok(keep)
}

fn stdin_is_tty() -> bool {
    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(0) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::hyprland::Hyprland;
    use crate::compositor::hyprland::ipc::fake::FakeSession;

    /// The plugin under test for everything that is not about plugin choice.
    const HYPR: Hyprland = Hyprland;
    use crate::layout::OutputState;
    use crate::monitor::{Mode, Rotation, Transform};

    /// `(name, width, height, x, y, scale, transform, disabled)`
    type Row<'a> = (&'a str, i32, i32, i32, i32, f64, u8, bool);

    fn monitors_json(entries: &[Row<'_>]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(name, w, h, x, y, scale, transform, disabled)| {
                format!(
                    r#"{{"id":0,"name":"{name}","description":"d","make":"m","model":"mo","serial":"s",
                    "width":{w},"height":{h},"refreshRate":60.0,"x":{x},"y":{y},"scale":{scale},
                    "transform":{transform},"focused":false,"disabled":{disabled},"mirrorOf":"none",
                    "vrr":false,"availableModes":["1920x1080@60.00Hz"]}}"#
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    fn want(name: &str, w: i32, h: i32, x: i32, y: i32) -> OutputState {
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
    fn diff_is_silent_when_hyprland_obeyed() {
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        assert_eq!(diff(&layout, &actual), Vec::new());
    }

    #[test]
    fn diff_catches_a_silently_refused_mode() {
        // Hyprland replies "ok" for a nonexistent mode: only a fresh read reveals it.
        let layout = Layout::new(vec![want("DP-1", 9999, 9999, 0, 0)]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("mode refused"));
    }

    #[test]
    fn diff_catches_a_refused_position_and_orientation() {
        let mut w = want("DP-1", 1920, 1080, 3000, 0);
        w.transform = Transform::new(Rotation::R90, false);
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 2);
        assert!(drifts.iter().all(|d| d.severity == Severity::Error));
    }

    #[test]
    fn adjusted_scale_is_only_a_warning() {
        // Real-world case: 1.37 requested, 1.33 applied.
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.scale = 1.37;
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> = serde_json::from_str(&monitors_json(&[(
            "DP-1", 1920, 1080, 0, 0, 1.33, 0, false,
        )]))
        .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].severity, Severity::Warning);
    }

    #[test]
    fn refresh_rounding_is_tolerated() {
        // 60 requested, 60.056 applied: it's the same mode.
        let layout = Layout::new(vec![want("eDP-1", 1920, 1080, 0, 0)]);
        let json = monitors_json(&[("eDP-1", 1920, 1080, 0, 0, 1.0, 0, false)])
            .replace("\"refreshRate\":60.0", "\"refreshRate\":60.056");
        let actual: Vec<Monitor> = serde_json::from_str(&json).unwrap();
        assert_eq!(diff(&layout, &actual), Vec::new());
    }

    #[test]
    fn diff_catches_a_vanished_output() {
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]);
        let drifts = diff(&layout, &[]);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("disappeared"));
    }

    #[test]
    fn diff_catches_enable_disable_mismatch() {
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.enabled = false;
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("disabled"));
    }

    /// The `eval` requests sent to the compositor, in order.
    fn evals(backend: &FakeSession) -> Vec<String> {
        backend
            .sent_commands()
            .into_iter()
            .filter(|c| c.starts_with("/eval "))
            .collect()
    }

    #[test]
    fn apply_sends_a_single_eval_and_reports_success() {
        let json = monitors_json(&[
            ("eDP-1", 1920, 1080, 0, 0, 1.0, 0, false),
            ("DP-1", 1920, 1080, 1920, 0, 1.0, 0, false),
        ]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![
            want("eDP-1", 1920, 1080, 0, 0),
            want("DP-1", 1920, 1080, 1920, 0),
        ]);

        let report = apply(&backend, &HYPR, &layout, false).unwrap();
        assert!(report.succeeded());
        assert!(!report.rolled_back);

        let sent = evals(&backend);
        assert_eq!(sent.len(), 1, "expected a single round trip");
        assert!(
            sent[0].contains(r#"output = "eDP-1", mode = "1920x1080@60.00", position = "0x0""#)
        );
        assert!(
            sent[0].contains(r#"output = "DP-1", mode = "1920x1080@60.00", position = "1920x0""#)
        );
    }

    #[test]
    fn a_plugin_that_drives_no_session_refuses_before_reading_anything() {
        // A plugin that only renders files has no way to apply. Saying so up
        // front beats a snapshot, a send that cannot happen, and a rollback of a
        // change that never took place.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]);

        let err = apply(
            &backend,
            &crate::compositor::testing::FileOnly,
            &layout,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("file only"), "{err}");
        assert!(
            backend.sent_commands().is_empty(),
            "not even a read should have happened"
        );
    }

    #[test]
    fn apply_refuses_an_invalid_layout_without_touching_hyprland() {
        let json = monitors_json(&[("A", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        // Two overlapping outputs: validation error.
        let layout = Layout::new(vec![
            want("A", 1920, 1080, 0, 0),
            want("B", 1920, 1080, 100, 0),
        ]);
        let err = apply(&backend, &HYPR, &layout, false).unwrap_err();
        assert!(err.to_string().contains("overlap"));
        assert!(
            !backend
                .sent_commands()
                .iter()
                .any(|c| c.contains("hl.monitor(")),
            "no command should be sent"
        );
    }

    #[test]
    fn apply_rolls_back_when_the_result_is_wrong() {
        // The backend always reports 1920x1080 at 0x0: the request for
        // 3000x0 won't be honored, hence rollback.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 3000, 0)]);

        let report = apply(&backend, &HYPR, &layout, false).unwrap();
        assert!(report.rolled_back);
        assert!(!report.succeeded());

        let sent = evals(&backend);
        assert_eq!(sent.len(), 2, "apply then restore");
        assert!(
            sent[1].contains(r#"position = "0x0""#),
            "restore must rewrite the original state: {}",
            sent[1]
        );
    }

    #[test]
    fn apply_waits_for_hyprland_to_catch_up() {
        // Real-world case: a rotation takes ~50ms to show up in j/monitors.
        // Reading only once would wrongly conclude failure.
        let rotated = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 1, false)]);
        let not_yet = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::settling_after(2, &not_yet, &rotated);

        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.transform = Transform::new(Rotation::R90, false);
        let report = apply(&backend, &HYPR, &Layout::new(vec![w]), false).unwrap();

        assert!(report.succeeded(), "drifts: {:?}", report.drifts);
        assert!(!report.rolled_back);
        assert!(
            backend.monitor_reads() >= 3,
            "the state must be re-read until it converges"
        );
    }

    #[test]
    fn settling_also_waits_for_the_mirror_to_take_effect() {
        // Mirroring is only a warning, but it does eventually apply: we
        // must wait for it like everything else.
        let mirrored = r#"[
          {"id":0,"name":"eDP-1","width":1920,"height":1080,"refreshRate":60.0,"x":0,"y":0,
           "scale":1.0,"transform":0,"disabled":false,"mirrorOf":"none","availableModes":[]},
          {"id":1,"name":"DP-1","width":1920,"height":1080,"refreshRate":60.0,"x":0,"y":0,
           "scale":1.0,"transform":0,"disabled":false,"mirrorOf":"0","availableModes":[]}
        ]"#;
        let not_yet = mirrored.replace(r#""mirrorOf":"0""#, r#""mirrorOf":"none""#);
        let backend = FakeSession::settling_after(2, &not_yet, mirrored);

        let mut b = want("DP-1", 1920, 1080, 0, 0);
        b.mirror_of = Some("eDP-1".into());
        let layout = Layout::new(vec![want("eDP-1", 1920, 1080, 0, 0), b]);

        let report = apply(&backend, &HYPR, &layout, false).unwrap();
        assert_eq!(report.drifts, Vec::new(), "mirroring must be awaited");
    }

    #[test]
    fn an_adjusted_scale_does_not_stall_the_settle_loop() {
        // Hyprland will never revisit its rounding: no point waiting.
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.scale = 1.37;
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.33, 0, false)]);
        let backend = FakeSession::with_monitors(&json);

        let started = Instant::now();
        let report = apply(&backend, &HYPR, &Layout::new(vec![w]), false).unwrap();
        assert!(report.succeeded());
        assert_eq!(report.drifts.len(), 1);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "apply must not wait out the full settle timeout"
        );
    }

    #[test]
    fn settling_gives_up_and_reports_a_genuine_failure() {
        // Nothing moves: after the timeout, the drift is properly reported.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.transform = Transform::new(Rotation::R90, false);

        let report = apply(&backend, &HYPR, &Layout::new(vec![w]), false).unwrap();
        assert!(report.rolled_back);
    }

    #[test]
    fn force_keeps_the_result_despite_drift() {
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 3000, 0)]);

        let report = apply(&backend, &HYPR, &layout, true).unwrap();
        assert!(!report.rolled_back);
        assert!(!report.drifts.is_empty());
    }

    #[test]
    fn apply_moves_the_focus_to_the_main_screen() {
        let json = monitors_json(&[
            ("eDP-1", 1920, 1080, 0, 0, 1.0, 0, false),
            ("DP-1", 1920, 1080, 1920, 0, 1.0, 0, false),
        ]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![
            want("eDP-1", 1920, 1080, 0, 0),
            want("DP-1", 1920, 1080, 1920, 0),
        ])
        .with_primary(Some("DP-1".into()));

        assert!(apply(&backend, &HYPR, &layout, false).unwrap().succeeded());
        assert!(
            backend
                .sent_commands()
                .iter()
                .any(|c| c == "dispatch focusmonitor DP-1")
        );
    }

    #[test]
    fn without_a_main_screen_the_focus_is_left_alone() {
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        apply(
            &backend,
            &HYPR,
            &Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]),
            false,
        )
        .unwrap();
        assert!(
            !backend
                .sent_commands()
                .iter()
                .any(|c| c.contains("focusmonitor")),
            "nothing was designated: the focus is none of our business"
        );
    }

    #[test]
    fn a_rolled_back_layout_does_not_move_the_focus() {
        // The backend never honours 3000x0, so this apply rolls back; focusing
        // the main screen of a layout that no longer exists would be wrong.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeSession::with_monitors(&json);
        let layout =
            Layout::new(vec![want("DP-1", 1920, 1080, 3000, 0)]).with_primary(Some("DP-1".into()));

        assert!(apply(&backend, &HYPR, &layout, false).unwrap().rolled_back);
        assert!(
            !backend
                .sent_commands()
                .iter()
                .any(|c| c.contains("focusmonitor"))
        );
    }

    #[test]
    fn force_bypasses_validation_errors() {
        let json = monitors_json(&[
            ("A", 1920, 1080, 0, 0, 1.0, 0, false),
            ("B", 1920, 1080, 100, 0, 1.0, 0, false),
        ]);
        let backend = FakeSession::with_monitors(&json);
        let layout = Layout::new(vec![
            want("A", 1920, 1080, 0, 0),
            want("B", 1920, 1080, 100, 0),
        ]);
        let report = apply(&backend, &HYPR, &layout, true).unwrap();
        assert!(!report.rolled_back);
    }
}
