//! sway: a flat `output …` configuration, `#` comments, and `include`.
//!
//! This plugin exists as much to be useful as to keep the trait honest. Almost
//! nothing it does resembles Hyprland: the syntax is space-separated words
//! rather than Lua tables, rotation and flipping are one hyphenated keyword
//! instead of a number 0..=7, booleans are `enabled`/`disabled`, and there is no
//! mirroring at all. An abstraction that only ever had Hyprland behind it would
//! have quietly baked all of that in.
//!
//! ## Live sessions
//!
//! sway is driven through i3's IPC — a length-framed binary protocol, nothing
//! like Hyprland's line-oriented socket — implemented in [`ipc`]. The same
//! `output …` and `input …` directives this module renders into a file are what
//! `RUN_COMMAND` accepts at runtime, so one renderer serves both paths.
//!
//! One honest gap remains, and it is sway's rather than ours: `GET_INPUTS`
//! reports a keyboard's *active layout name* ("French (alt.)"), never the
//! `xkb_layout` code that produced it. Reading the keyboard back is therefore
//! partial — see [`ipc::read_input`].

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::compositor::Compositor;
use crate::input::InputConfig;
use crate::layout::{OutputState, format_scale};
use crate::monitor::{Rotation, Transform};
use crate::session::{EventStream, Session};

pub mod ipc;

pub struct Sway;

impl Compositor for Sway {
    fn id(&self) -> &'static str {
        "sway"
    }

    fn label(&self) -> &'static str {
        "sway"
    }

    fn running(&self) -> bool {
        std::env::var_os("SWAYSOCK").is_some() || super::desktop_is("sway")
    }

    fn config_dir(&self) -> PathBuf {
        super::config_subdir("sway")
    }

    fn main_config(&self) -> PathBuf {
        self.config_dir().join("config")
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

    /// One `output …` line per screen.
    ///
    /// Every field we own is stated, for the same reason as under Hyprland: sway
    /// keeps whatever a previous line set, so an omitted `transform` would let a
    /// rotation outlive its own removal.
    ///
    /// Mirroring has no equivalent — sway has never grown one, `wl-mirror` is the
    /// usual answer — so a mirrored output is written as the plain output it is,
    /// with a comment saying what was dropped. Silently emitting an overlapping
    /// position would produce a layout nobody asked for.
    fn output_directive(&self, o: &OutputState) -> String {
        let name = quote(&o.name);
        if !o.enabled {
            return format!("output {name} disable");
        }

        let mut line = format!("output {name} enable");
        match o.mode {
            // sway wants the rate in mHz-precision with the unit spelled out.
            Some(m) if m.refresh > 0.0 => {
                line.push_str(&format!(
                    " mode {}x{}@{:.3}Hz",
                    m.width, m.height, m.refresh
                ));
            }
            Some(m) => line.push_str(&format!(" mode {}x{}", m.width, m.height)),
            None => line.push_str(" mode --custom preferred"),
        }
        line.push_str(&format!(" position {} {}", o.x, o.y));
        line.push_str(&format!(" scale {}", format_scale(o.scale)));
        line.push_str(&format!(" transform {}", transform_keyword(o.transform)));
        line.push_str(&format!(
            " adaptive_sync {}",
            if o.vrr { "on" } else { "off" }
        ));

        if let Some(target) = &o.mirror_of {
            line.push_str(&format!(
                "  # mirror of {target} dropped: sway has no equivalent"
            ));
        }
        line
    }

    /// One block per device class, because that is how sway addresses devices.
    ///
    /// `type:keyboard` and `type:pointer` rather than `*`: applying a touchpad
    /// setting to every input device is how you end up with a mouse that scrolls
    /// backwards.
    fn input_directives(&self, input: &InputConfig) -> Vec<String> {
        let mut keyboard = vec![format!("    xkb_layout {}", quote(&input.kb_layout))];
        // Unlike Hyprland, sway treats an empty string as a value to set rather
        // than as "unset", and rejects some of them: the lines are omitted.
        if !input.kb_variant.trim().is_empty() {
            keyboard.push(format!("    xkb_variant {}", quote(&input.kb_variant)));
        }
        if !input.kb_options.trim().is_empty() {
            keyboard.push(format!("    xkb_options {}", quote(&input.kb_options)));
        }

        vec![
            format!("input type:keyboard {{\n{}\n}}", keyboard.join("\n")),
            format!(
                "input type:touchpad {{\n    natural_scroll {}\n}}",
                enabled(input.touchpad_natural_scroll)
            ),
            format!(
                "input type:pointer {{\n    natural_scroll {}\n}}",
                enabled(input.natural_scroll)
            ),
        ]
    }

    /// `include` takes a path, and understands `~`.
    fn include(&self, _main: &Path, generated: &Path) -> String {
        format!("include {}", quote(&crate::emit::tildify(generated)))
    }

    fn includes(&self, line: &str, generated: &Path) -> bool {
        let file = generated.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.is_include(line) && !file.is_empty() && line.contains(file)
    }

    fn is_include(&self, line: &str) -> bool {
        line.trim_start().starts_with("include ")
    }

    fn opens_output(&self, line: &str) -> bool {
        // `output` at the start of the directive only: `workspace 1 output DP-1`
        // assigns a workspace and is none of our business.
        line.trim_start().starts_with("output ")
    }

    fn drives_sessions(&self) -> bool {
        true
    }

    fn connect(&self) -> Result<Box<dyn Session>> {
        Ok(Box::new(SwaySession {
            socket: ipc::socket_path()?,
        }))
    }
}

