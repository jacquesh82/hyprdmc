//! Hyprland, since 0.55: a Lua configuration, and `/eval` to reconfigure a
//! running session.
//!
//! hyprlang is gone from this version, and with it `monitor = …` directives and
//! `source = …`. What replaces them is a Lua API — `hl.monitor{…}`,
//! `hl.config{…}` — pulled in with `require`, and a compositor that refuses
//! `keyword` outright ("keyword can't work with non-legacy parsers"), which is
//! why the live path goes through `/eval` too.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rust_i18n::t;

use crate::compositor::Compositor;
use crate::input::InputConfig;
use crate::layout::{OutputState, format_scale};
use crate::session::{EventStream, Session};

pub mod ipc;

pub struct Hyprland;

impl Compositor for Hyprland {
    fn id(&self) -> &'static str {
        "hyprland"
    }

    fn label(&self) -> &'static str {
        "Hyprland"
    }

    fn running(&self) -> bool {
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() || super::desktop_is("Hyprland")
    }

    fn config_dir(&self) -> PathBuf {
        super::config_subdir("hypr")
    }

    fn main_config(&self) -> PathBuf {
        self.config_dir().join("hyprland.lua")
    }

    fn monitors_file(&self) -> &'static str {
        "monitors.lua"
    }

    /// `inputs.lua`, not `input.lua`: the file is pulled in with
    /// `require("<stem>")`, and `input` is already taken in Hyprland's Lua
    /// environment — requiring it would resolve to that module instead of ours.
    fn input_file(&self) -> &'static str {
        "inputs.lua"
    }

    fn comment(&self) -> &'static str {
        "--"
    }

    /// Renders the `hl.monitor{…}` call that configures this output.
    ///
    /// Hyprland's Lua rules are cumulative: a field left out keeps whatever an
    /// earlier call gave it. Every field we own is therefore always written out,
    /// otherwise a mirror or a rotation would survive its own removal.
    fn output_directive(&self, o: &OutputState) -> String {
        if !o.enabled {
            return format!(
                "hl.monitor({{ output = {}, disabled = true }})",
                lua_string(&o.name)
            );
        }
        let mode = match o.mode {
            Some(m) => format!("{}x{}@{:.2}", m.width, m.height, m.refresh),
            None => "preferred".to_string(),
        };
        format!(
            "hl.monitor({{ output = {}, mode = {}, position = \"{}x{}\", \
             scale = {}, transform = {}, mirror = {}, vrr = {}, disabled = false }})",
            lua_string(&o.name),
            lua_string(&mode),
            o.x,
            o.y,
            format_scale(o.scale),
            o.transform.to_u8(),
            lua_string(o.mirror_of.as_deref().unwrap_or("")),
            u8::from(o.vrr),
        )
    }

    /// One `hl.config{…}` call carrying every setting.
    ///
    /// One call rather than one per field: the compositor reconfigures the
    /// devices once, and a half-applied keyboard is not a state anyone wants to
    /// debug.
    fn input_directives(&self, input: &InputConfig) -> Vec<String> {
        vec![format!(
            "hl.config({{ input = {{ kb_layout = {}, kb_variant = {}, kb_options = {}, \
             natural_scroll = {}, touchpad = {{ natural_scroll = {} }} }} }})",
            lua_string(&input.kb_layout),
            lua_string(&input.kb_variant),
            lua_string(&input.kb_options),
            input.natural_scroll,
            input.touchpad_natural_scroll,
        )]
    }

    /// `require("monitors")` when the file is a neighbour, `dofile` otherwise.
    ///
    /// Hyprland's `package.path` only covers its own configuration directory, so
    /// `require` works exactly when both files sit side by side; a generated file
    /// placed anywhere else is loaded by absolute path.
    fn include(&self, main: &Path, generated: &Path) -> String {
        let module = generated.file_stem().and_then(|s| s.to_str());
        match (module, generated.parent(), main.parent()) {
            (Some(m), Some(dir), Some(conf_dir)) if dir == conf_dir => {
                format!("require({})", lua_string(m))
            }
            _ => format!("dofile({})", lua_string(&generated.display().to_string())),
        }
    }

    fn includes(&self, line: &str, generated: &Path) -> bool {
        let module = generated.file_stem().and_then(|n| n.to_str()).unwrap_or("");
        let file = generated.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.is_include(line)
            && !module.is_empty()
            && (line.contains(module) || line.contains(file))
    }

    fn is_include(&self, line: &str) -> bool {
        line.contains("require(") || line.contains("dofile(")
    }

    fn opens_output(&self, line: &str) -> bool {
        line.contains("hl.monitor(")
    }

    fn drives_sessions(&self) -> bool {
        true
    }

    fn connect(&self) -> Result<Box<dyn Session>> {
        Ok(Box::new(HyprSession::new(Arc::new(
            ipc::HyprSocket::connect()?,
        ))))
    }
}

