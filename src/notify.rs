//! Desktop notifications.
//!
//! The daemon acts on its own — it rearranges screens when hardware changes
//! while the user is looking elsewhere. A notification is how it says what it
//! just did, and which profile it picked.
//!
//! Best-effort throughout: no notification daemon, no graphical session, or no
//! `notify-send` at all must ever prevent a display from being configured.

use std::process::{Command, Stdio};

/// How much the notification interrupts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Routine: a profile was applied as expected.
    Low,
    /// Worth reading: a layout was refused, or reverted on its own.
    Normal,
}

impl Urgency {
    fn as_arg(self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
        }
    }
}

/// Sends a notification, silently doing nothing if that is impossible.
///
/// Returns whether it was actually sent, which the tests rely on and callers
/// are free to ignore.
pub fn send(summary: &str, body: &str, urgency: Urgency) -> bool {
    if !crate::browser::has_display() {
        return false;
    }
    Command::new("notify-send")
        .args(build_args(summary, body, urgency))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Arguments passed to `notify-send`.
///
/// The `x-canonical-private-synchronous` hint makes a new notification replace
/// the previous one from us instead of stacking: docking a laptop can fire
/// several reconciliations in a row, and five stacked popups saying nearly the
/// same thing is worse than none.
fn build_args(summary: &str, body: &str, urgency: Urgency) -> Vec<String> {
    let mut args = vec![
        "--app-name=hyprdmc".to_string(),
        format!("--urgency={}", urgency.as_arg()),
        "--icon=video-display".to_string(),
        "--hint=string:x-canonical-private-synchronous:hyprdmc".to_string(),
        summary.to_string(),
    ];
    if !body.is_empty() {
        args.push(body.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_only_notifications_omit_the_body() {
        let args = build_args("Profile applied", "", Urgency::Low);
        assert_eq!(args.last().unwrap(), "Profile applied");
        assert_eq!(args.iter().filter(|a| !a.starts_with("--")).count(), 1);
    }

    #[test]
    fn body_follows_the_summary() {
        let args = build_args("Display connected", "DP-1", Urgency::Normal);
        assert_eq!(args[args.len() - 2], "Display connected");
        assert_eq!(args[args.len() - 1], "DP-1");
    }

    #[test]
    fn urgency_is_forwarded() {
        assert!(build_args("s", "", Urgency::Low).contains(&"--urgency=low".to_string()));
        assert!(build_args("s", "", Urgency::Normal).contains(&"--urgency=normal".to_string()));
    }

    #[test]
    fn notifications_replace_each_other_rather_than_stacking() {
        let args = build_args("s", "b", Urgency::Low);
        assert!(
            args.iter()
                .any(|a| a.contains("x-canonical-private-synchronous:hyprdmc")),
            "a burst of hotplug events must not produce a stack of popups"
        );
    }

    #[test]
    fn nothing_is_sent_without_a_graphical_session() {
        // Guards the daemon started by systemd before the session is up.
        if !crate::browser::has_display() {
            assert!(!send("s", "b", Urgency::Low));
        }
    }
}
