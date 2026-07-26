//! Command-line definition.

use clap::{Args, Parser, Subcommand};
use rust_i18n::t;

#[derive(Debug, Parser)]
#[command(
    name = "hyprdmc",
    version,
    about = "Dynamic monitor management for Hyprland",
    long_about = "hyprdmc detects outputs, positions them, rotates or flips them, \
                  and keeps the configuration up to date on hotplug.\n\
                  It also exposes a web interface to do all of this with the mouse."
)]
pub struct Cli {
    /// Verbose logging.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Lists detected outputs.
    List {
        /// Raw JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Shows the available modes of an output.
    Modes {
        /// Connector name (`eDP-1`, `DP-1`…).
        output: String,
    },

    /// Changes an output and applies the change immediately.
    Set(SetArgs),

    /// Positions outputs relative to one another.
    ///
    /// Example: `hyprdmc arrange DP-1 right-of eDP-1`
    /// Relations: left-of, right-of, above, below, same-as.
    Arrange {
        /// Sequence of “OUTPUT RELATION REFERENCE” triples.
        #[arg(required = true, num_args = 3..)]
        spec: Vec<String>,

        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Automatically arranges the outputs from left to right.
    Auto {
        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Profile management.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Applies the profile matching the connected outputs.
    Apply {
        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Writes the current layout to `monitors.conf`.
    Persist,

    /// The last few layouts that were applied, and how to get back to them.
    History {
        #[command(subcommand)]
        action: Option<HistoryAction>,
    },

    /// Wires `monitors.conf` into `hyprland.conf`.
    Init {
        /// Shows what would be done without changing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Daemon: reacts to hotplug events and serves the web interface.
    Daemon {
        #[command(flatten)]
        web: WebArgs,

        /// Do not start the web interface.
        #[arg(long)]
        no_web: bool,
    },

    /// Web interface only, without hotplug monitoring.
    Web {
        #[command(flatten)]
        web: WebArgs,
    },
}

#[derive(Debug, Args)]
pub struct WebArgs {
    /// Listening port (default: the one from the configuration, 8787).
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Listening address (default: 127.0.0.1).
    #[arg(long)]
    pub bind: Option<String>,

    /// Open the web UI in the default browser once the server is listening.
    ///
    /// `web` does this by default; `daemon` does not, since a background
    /// service should not pop a window open every time the session starts.
    #[arg(long, conflicts_with = "no_open")]
    pub open: bool,

    /// Do not open a browser.
    #[arg(long)]
    pub no_open: bool,
}

impl WebArgs {
    /// Should a browser be launched, given what this command does by default?
    pub fn should_open(&self, default: bool) -> bool {
        if self.open {
            return true;
        }
        if self.no_open {
            return false;
        }
        default
    }
}

#[derive(Debug, Args, Clone, Copy)]
pub struct SafetyArgs {
    /// Applies despite validation errors and observed drifts.
    #[arg(long)]
    pub force: bool,

    /// No confirmation prompt and no automatic revert.
    #[arg(long)]
    pub no_confirm: bool,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Name of the connector to change.
    pub output: String,

    /// Mode: `1920x1080@60`, or `preferred`.
    #[arg(short, long)]
    pub mode: Option<String>,

    /// Position in the workspace: `1920x0`.
    #[arg(short, long)]
    pub pos: Option<String>,

    /// Scale factor.
    #[arg(short, long)]
    pub scale: Option<f64>,

    /// Rotation in degrees: 0, 90, 180 or 270.
    #[arg(short, long, value_parser = ["0", "90", "180", "270"])]
    pub rotate: Option<String>,

    /// Flips the image (horizontal mirror effect).
    #[arg(long, conflicts_with = "no_flip")]
    pub flip: bool,

    /// Restores a non-flipped image.
    #[arg(long)]
    pub no_flip: bool,

    /// Mirrors the given output.
    #[arg(long, conflicts_with = "no_mirror")]
    pub mirror: Option<String>,

    /// Stops mirroring another output.
    #[arg(long)]
    pub no_mirror: bool,

    /// Enables the output.
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,

    /// Disables the output.
    #[arg(long)]
    pub disable: bool,

    /// Variable refresh rate.
    #[arg(long, value_parser = parse_onoff)]
    pub vrr: Option<bool>,

    /// Saves the result to the given profile.
    #[arg(long, value_name = "PROFILE")]
    pub save: Option<String>,

    #[command(flatten)]
    pub safety: SafetyArgs,
}

fn parse_onoff(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "oui" => Ok(true),
        "off" | "false" | "0" | "non" => Ok(false),
        other => Err(t!("cli.invalid_onoff", value = other).to_string()),
    }
}

#[derive(Debug, Subcommand)]
pub enum ProfileAction {
    /// Lists saved profiles.
    List,

    /// Shows the details of a profile.
    Show { name: String },

    /// Saves the current layout under this name.
    Save {
        name: String,

        /// The profile will only apply if no other output is connected.
        #[arg(long)]
        exact: bool,
    },

    /// Applies a profile.
    Apply {
        name: String,

        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Deletes a profile.
    Delete { name: String },

    /// Renames a profile.
    Rename { from: String, to: String },
}

#[derive(Debug, Subcommand)]
pub enum HistoryAction {
    /// List the recorded layouts (the default).
    List,

    /// Reapply a recorded layout. `0` is the most recent.
    Restore {
        index: usize,

        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Forget the history and every remembered display set.
    Clear,
}
