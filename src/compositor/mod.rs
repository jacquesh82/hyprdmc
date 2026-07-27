//! Compositor plugins: turning a layout into the file a given Wayland
//! compositor reads, and into the request that reconfigures it live.
//!
//! Everything above this module — geometry, validation, profiles, recall,
//! history, the web UI — is compositor-agnostic and always was. What was not is
//! the last step: the `hl.monitor{…}` calls, the `require` line wired into
//! `hyprland.lua`, and the `/eval` request that pushes them to the running
//! session. That step now lives behind [`Compositor`], so supporting another
//! Wayland compositor is one file implementing one trait rather than a fork.
//!
//! ## Why a trait and not a shared object
//!
//! `hyprdmc` ships as a single static binary with no runtime dependencies —
//! that is a promise the README makes and that packaging relies on. A `dlopen`
//! plugin API would trade it for an unstable Rust ABI and a search path to get
//! wrong. A plugin here is therefore a *compile-time* one: a module, a `static`,
//! and one line in [`REGISTRY`]. Adding a compositor stays a self-contained
//! change that touches nothing else.
//!
//! ## Two capabilities, two traits
//!
//! Writing a configuration file and talking to a running compositor are
//! different problems, and they fail differently. Rendering is pure and works on
//! a machine that is not even running that compositor, which is what makes
//! `compositor = "sway"` on a Hyprland box a useful thing rather than a bug.
//! Reaching a session is I/O and only works while the compositor is up.
//!
//! So a plugin implements two traits: [`Compositor`] for the syntax, and
//! [`crate::session::Session`] for the live connection. A plugin may implement
//! only the first — [`Compositor::drives_sessions`] says so, and callers refuse
//! up front instead of guessing. Both plugins shipped here implement both.
//!
//! Writing one: `docs/writing-a-plugin.md`.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use rust_i18n::t;

use crate::input::InputConfig;
use crate::layout::{Layout, OutputState};
use crate::session::Session;

pub mod hyprland;
pub mod sway;

/// What `hyprdmc` needs to know about a compositor.
///
/// Implementations are stateless: they are consulted, never configured, and a
/// single `static` serves the whole process.
pub trait Compositor: Send + Sync {
    /// Stable identifier, used in `config.toml` and on the command line.
    fn id(&self) -> &'static str;

    /// Name to show a human.
    fn label(&self) -> &'static str;

    /// Is this the compositor running in this session?
    ///
    /// Read from the environment only. Probing sockets here would make
    /// detection depend on whether a daemon happens to be up.
    fn running(&self) -> bool;

    /// Directory holding the user's own configuration.
    fn config_dir(&self) -> PathBuf;

    /// The user's main configuration file, the one generated files are wired
    /// into.
    fn main_config(&self) -> PathBuf;

    /// File name for the generated monitor configuration.
    fn monitors_file(&self) -> &'static str;

    /// File name for the generated keyboard and pointer configuration.
    fn input_file(&self) -> &'static str;

    /// Marker that starts a line comment: `--` for Lua, `#` for sway.
    fn comment(&self) -> &'static str;

    /// The directive configuring one output.
    fn output_directive(&self, output: &OutputState) -> String;

    /// Directives that carry the keyboard and pointer settings.
    ///
    /// A list because compositors disagree on the shape: Hyprland takes one
    /// nested call, sway wants one block per device class.
    fn input_directives(&self, input: &InputConfig) -> Vec<String>;

    /// The statement that pulls `generated` into `main`.
    fn include(&self, main: &Path, generated: &Path) -> String;

    /// Does this line already pull `generated` in?
    ///
    /// Only ever called on lines that are not comments.
    fn includes(&self, line: &str, generated: &Path) -> bool;

    /// Does this line pull in *some* other file?
    ///
    /// Separate from [`Self::includes`] because it answers a different question:
    /// where the user keeps their includes, so a new one joins them instead of
    /// landing at the top of the file.
    fn is_include(&self, line: &str) -> bool;

