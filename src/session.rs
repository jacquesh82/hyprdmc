//! Talking to a running compositor, whichever one it is.
//!
//! [`Session`] is the second half of a compositor plugin. The first half —
//! [`crate::compositor::Compositor`] — knows how to *write* a configuration;
//! this one knows how to *reach a live session*: read the outputs, push a
//! change, move the focus, watch for hotplug.
//!
//! They are separate traits because they fail separately. A plugin can render a
//! file on a machine that is not even running that compositor (which is how
//! `hyprdmc persist --compositor sway` works), and a session only exists while
//! the compositor does. Rendering is pure, sessions are I/O.
//!
//! ## Why the event stream is blocking
//!
//! [`EventStream::next_event`] blocks. The daemon runs it on a blocking task and
//! forwards into a channel — see [`crate::daemon::run`] — which means a new
//! plugin needs no async code of its own to participate in hotplug. Hyprland's
//! event socket is line-oriented and sway's is length-framed; both are trivial
//! to read blocking and neither is worth an `async` trait for.

use anyhow::Result;

use crate::input::InputConfig;
use crate::monitor::Monitor;

/// Something that happened in the compositor and that `hyprdmc` reacts to.
///
/// The compositor-agnostic form of a wire event: each session parses its own
/// protocol into this, so the daemon's loop never sees a wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositorEvent {
    /// An output appeared (connector name).
    OutputAdded(String),
    /// An output disappeared (connector name).
    OutputRemoved(String),
    /// The configuration was reloaded: the state may have changed under us.
    ConfigReloaded,
    /// Everything else, kept for verbose logging.
    Other(String),
}

impl CompositorEvent {
    /// Should this trigger a profile re-evaluation?
    pub fn affects_outputs(&self) -> bool {
        matches!(self, Self::OutputAdded(_) | Self::OutputRemoved(_))
    }

    /// The output this event is about, if it names one.
    pub fn output(&self) -> Option<&str> {
        match self {
            Self::OutputAdded(name) | Self::OutputRemoved(name) => Some(name),
            _ => None,
        }
    }
}

/// A live event source. `next_event` blocks; `None` means the compositor closed
/// the connection, which is a reconnect rather than an error.
pub trait EventStream: Send {
    fn next_event(&mut self) -> Option<CompositorEvent>;
}

/// A live connection to a running compositor.
///
/// Every method is blocking: callers that must not block the async executor
/// wrap them in `spawn_blocking`, as [`crate::daemon::AppState`] does.
pub trait Session: Send + Sync {
    /// Outputs as the compositor reports them, disabled ones included.
    fn outputs(&self) -> Result<Vec<Monitor>>;

    /// Pushes directives — already rendered by the plugin — to the session.
    ///
    /// Directives arrive in the compositor's own syntax, so this method is only
    /// ever paired with the plugin that produced them.
    fn apply(&self, directives: &[String]) -> Result<()>;

    /// Moves the focus to an output.
    ///
    /// Best-effort by contract: callers treat a failure as cosmetic, because it
    /// is — see [`crate::apply::apply`], which will not undo a good layout over
    /// a refused focus change.
    fn focus(&self, output: &str) -> Result<()>;

    /// The keyboard and pointer settings currently in force.
    fn read_input(&self) -> Result<InputConfig>;

    /// Applies keyboard and pointer settings to the running session.
    fn apply_input(&self, input: &InputConfig) -> Result<()>;

    /// Opens the event stream.
    fn watch(&self) -> Result<Box<dyn EventStream>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_appearing_and_disappearing_outputs_trigger_a_re_evaluation() {
        assert!(CompositorEvent::OutputAdded("DP-1".into()).affects_outputs());
        assert!(CompositorEvent::OutputRemoved("DP-1".into()).affects_outputs());
        assert!(!CompositorEvent::ConfigReloaded.affects_outputs());
        assert!(!CompositorEvent::Other("workspace".into()).affects_outputs());
    }

    #[test]
    fn an_event_names_the_output_it_is_about() {
        assert_eq!(
            CompositorEvent::OutputAdded("DP-1".into()).output(),
            Some("DP-1")
        );
        assert_eq!(CompositorEvent::ConfigReloaded.output(), None);
    }
}