/// A live Hyprland session.
///
/// Since 0.55 the configuration is Lua and `keyword` is refused outright
/// ("keyword can't work with non-legacy parsers"), so every change goes through
/// `/eval`.
pub struct HyprSession {
    transport: Arc<dyn ipc::Transport>,
    /// Where the event socket lives. Absent for a stubbed transport, which has
    /// no socket to stream from.
    sockets: Option<ipc::HyprSocket>,
}

impl HyprSession {
    pub fn new(socket: Arc<ipc::HyprSocket>) -> Self {
        Self {
            sockets: Some((*socket).clone()),
            transport: socket,
        }
    }

    /// A session over a stubbed transport. Tests only: there is no event socket.
    #[cfg(test)]
    pub fn with_transport(transport: Arc<dyn ipc::Transport>) -> Self {
        Self {
            transport,
            sockets: None,
        }
    }
}

impl Session for HyprSession {
    fn outputs(&self) -> Result<Vec<crate::monitor::Monitor>> {
        self.transport.monitors()
    }

    /// Every call travels in a *single* request: `[[BATCH]]` would cut the Lua in
    /// half at the first `;`, and one request also means the compositor
    /// reconfigures the outputs once rather than once per screen.
    fn apply(&self, directives: &[String]) -> Result<()> {
        if directives.is_empty() {
            return Ok(());
        }
        self.transport
            .send(&format!("/eval {}", directives.join(" ")))
    }

    /// A dispatcher rather than a configuration change: this is the runtime half
    /// of "main screen".
    fn focus(&self, output: &str) -> Result<()> {
        self.transport
            .send(&format!("dispatch focusmonitor {output}"))
    }

    fn read_input(&self) -> Result<InputConfig> {
        ipc::read_input(self.transport.as_ref())
    }

    fn apply_input(&self, input: &InputConfig) -> Result<()> {
        self.transport
            .send(&format!("/eval {}", self.input_lua(input)))
    }

    fn watch(&self) -> Result<Box<dyn EventStream>> {
        let sockets = self
            .sockets
            .as_ref()
            .context("this session has no event socket")?;
        Ok(Box::new(ipc::Events::connect(&sockets.event_socket())?))
    }
}

impl HyprSession {
    /// The `hl.config{…}` call carrying the input settings.
    ///
    /// Duplicated from the plugin's `input_directives` rather than shared,
    /// because a `Session` does not carry its plugin — and one `format!` is a
    /// cheaper coupling than a back-reference. The test below pins them equal.
    fn input_lua(&self, input: &InputConfig) -> String {
        Hyprland.input_directives(input).join(" ")
    }
}