/// A live sway session.
///
/// Holds the socket path rather than an open connection: sway's IPC is
/// request/reply on a short-lived connection, and one per operation means a
/// compositor restart cannot leave a stale socket wedged in the daemon.
pub struct SwaySession {
    socket: PathBuf,
}

impl SwaySession {
    fn connect(&self) -> Result<ipc::Connection> {
        ipc::Connection::at(&self.socket)
    }

    /// Runs directives as one command.
    ///
    /// sway separates commands with `,` or `;`; `,` is used because our
    /// directives are `output …` lines that must all be parsed against the same
    /// criteria block.
    fn run(&self, directives: &[String]) -> Result<()> {
        if directives.is_empty() {
            return Ok(());
        }
        let mut conn = self.connect()?;
        // One command per round trip rather than one joined string: sway reports
        // failures positionally, and a single bad output name in a joined command
        // makes it impossible to say which line was refused.
        for directive in directives {
            ipc::run_command(&mut conn, directive)?;
        }
        Ok(())
    }
}

impl Session for SwaySession {
    fn outputs(&self) -> Result<Vec<crate::monitor::Monitor>> {
        ipc::outputs(&mut self.connect()?)
    }

    fn apply(&self, directives: &[String]) -> Result<()> {
        self.run(directives)
    }

    fn focus(&self, output: &str) -> Result<()> {
        self.run(&[format!("focus output {}", quote(output))])
    }

    fn read_input(&self) -> Result<InputConfig> {
        ipc::read_input(&mut self.connect()?)
    }

    fn apply_input(&self, input: &InputConfig) -> Result<()> {
        // The rendered blocks are multi-line for the file; runtime commands want
        // one line, so the braces are flattened away.
        let directives: Vec<String> = Sway
            .input_directives(input)
            .iter()
            .map(|block| flatten(block))
            .collect();
        self.run(&directives)
    }

    fn watch(&self) -> Result<Box<dyn EventStream>> {
        Ok(Box::new(ipc::Events::connect(&self.socket)?))
    }
}

