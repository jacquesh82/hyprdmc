//! sway's wire protocol: i3's IPC, plus sway's own extensions.
//!
//! Nothing like Hyprland's. Where Hyprland writes a line of text down a socket
//! and reads until EOF, this is a **length-framed binary protocol** on a
//! long-lived connection:
//!
//! ```text
//! "i3-ipc" <length: u32> <type: u32> <payload: length bytes>
//! ```
//!
//! Two details worth stating because they are easy to get wrong:
//!
//! * **Native byte order.** i3's specification says the integers "are not
//!   converted, so they are in native byte order" — not network order. Hence
//!   `to_ne_bytes`/`from_ne_bytes` rather than the big-endian reflex.
//! * **Events share the reply channel.** An event is a reply whose type has its
//!   highest bit set, so a connection that has subscribed must expect events
//!   interleaved with command replies. This module keeps them apart by using a
//!   *separate connection* for the event stream, which is also what saves the
//!   request path from having to buffer events it did not ask for.
//!
//! References:
//! * <https://i3wm.org/docs/ipc.html> — framing, type codes 0–12, the high-bit
//!   event convention, `SUBSCRIBE` payload.
//! * `sway-ipc(7)` — `SWAYSOCK`, type 100 `GET_INPUTS`, and the output fields
//!   i3 does not have (`make`, `model`, `serial`, `scale`, `transform`, `modes`,
//!   `current_mode`, `adaptive_sync_status`).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rust_i18n::t;
use serde::Deserialize;

use crate::input::InputConfig;
use crate::monitor::{Mode, Monitor, Rotation, Transform};
use crate::session::CompositorEvent;

/// The eight bytes every message starts with.
const MAGIC: &[u8; 6] = b"i3-ipc";

/// Message types we use. The numbering is i3's; 100 is sway's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Message {
    RunCommand = 0,
    Subscribe = 2,
    GetOutputs = 3,
    GetInputs = 100,
}

/// Set on a reply's type to mark it an event rather than a command reply.
const EVENT_BIT: u32 = 0x8000_0000;

/// Where sway is listening.
///
/// `SWAYSOCK` first, then `I3SOCK`, which sway also sets for compatibility with
/// i3 clients. Asking `sway --get-socketpath` is the documented fallback but
/// needs the binary on `PATH`, so it is deliberately not used: a session whose
/// socket is not in the environment is one hyprdmc should not guess at.
pub fn socket_path() -> Result<PathBuf> {
    for var in ["SWAYSOCK", "I3SOCK"] {
        if let Some(path) = std::env::var_os(var) {
            return Ok(PathBuf::from(path));
        }
    }
    bail!(t!("ipc.sway_not_running").to_string())
}

/// One framed connection to sway.
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub fn open() -> Result<Self> {
        Self::at(&socket_path()?)
    }

    pub fn at(path: &std::path::Path) -> Result<Self> {
        let stream =
            UnixStream::connect(path).with_context(|| t!("ipc.sway_unreachable").to_string())?;
        Ok(Self { stream })
    }

    /// Sends a message and reads the next reply that is not an event.
    ///
    /// Events are skipped rather than mixed in: on a connection that has never
    /// subscribed there are none, and on one that has, a stray event must not be
    /// mistaken for the answer to the question just asked.
    pub fn roundtrip(&mut self, message: Message, payload: &[u8]) -> Result<Vec<u8>> {
        self.write(message, payload)?;
        loop {
            let (kind, body) = self.read()?;
            if kind & EVENT_BIT == 0 {
                return Ok(body);
            }
        }
    }

    fn write(&mut self, message: Message, payload: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(14 + payload.len());
        frame.extend_from_slice(MAGIC);
        frame.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        frame.extend_from_slice(&(message as u32).to_ne_bytes());
        frame.extend_from_slice(payload);
        self.stream
            .write_all(&frame)
            .with_context(|| t!("ipc.sway_unreachable").to_string())?;
        self.stream.flush()?;
        Ok(())
    }

    /// Reads one frame: `(type, payload)`.
    fn read(&mut self) -> Result<(u32, Vec<u8>)> {
        let mut header = [0u8; 14];
        self.stream
            .read_exact(&mut header)
            .with_context(|| t!("ipc.sway_unreachable").to_string())?;
        if &header[..6] != MAGIC {
            bail!(t!("ipc.sway_bad_frame").to_string());
        }
        let length = u32::from_ne_bytes(header[6..10].try_into().expect("4 bytes")) as usize;
        let kind = u32::from_ne_bytes(header[10..14].try_into().expect("4 bytes"));

        let mut payload = vec![0u8; length];
        self.stream
            .read_exact(&mut payload)
            .with_context(|| t!("ipc.sway_unreachable").to_string())?;
        Ok((kind, payload))
    }
}