/// Quotes a value as a Lua string literal.
///
/// Output names come from the compositor and profile rules come from the user:
/// neither is trusted to be free of quotes or backslashes.
pub fn lua_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Header of the generated monitor file.
pub fn monitors_header() -> String {
    t!("emit.header").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Compositor;
    use crate::monitor::{Mode, Rotation, Transform};
    use crate::session::Session;

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
    fn a_call_always_states_every_field_it_owns() {
        // Cumulative rules: omitting a field would let the previous value
        // survive, so defaults are spelled out too.
        assert_eq!(
            Hyprland.output_directive(&out("eDP-1", 0, 0)),
            "hl.monitor({ output = \"eDP-1\", mode = \"1920x1080@60.00\", position = \"0x0\", \
             scale = 1, transform = 0, mirror = \"\", vrr = 0, disabled = false })"
        );
    }

    #[test]
    fn a_call_carries_transform_mirror_and_vrr() {
        let mut o = out("DP-1", 1920, 0);
        o.transform = Transform::new(Rotation::R90, true);
        o.mirror_of = Some("eDP-1".into());
        o.vrr = true;
        o.scale = 1.25;
        assert_eq!(
            Hyprland.output_directive(&o),
            "hl.monitor({ output = \"DP-1\", mode = \"1920x1080@60.00\", position = \"1920x0\", \
             scale = 1.25, transform = 5, mirror = \"eDP-1\", vrr = 1, disabled = false })"
        );
    }

    #[test]
    fn a_disabled_output_only_states_that_it_is_off() {
        let mut o = out("eDP-1", 0, 0);
        o.enabled = false;
        assert_eq!(
            Hyprland.output_directive(&o),
            "hl.monitor({ output = \"eDP-1\", disabled = true })"
        );
    }

    #[test]
    fn an_output_without_a_mode_falls_back_to_preferred() {
        let mut o = out("eDP-1", 0, 0);
        o.mode = None;
        assert!(
            Hyprland
                .output_directive(&o)
                .contains("mode = \"preferred\"")
        );
    }

    #[test]
    fn lua_strings_escape_quotes_and_backslashes() {
        assert_eq!(
            lua_string(r#"desc:Acme "X" \ 1"#),
            r#""desc:Acme \"X\" \\ 1""#
        );
    }

    #[test]
    fn input_settings_travel_as_a_single_nested_config_call() {
        let directives = Hyprland.input_directives(&InputConfig {
            kb_layout: "fr".into(),
            kb_variant: "oss".into(),
            kb_options: "compose:ralt".into(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        });
        assert_eq!(directives.len(), 1);
        let lua = &directives[0];
        assert!(lua.starts_with("hl.config({ input = {"));
        assert!(lua.contains(r#"kb_layout = "fr""#));
        assert!(lua.contains("natural_scroll = false"));
        assert!(lua.contains("touchpad = { natural_scroll = true }"));
    }

    #[test]
    fn a_neighbour_is_required_and_anything_else_is_loaded_by_path() {
        let main = Path::new("/home/u/.config/hypr/hyprland.lua");
        assert_eq!(
            Hyprland.include(main, Path::new("/home/u/.config/hypr/monitors.lua")),
            r#"require("monitors")"#
        );
        assert_eq!(
            Hyprland.include(main, Path::new("/srv/shared/screens.lua")),
            r#"dofile("/srv/shared/screens.lua")"#
        );
    }

    #[test]
    fn an_include_is_recognised_by_module_or_by_file_name() {
        let generated = Path::new("/home/u/.config/hypr/monitors.lua");
        assert!(Hyprland.includes(r#"require("monitors")"#, generated));
        assert!(Hyprland.includes(r#"dofile("/x/monitors.lua")"#, generated));
        assert!(!Hyprland.includes(r#"require("binds")"#, generated));
        assert!(!Hyprland.includes("hl.config({})", generated));
    }

    #[test]
    fn hyprland_drives_live_sessions() {
        assert!(Hyprland.drives_sessions());
    }

    #[test]
    fn a_session_sends_every_call_in_one_eval() {
        // `[[BATCH]]` would cut the Lua in half at the first `;`.
        let wire = std::sync::Arc::new(ipc::fake::FakeTransport::default());
        let session = HyprSession::with_transport(wire.clone());
        session
            .apply(&["hl.monitor({ a })".into(), "hl.monitor({ b })".into()])
            .unwrap();
        assert_eq!(
            wire.sent_commands(),
            vec!["/eval hl.monitor({ a }) hl.monitor({ b })"]
        );
    }

    #[test]
    fn applying_nothing_touches_no_socket() {
        let wire = std::sync::Arc::new(ipc::fake::FakeTransport::default());
        HyprSession::with_transport(wire.clone())
            .apply(&[])
            .unwrap();
        assert!(wire.sent_commands().is_empty());
    }

    #[test]
    fn focus_is_a_dispatcher() {
        let wire = std::sync::Arc::new(ipc::fake::FakeTransport::default());
        HyprSession::with_transport(wire.clone())
            .focus("DP-1")
            .unwrap();
        assert_eq!(wire.sent_commands(), vec!["dispatch focusmonitor DP-1"]);
    }

    #[test]
    fn the_session_and_the_renderer_agree_on_the_input_call() {
        // The one duplication in this file: `apply_input` formats the call itself
        // rather than reaching back for its plugin. This pins them equal.
        let wire = std::sync::Arc::new(ipc::fake::FakeTransport::default());
        let input = InputConfig::default();
        HyprSession::with_transport(wire.clone())
            .apply_input(&input)
            .unwrap();
        let sent = wire.sent_commands();
        assert_eq!(
            sent[0],
            format!("/eval {}", Hyprland.input_directives(&input).join(" "))
        );
    }

    #[test]
    fn live_input_values_are_read_from_the_compositor() {
        let wire = std::sync::Arc::new(ipc::fake::FakeTransport::with_options(&[
            (
                "input:kb_layout",
                r#"{"option":"input:kb_layout","str":"fr","set":true}"#,
            ),
            (
                "input:kb_variant",
                r#"{"option":"input:kb_variant","str":"[[EMPTY]]","set":false}"#,
            ),
            (
                "input:touchpad:natural_scroll",
                r#"{"option":"input:touchpad:natural_scroll","bool":true,"set":true}"#,
            ),
        ]));
        let cfg = HyprSession::with_transport(wire).read_input().unwrap();
        assert_eq!(cfg.kb_layout, "fr");
        assert_eq!(cfg.kb_variant, "", "[[EMPTY]] is not a variant name");
        assert!(cfg.touchpad_natural_scroll);
    }
}