/// Turns a rendered `input type:x {\n  a b\n}` block into `input type:x a b`.
///
/// sway accepts both, but only the one-line form as a runtime command.
fn flatten(block: &str) -> String {
    let Some((head, body)) = block.split_once('{') else {
        return block.to_string();
    };
    let settings: Vec<&str> = body
        .trim_end_matches(['}', '\n', ' '])
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // A block with several settings becomes several commands joined by `,`,
    // which is how sway chains them under one criteria.
    let head = head.trim();
    settings
        .iter()
        .map(|s| format!("{head} {s}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// sway's single keyword for rotation and flipping.
///
/// The flipped forms are spelled `flipped-90`, and `flipped` alone means
/// "flipped, not rotated" — there is no `flipped-0`.
fn transform_keyword(t: Transform) -> &'static str {
    match (t.flipped, t.rotation) {
        (false, Rotation::R0) => "normal",
        (false, Rotation::R90) => "90",
        (false, Rotation::R180) => "180",
        (false, Rotation::R270) => "270",
        (true, Rotation::R0) => "flipped",
        (true, Rotation::R90) => "flipped-90",
        (true, Rotation::R180) => "flipped-180",
        (true, Rotation::R270) => "flipped-270",
    }
}

fn enabled(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

/// Quotes a value the way sway's parser reads it.
///
/// sway splits on whitespace and has no escape for a double quote inside a
/// quoted word, so one is dropped rather than written out to break the line it
/// sits on. Output names come from the compositor and patterns come from the
/// user; neither is trusted.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace(['"', '\n', '\r'], ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Compositor;
    use crate::monitor::Mode;

    fn out(name: &str, x: i32, y: i32) -> OutputState {
        OutputState {
            name: name.into(),
            enabled: true,
            mode: Some(Mode::new(1920, 1080, 60.0)),
            x,
            y,
            scale: 1.0,
            transform: Transform::default(),
            mirror_of: None,
            vrr: false,
        }
    }

    #[test]
    fn an_output_line_states_every_field_we_own() {
        assert_eq!(
            Sway.output_directive(&out("DP-1", 1920, 0)),
            "output \"DP-1\" enable mode 1920x1080@60.000Hz position 1920 0 \
             scale 1 transform normal adaptive_sync off"
        );
    }

    #[test]
    fn a_disabled_output_is_one_word() {
        let mut o = out("eDP-1", 0, 0);
        o.enabled = false;
        assert_eq!(Sway.output_directive(&o), "output \"eDP-1\" disable");
    }

    #[test]
    fn rotation_and_flipping_become_sways_single_keyword() {
        let cases = [
            (Rotation::R0, false, "normal"),
            (Rotation::R90, false, "90"),
            (Rotation::R270, false, "270"),
            (Rotation::R0, true, "flipped"),
            (Rotation::R90, true, "flipped-90"),
            (Rotation::R270, true, "flipped-270"),
        ];
        for (rotation, flipped, expected) in cases {
            let mut o = out("DP-1", 0, 0);
            o.transform = Transform::new(rotation, flipped);
            assert!(
                Sway.output_directive(&o)
                    .contains(&format!("transform {expected}")),
                "{rotation:?}/{flipped} should be {expected}"
            );
        }
    }

    #[test]
    fn a_mirror_is_dropped_out_loud_rather_than_faked() {
        // sway has no mirroring. Emitting the position of the target instead
        // would put two screens on top of each other and call it a feature.
        let mut o = out("DP-1", 1920, 0);
        o.mirror_of = Some("eDP-1".into());
        let line = Sway.output_directive(&o);
        assert!(line.contains("# mirror of eDP-1 dropped"), "{line}");
        assert!(
            line.contains("position 1920 0"),
            "the output is still placed"
        );
    }

    #[test]
    fn vrr_and_fractional_scale_are_carried() {
        let mut o = out("DP-1", 0, 0);
        o.vrr = true;
        o.scale = 1.25;
        let line = Sway.output_directive(&o);
        assert!(line.contains("adaptive_sync on"), "{line}");
        assert!(line.contains("scale 1.25"), "{line}");
    }

    #[test]
    fn an_unset_variant_is_omitted_rather_than_set_to_nothing() {
        let directives = Sway.input_directives(&InputConfig {
            kb_layout: "fr".into(),
            kb_variant: String::new(),
            kb_options: String::new(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        });
        let keyboard = &directives[0];
        assert!(keyboard.contains(r#"xkb_layout "fr""#));
        assert!(!keyboard.contains("xkb_variant"), "{keyboard}");
        assert!(!keyboard.contains("xkb_options"), "{keyboard}");
    }

    #[test]
    fn each_device_class_gets_its_own_scroll_direction() {
        let directives = Sway.input_directives(&InputConfig {
            kb_layout: "us".into(),
            kb_variant: String::new(),
            kb_options: String::new(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        });
        let touchpad = directives
            .iter()
            .find(|d| d.contains("type:touchpad"))
            .unwrap();
        let pointer = directives
            .iter()
            .find(|d| d.contains("type:pointer"))
            .unwrap();
        assert!(touchpad.contains("natural_scroll enabled"));
        assert!(pointer.contains("natural_scroll disabled"));
    }

    #[test]
    fn quoting_drops_what_sway_cannot_escape() {
        // sway has no escape for a quote inside a quoted word: keeping it would
        // break the line it sits on.
        assert_eq!(quote(r#"Acme "27""#), r#""Acme 27""#);
    }

    #[test]
    fn an_include_is_a_path_and_is_recognised_again() {
        let main = Path::new("/home/u/.config/sway/config");
        let generated = Path::new("/home/u/.config/sway/monitors.conf");
        let line = Sway.include(main, generated);
        assert!(line.starts_with("include "), "{line}");
        assert!(Sway.includes(&line, generated));
        assert!(!Sway.includes("include \"other.conf\"", generated));
    }

    #[test]
    fn only_an_output_directive_is_adopted() {
        assert!(Sway.opens_output("output DP-1 enable"));
        assert!(Sway.opens_output("   output DP-1 enable"));
        assert!(
            !Sway.opens_output("workspace 1 output DP-1"),
            "assigning a workspace is not configuring an output"
        );
        assert!(!Sway.opens_output("input type:keyboard {"));
    }

    #[test]
    fn sway_drives_live_sessions() {
        assert!(Sway.drives_sessions());
    }

    #[test]
    fn an_input_block_flattens_into_runtime_commands() {
        // The file form is a brace block; `RUN_COMMAND` wants one line per
        // setting, chained under the same criteria.
        let block = "input type:keyboard {\n    xkb_layout \"fr\"\n    xkb_variant \"oss\"\n}";
        assert_eq!(
            flatten(block),
            r#"input type:keyboard xkb_layout "fr", input type:keyboard xkb_variant "oss""#
        );
        // Something with no block at all passes through untouched.
        assert_eq!(flatten("output DP-1 enable"), "output DP-1 enable");
    }
}