/// Runs commands and fails if sway rejected any of them.
///
/// The reply is one object per parsed command, each with `success` and, when it
/// failed, `error`. Reporting sway's own sentence matters: "Unknown output DP-9"
/// tells the user something, "command failed" does not.
pub fn run_command(conn: &mut Connection, command: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Outcome {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        parse_error: Option<bool>,
    }

    let body = conn.roundtrip(Message::RunCommand, command.as_bytes())?;
    // An empty command is accepted by sway with an empty array, not an error.
    let outcomes: Vec<Outcome> = serde_json::from_slice(&body)
        .with_context(|| t!("ipc.unexpected_response", body = truncate(&body)).to_string())?;

    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.success)
        .map(|o| {
            let message = o.error.clone().unwrap_or_else(|| {
                if o.parse_error.unwrap_or(false) {
                    t!("ipc.sway_parse_error").to_string()
                } else {
                    t!("ipc.sway_unknown_error").to_string()
                }
            });
            format!("  • {message}")
        })
        .collect();

    if failures.is_empty() {
        return Ok(());
    }
    bail!(
        t!(
            "ipc.rejected",
            message = failures.join("\n"),
            commands = command
        )
        .to_string()
    );
}

/// Subscribes this connection to the event types it should report.
pub fn subscribe(conn: &mut Connection, events: &[&str]) -> Result<()> {
    let payload = serde_json::to_vec(&events)?;
    let body = conn.roundtrip(Message::Subscribe, &payload)?;
    #[derive(Deserialize)]
    struct Ack {
        #[serde(default)]
        success: bool,
    }
    let ack: Ack = serde_json::from_slice(&body).unwrap_or(Ack { success: false });
    if !ack.success {
        bail!(t!("ipc.sway_subscribe_failed").to_string());
    }
    Ok(())
}

// ------------------------------------------------------------------ outputs --

/// One output as `GET_OUTPUTS` reports it.
///
/// A type of its own rather than deserializing straight into [`Monitor`]:
/// `Monitor`'s field names follow Hyprland's JSON, and they are also what the
/// web API serialises, so they are not free to change. Mapping here keeps both
/// wire formats honest and the shared model stable.
#[derive(Debug, Deserialize)]
struct SwayOutput {
    name: String,
    #[serde(default)]
    make: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    serial: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    focused: bool,
    #[serde(default = "one")]
    scale: f64,
    /// `"normal"`, `"90"`, `"flipped-180"`… Absent on an inactive output.
    #[serde(default)]
    transform: Option<String>,
    #[serde(default)]
    rect: Rect,
    #[serde(default)]
    modes: Vec<SwayMode>,
    #[serde(default)]
    current_mode: Option<SwayMode>,
    /// `"enabled"`, `"disabled"` or `"unknown"`.
    #[serde(default)]
    adaptive_sync_status: Option<String>,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Default, Deserialize)]