    /// Does this line open a directive that `hyprdmc` is taking over, and that
    /// `init` should therefore comment out?
    fn opens_output(&self, line: &str) -> bool;

    /// Does this plugin know how to talk to a live session at all?
    ///
    /// A capability of the *plugin*, not of the machine: it says whether a
    /// [`Session`] implementation exists, not whether the compositor is running.
    /// Callers use it to disable "apply" up front rather than after a failure —
    /// see [`crate::apply::apply`] and the web UI's Apply button.
    fn drives_sessions(&self) -> bool;

    /// Opens a connection to the running compositor.
    ///
    /// Fails when the compositor is not running, when its socket cannot be found,
    /// or — for a plugin that only renders files — always. Separate from
    /// [`Self::drives_sessions`] because "no implementation" and "not running
    /// right now" are different answers and deserve different messages.
    fn connect(&self) -> Result<Box<dyn Session>>;

    // ---------------------------------------------------------- provided --

    /// Every output's directive, in order.
    fn output_directives(&self, layout: &Layout) -> Vec<String> {
        layout
            .outputs
            .iter()
            .map(|o| self.output_directive(o))
            .collect()
    }

    /// Default path of the generated monitor file.
    fn monitors_path(&self) -> PathBuf {
        self.config_dir().join(self.monitors_file())
    }

    /// Default path of the generated input file.
    fn input_path(&self) -> PathBuf {
        self.config_dir().join(self.input_file())
    }
}

/// Every plugin compiled in. One line per compositor — this is the whole
/// registration mechanism.
static REGISTRY: &[&(dyn Compositor + Sync)] = &[&hyprland::Hyprland, &sway::Sway];

/// The plugins available, in registration order.
pub fn all() -> &'static [&'static (dyn Compositor + Sync)] {
    REGISTRY
}

/// The plugin with this identifier.
pub fn by_id(id: &str) -> Option<&'static (dyn Compositor + Sync)> {
    let id = id.trim().to_ascii_lowercase();
    REGISTRY.iter().copied().find(|c| c.id() == id)
}

/// The compositor this session is running, if we recognise it.
pub fn detect() -> Option<&'static (dyn Compositor + Sync)> {
    REGISTRY.iter().copied().find(|c| c.running())
}

/// The plugin to use: the configured one, else the one detected, else Hyprland.
///
/// Hyprland is the fallback rather than an error because it is what `hyprdmc`
/// was built for and what a session with no recognisable signature is most
/// likely to be — being wrong there costs a clear error from the compositor,
/// whereas refusing to start costs the user their displays.
pub fn resolve(preference: Option<&str>) -> Result<&'static (dyn Compositor + Sync)> {
    match preference.map(str::trim).filter(|p| !p.is_empty()) {
        Some("auto") | None => Ok(detect().unwrap_or(&hyprland::Hyprland)),
        Some(id) => by_id(id).ok_or_else(|| {
            let known = all().iter().map(|c| c.id()).collect::<Vec<_>>().join(", ");
            anyhow!(t!("compositor.unknown", name = id, known = known).to_string())
        }),
    }
}

/// True when the environment names this compositor.
///
/// `XDG_CURRENT_DESKTOP` is a colon-separated list and its case is not
/// guaranteed, which is why this is a helper and not an equality test.
pub(crate) fn desktop_is(name: &str) -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|value| {
        value
            .split(':')
            .any(|entry| entry.trim().eq_ignore_ascii_case(name))
    })
}

/// `$XDG_CONFIG_HOME/<name>`, falling back to `~/.config/<name>`.
pub(crate) fn config_subdir(name: &str) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".config"))
        .join(name)
}

/// Doubles shared by the test modules of other files.
#[cfg(test)]
pub mod testing {
    use super::*;

    /// A plugin that renders but has no session — the shape a "write the file
    /// and reload it yourself" compositor would take.
    pub struct FileOnly;