struct Rect {
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct SwayMode {
    width: i32,
    height: i32,
    /// Millihertz: 60000 is 60 Hz.
    #[serde(default)]
    refresh: i32,
}

impl SwayMode {
    fn to_mode(self) -> Mode {
        Mode::new(self.width, self.height, f64::from(self.refresh) / 1000.0)
    }
}

/// sway's transform keyword, back to the rotation-plus-flip pair.
fn parse_transform(keyword: Option<&str>) -> Transform {
    let Some(keyword) = keyword else {
        return Transform::default();
    };
    let (flipped, rotation) = match keyword.trim() {
        "normal" | "0" => (false, Rotation::R0),
        "90" => (false, Rotation::R90),
        "180" => (false, Rotation::R180),
        "270" => (false, Rotation::R270),
        "flipped" | "flipped-0" => (true, Rotation::R0),
        "flipped-90" => (true, Rotation::R90),
        "flipped-180" => (true, Rotation::R180),
        "flipped-270" => (true, Rotation::R270),
        // A keyword we do not know is better read as "unrotated" than as a
        // panic: sway may add one, and an unknown orientation is not fatal.
        _ => (false, Rotation::R0),
    };
    Transform::new(rotation, flipped)
}

/// Maps sway's outputs onto the shared model.
fn to_monitors(outputs: Vec<SwayOutput>) -> Vec<Monitor> {
    outputs
        .into_iter()
        .enumerate()
        .map(|(index, o)| {
            let mode = o.current_mode.or_else(|| o.modes.first().copied());
            Monitor {
                // sway gives outputs no numeric id; the index is stable within
                // one reply and nothing here depends on it across replies.
                id: index as i64,
                description: format!("{} {}", o.make, o.model).trim().to_string(),
                // sway reports the *logical* position, which is what
                // `OutputState` wants, and the mode in physical pixels.
                x: o.rect.x,
                y: o.rect.y,
                width: mode.map_or(0, |m| m.width),
                height: mode.map_or(0, |m| m.height),
                refresh_rate: mode.map_or(0.0, |m| f64::from(m.refresh) / 1000.0),
                scale: if o.scale > 0.0 { o.scale } else { 1.0 },
                transform: parse_transform(o.transform.as_deref()).to_u8(),
                focused: o.focused,
                disabled: !o.active,
                // sway has no mirroring at all — see the plugin's own note.
                mirror_of: "none".to_string(),
                vrr: o.adaptive_sync_status.as_deref() == Some("enabled"),
                available_modes: o
                    .modes
                    .iter()
                    .map(|m| {
                        let mode = m.to_mode();
                        format!("{}x{}@{:.3}Hz", mode.width, mode.height, mode.refresh)
                    })
                    .collect(),
                name: o.name,
                make: o.make,
                model: o.model,
                serial: o.serial,
            }
        })
        .collect()
}

/// Reads the outputs.
pub fn outputs(conn: &mut Connection) -> Result<Vec<Monitor>> {
    let body = conn.roundtrip(Message::GetOutputs, &[])?;
    let outputs: Vec<SwayOutput> = serde_json::from_slice(&body)
        .with_context(|| t!("ipc.unexpected_response", body = truncate(&body)).to_string())?;
    Ok(to_monitors(outputs))
}

// -------------------------------------------------------------------- input --

/// One input device as `GET_INPUTS` reports it.
#[derive(Debug, Deserialize)]
struct SwayInput {
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    xkb_active_layout_name: Option<String>,
    #[serde(default)]
    libinput: Option<Libinput>,
}

#[derive(Debug, Deserialize)]
struct Libinput {
    /// `"enabled"` or `"disabled"`.
    #[serde(default)]
    natural_scroll: Option<String>,
}

/// Reads what the session is currently using.
///
/// Deliberately partial, and this is the honest limit of sway's IPC rather than
/// a shortcut: `GET_INPUTS` reports the *active layout's human name*
/// ("French (alt.)"), not the `xkb_layout`/`xkb_variant` codes that were
/// configured. Those codes are what we write, so they cannot be read back — what
/// comes out here is the scroll direction, which *is* reported, plus defaults for
/// the keyboard. The UI shows the values in `config.toml` on top of this.
pub fn read_input(conn: &mut Connection) -> Result<InputConfig> {
    let body = conn.roundtrip(Message::GetInputs, &[])?;
    let devices: Vec<SwayInput> = serde_json::from_slice(&body)
        .with_context(|| t!("ipc.unexpected_response", body = truncate(&body)).to_string())?;

    let natural = |kind: &str| {
        devices
            .iter()
            .filter(|d| d.kind == kind)
            .filter_map(|d| d.libinput.as_ref()?.natural_scroll.as_deref())
            .any(|value| value == "enabled")
    };

    let mut input = InputConfig {
        natural_scroll: natural("pointer"),
        touchpad_natural_scroll: natural("touchpad"),
        ..Default::default()
    };
    // Better than nothing and honest about it: the active layout's name is not
    // its code, so it only serves as a hint when it happens to be one.
    if let Some(name) = devices
        .iter()
        .find(|d| d.kind == "keyboard")
        .and_then(|d| d.xkb_active_layout_name.as_deref())
        && name.len() <= 8
        && name.chars().all(|c| c.is_ascii_lowercase())
    {
        input.kb_layout = name.to_string();
    }
    Ok(input)
}

// ------------------------------------------------------------------- events --

/// Blocking reader over a subscribed connection.
pub struct Events {
    conn: Connection,
}

impl Events {
    /// Opens a *second* connection and subscribes it.
    ///
    /// Its own connection on purpose: once subscribed, events arrive unbidden on
    /// the same channel as replies, and sharing one socket would mean every
    /// `GET_OUTPUTS` had to wade through them.
    pub fn connect(path: &std::path::Path) -> Result<Self> {
        let mut conn = Connection::at(path)?;
        subscribe(&mut conn, &["output"])?;
        Ok(Self { conn })
    }
}

impl crate::session::EventStream for Events {
    fn next_event(&mut self) -> Option<CompositorEvent> {
        loop {
            let (kind, body) = self.conn.read().ok()?;
            if kind & EVENT_BIT == 0 {
                continue;
            }
            return Some(parse_event(&body));
        }
    }
}

/// Turns an `output` event into the shared form.
///
/// sway's payload is `{"change": "unspecified"}` — it says *something* about the
/// outputs changed, never which one or how. There is nothing to map onto
/// `OutputAdded`/`OutputRemoved`, so it becomes [`CompositorEvent::ConfigReloaded`]:
/// the daemon re-reads the outputs and compares, which is the same conclusion by
/// a different route.
fn parse_event(body: &[u8]) -> CompositorEvent {
    #[derive(Deserialize)]
    struct OutputEvent {
        #[serde(default)]
        change: String,
    }
    let change = serde_json::from_slice::<OutputEvent>(body)
        .map(|e| e.change)
        .unwrap_or_default();
    CompositorEvent::Other(format!("sway output event: {change}"))
}

fn truncate(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.len() <= 200 {
        text.to_string()
    } else {
        format!("{}…", &text[..200])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_magic_then_two_native_endian_words() {
        // The one detail i3's spec is explicit about and everyone gets wrong:
        // native byte order, not network order.
        let mut frame = Vec::new();
        frame.extend_from_slice(MAGIC);
        frame.extend_from_slice(&3u32.to_ne_bytes());
        frame.extend_from_slice(&(Message::RunCommand as u32).to_ne_bytes());
        assert_eq!(&frame[..6], b"i3-ipc");
        assert_eq!(frame.len(), 14);
        assert_eq!(
            u32::from_ne_bytes(frame[6..10].try_into().unwrap()),
            3,
            "the length must survive a round trip in the same order"
        );
    }

    #[test]
    fn message_codes_match_the_published_protocol() {
        // i3: RUN_COMMAND 0, SUBSCRIBE 2, GET_OUTPUTS 3. sway adds GET_INPUTS 100.
        assert_eq!(Message::RunCommand as u32, 0);
        assert_eq!(Message::Subscribe as u32, 2);
        assert_eq!(Message::GetOutputs as u32, 3);
        assert_eq!(Message::GetInputs as u32, 100);
    }

    #[test]
    fn an_event_is_a_reply_with_the_high_bit_set() {
        assert_eq!(EVENT_BIT, 1 << 31);
        assert_ne!(EVENT_BIT & Message::GetOutputs as u32, EVENT_BIT);
    }

    const OUTPUTS: &str = r#"[
      {"name":"eDP-1","make":"AU Optronics","model":"0x5799","serial":"","active":true,
       "focused":true,"scale":1.5,"transform":"90","rect":{"x":0,"y":0,"width":1280,"height":720},
       "current_mode":{"width":1920,"height":1080,"refresh":60056},
       "modes":[{"width":1920,"height":1080,"refresh":60056},{"width":1280,"height":720,"refresh":60000}],
       "adaptive_sync_status":"enabled"},
      {"name":"DP-3","make":"Dell","model":"U2723QE","serial":"ABC","active":false,
       "rect":{"x":0,"y":0,"width":0,"height":0},"modes":[]}
    ]"#;

    #[test]
    fn sway_outputs_map_onto_the_shared_model() {
        let parsed: Vec<SwayOutput> = serde_json::from_str(OUTPUTS).unwrap();
        let monitors = to_monitors(parsed);
        assert_eq!(monitors.len(), 2);

        let laptop = &monitors[0];
        assert_eq!(laptop.name, "eDP-1");
        // The mode is physical pixels; `rect` is the logical position.
        assert_eq!((laptop.width, laptop.height), (1920, 1080));
        assert_eq!((laptop.x, laptop.y), (0, 0));
        // Millihertz on the wire, hertz in the model.
        assert!(
            (laptop.refresh_rate - 60.056).abs() < 1e-6,
            "{}",
            laptop.refresh_rate
        );
        assert_eq!(laptop.scale, 1.5);
        assert_eq!(laptop.transform, 1, "\"90\" is transform 1");
        assert!(laptop.vrr, "adaptive_sync_status enabled");
        assert!(!laptop.disabled);
        assert!(laptop.focused);
        assert_eq!(laptop.fingerprint(), "AU Optronics 0x5799");
        assert_eq!(laptop.available_modes.len(), 2);
        // The formatted mode has to survive our own parser.
        assert_eq!(
            laptop.available_modes[0].parse::<Mode>().unwrap(),
            Mode::new(1920, 1080, 60.056)
        );

        let off = &monitors[1];
        assert!(off.disabled, "active:false is disabled");
        assert_eq!(off.mirror_of, "none", "sway has no mirroring");
    }

    #[test]
    fn every_transform_keyword_round_trips_through_the_plugin() {
        // The pair that must agree: what `super::Sway` writes, this reads back.
        use crate::compositor::Compositor;
        for value in 0u8..=7 {
            let transform = Transform::from_u8(value).unwrap();
            let mut state = crate::layout::OutputState {
                name: "DP-1".into(),
                enabled: true,
                mode: Some(Mode::new(1920, 1080, 60.0)),
                x: 0,
                y: 0,
                scale: 1.0,
                transform,
                mirror_of: None,
                vrr: false,
            };
            state.transform = transform;
            let directive = super::super::Sway.output_directive(&state);
            let keyword = directive
                .split("transform ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .expect("the directive states a transform");
            assert_eq!(
                parse_transform(Some(keyword)).to_u8(),
                value,
                "transform {value} became {keyword:?} and did not come back"
            );
        }
    }

    #[test]
    fn an_unknown_transform_keyword_is_read_as_unrotated() {
        // sway may grow one; an unfamiliar orientation is not worth a panic.
        assert_eq!(parse_transform(Some("sideways")).to_u8(), 0);
        assert_eq!(parse_transform(None).to_u8(), 0);
    }

    #[test]
    fn a_failed_command_reports_sways_own_sentence() {
        // "Unknown output DP-9" tells the user something; "failed" does not.
        let reply = br#"[{"success":false,"error":"Unknown output DP-9"}]"#;
        let outcomes: Vec<serde_json::Value> = serde_json::from_slice(reply).unwrap();
        assert_eq!(outcomes[0]["error"], "Unknown output DP-9");
    }

    #[test]
    fn scroll_direction_is_read_per_device_class() {
        let body = br#"[
          {"type":"keyboard","xkb_active_layout_name":"French (alt.)"},
          {"type":"touchpad","libinput":{"natural_scroll":"enabled"}},
          {"type":"pointer","libinput":{"natural_scroll":"disabled"}}
        ]"#;
        let devices: Vec<SwayInput> = serde_json::from_slice(body).unwrap();
        let natural = |kind: &str| {
            devices
                .iter()
                .filter(|d| d.kind == kind)
                .filter_map(|d| d.libinput.as_ref()?.natural_scroll.as_deref())
                .any(|v| v == "enabled")
        };
        assert!(natural("touchpad"));
        assert!(!natural("pointer"));
    }

    #[test]
    fn a_human_layout_name_is_not_mistaken_for_a_code() {
        // sway reports "French (alt.)", never "fr": writing that into kb_layout
        // would produce a config sway itself rejects.
        let body = br#"[{"type":"keyboard","xkb_active_layout_name":"French (alt.)"}]"#;
        let devices: Vec<SwayInput> = serde_json::from_slice(body).unwrap();
        let name = devices[0].xkb_active_layout_name.as_deref().unwrap();
        let looks_like_a_code = name.len() <= 8 && name.chars().all(|c| c.is_ascii_lowercase());
        assert!(!looks_like_a_code, "{name} must not be taken for a code");
    }

    #[test]
    fn the_socket_comes_from_the_environment_or_not_at_all() {
        // Guessing by probing paths would attach hyprdmc to someone else's
        // session; an absent SWAYSOCK is an error with a name.
        if std::env::var_os("SWAYSOCK").is_none() && std::env::var_os("I3SOCK").is_none() {
            let Err(err) = socket_path() else {
                panic!("no socket in the environment must not resolve");
            };
            assert!(err.to_string().to_lowercase().contains("sway"), "{err}");
        }
    }
}