    impl Compositor for FileOnly {
        fn id(&self) -> &'static str {
            "file-only"
        }
        fn label(&self) -> &'static str {
            "file only"
        }
        fn running(&self) -> bool {
            false
        }
        fn config_dir(&self) -> PathBuf {
            PathBuf::from("/tmp")
        }
        fn main_config(&self) -> PathBuf {
            PathBuf::from("/tmp/config")
        }
        fn monitors_file(&self) -> &'static str {
            "monitors.conf"
        }
        fn input_file(&self) -> &'static str {
            "input.conf"
        }
        fn comment(&self) -> &'static str {
            "#"
        }
        fn output_directive(&self, o: &OutputState) -> String {
            format!("output {}", o.name)
        }
        fn input_directives(&self, _: &InputConfig) -> Vec<String> {
            Vec::new()
        }
        fn include(&self, _: &Path, generated: &Path) -> String {
            format!("include {}", generated.display())
        }
        fn includes(&self, line: &str, _: &Path) -> bool {
            line.starts_with("include")
        }
        fn is_include(&self, line: &str) -> bool {
            line.starts_with("include")
        }
        fn opens_output(&self, line: &str) -> bool {
            line.starts_with("output ")
        }
        fn drives_sessions(&self) -> bool {
            false
        }
        fn connect(&self) -> Result<Box<dyn Session>> {
            anyhow::bail!("this plugin only writes files")
        }
    }

    /// An output to render, so a plugin can be exercised without a compositor.
    pub fn sample_output() -> OutputState {
        OutputState {
            name: "DP-1".into(),
            enabled: true,
            mode: None,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: crate::monitor::Transform::default(),
            mirror_of: None,
            vrr: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{FileOnly, sample_output};

    #[test]
    fn every_plugin_has_a_distinct_identifier() {
        let mut ids: Vec<&str> = all().iter().map(|c| c.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two plugins answer to the same id");
        assert!(count >= 2, "the seam is not proven by a single plugin");
    }

    #[test]
    fn a_plugin_can_be_looked_up_by_id_case_insensitively() {
        assert_eq!(by_id("hyprland").unwrap().id(), "hyprland");
        assert_eq!(by_id("  SWAY ").unwrap().id(), "sway");
        assert!(by_id("mutter").is_none());
    }

    #[test]
    fn an_explicit_choice_wins_and_a_wrong_one_is_named() {
        assert_eq!(resolve(Some("sway")).unwrap().id(), "sway");
        // `unwrap_err` would want Debug on a trait object; this says the same.
        let Err(err) = resolve(Some("mutter")) else {
            panic!("a compositor nobody implements must not silently resolve");
        };
        let err = err.to_string();
        assert!(err.contains("mutter"), "{err}");
        assert!(err.contains("hyprland"), "the error must list what works");
    }

    #[test]
    fn no_preference_falls_back_rather_than_failing() {
        // A session we cannot identify must still be usable.
        assert!(resolve(None).is_ok());
        assert!(resolve(Some("auto")).is_ok());
        assert!(resolve(Some("  ")).is_ok());
    }

    #[test]
    fn generated_file_names_are_distinct_within_a_plugin() {
        for c in all() {
            assert_ne!(
                c.monitors_file(),
                c.input_file(),
                "{}: one file would overwrite the other",
                c.id()
            );
        }
    }

    #[test]
    fn a_plugin_may_render_without_driving_a_session() {
        // The capability the two shipped plugins both have, exercised through one
        // that does not: rendering must work with no session in sight.
        assert!(!FileOnly.drives_sessions());
        assert!(FileOnly.connect().is_err());
        assert_eq!(
            FileOnly.output_directive(&sample_output()),
            "output DP-1",
            "rendering never needs a live compositor"
        );
    }

    #[test]
    fn every_shipped_plugin_drives_a_session() {
        // Both do today. A plugin that does not is legal (see FileOnly) but the
        // registry should not gain one by accident.
        for c in all() {
            assert!(
                c.drives_sessions(),
                "{}: shipped without a session implementation",
                c.id()
            );
        }
    }
}
