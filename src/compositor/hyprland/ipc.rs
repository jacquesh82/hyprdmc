//! Hyprland's wire protocol, without going through `hyprctl`.
//!
//! * `.socket.sock`  — requests/commands. One connection per command: write
//!   the command, read the response until EOF.
//! * `.socket2.sock` — event stream. Long-lived connection, lines shaped like
//!   `EVENT>>DATA\n`.
//!
//! This is the *transport* only: which bytes go down the socket and what comes
//! back. What to ask for lives one level up, in [`super::HyprSession`], and the
//! split is what lets tests stub the wire without a compositor. See
//! `docs/writing-a-plugin.md` for the shape a new compositor follows.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rust_i18n::t;

use crate::input::InputConfig;
use crate::monitor::Monitor;
use crate::session::CompositorEvent;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// One request down Hyprland's command socket, one reply back.
///
/// A trait with a single method so tests can stub the wire; [`HyprSocket`] is
/// the real thing.
pub trait Transport: Send + Sync {
    /// Sends a raw command and returns the textual response.
    fn query(&self, cmd: &str) -> Result<String>;

    /// Sends one request and fails if Hyprland rejected it.
    fn send(&self, request: &str) -> Result<()> {
        if request.trim().is_empty() {
            return Ok(());
        }
        let reply = self.query(request)?;
        check_ok(&reply, &[request.to_string()])
    }

    /// Outputs as `j/monitors all` reports them, disabled ones included.
    fn monitors(&self) -> Result<Vec<Monitor>> {
        let raw = self.query("j/monitors all")?;
        serde_json::from_str(&raw)
            .with_context(|| t!("ipc.unexpected_response", body = truncate(&raw)).to_string())
    }
}

/// Hyprland replies `ok` for every accepted command; anything else is an
/// error message that must be surfaced to the user as-is.
fn check_ok(reply: &str, cmds: &[String]) -> Result<()> {
    let trimmed = reply.trim();
    if trimmed.lines().all(|l| matches!(l.trim(), "" | "ok")) {
        return Ok(());
    }
    bail!(
        t!(
            "ipc.rejected",
            message = trimmed,
            commands = cmds.join("\n  ")
        )
        .to_string()
    );
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 200 {
        s.to_string()
    } else {
        format!("{}…", &s[..200])
    }
}

/// Locates the current Hyprland instance directory.
pub fn instance_dir() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", unsafe { libc_getuid() })));
    let base = runtime.join("hypr");

    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let dir = base.join(&sig);
        if dir.is_dir() {
            return Ok(dir);
        }
        bail!(
            t!(
                "ipc.stale_signature",
                signature = sig,
                path = dir.display().to_string()
            )
            .to_string()
        );
    }

    // Outside a Hyprland session (systemd --user, for instance): if there is
    // only one instance, use it.
    let mut instances: Vec<PathBuf> = std::fs::read_dir(&base)
        .with_context(|| t!("ipc.not_running", path = base.display().to_string()).to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(".socket.sock").exists())
        .collect();
    instances.sort();

    match instances.len() {
        0 => bail!(t!("ipc.no_instance", path = base.display().to_string()).to_string()),
        1 => Ok(instances.remove(0)),
        n => bail!(
            t!(
                "ipc.many_instances",
                count = n,
                path = base.display().to_string()
            )
            .to_string()
        ),
    }
}

// Avoids depending on the `libc` crate for a single call.
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Real transport: Hyprland's UNIX sockets.
#[derive(Debug, Clone)]
pub struct HyprSocket {
    dir: PathBuf,
}

impl HyprSocket {
    pub fn connect() -> Result<Self> {
        Ok(Self {
            dir: instance_dir()?,
        })
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn request_socket(&self) -> PathBuf {
        self.dir.join(".socket.sock")
    }

    pub fn event_socket(&self) -> PathBuf {
        self.dir.join(".socket2.sock")
    }
}

impl Transport for HyprSocket {
    fn query(&self, cmd: &str) -> Result<String> {
        let path = self.request_socket();
        let mut stream =
            UnixStream::connect(&path).with_context(|| t!("ipc.unreachable").to_string())?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream
            .write_all(cmd.as_bytes())
            .with_context(|| t!("ipc.send_failed", cmd = cmd).to_string())?;
        stream.flush()?;

        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .with_context(|| t!("ipc.read_failed", cmd = cmd).to_string())?;
        String::from_utf8(buf)
            .map_err(|e| anyhow!(t!("ipc.invalid_utf8", error = e.to_string()).to_string()))
    }
}

/// Parses a line of `.socket2.sock`, shaped like `EVENT>>DATA`.
pub fn parse_event(line: &str) -> Option<CompositorEvent> {
    let (event, data) = line.split_once(">>")?;
    Some(match event {
        // `monitoraddedv2` carries "ID,NAME,DESCRIPTION"; we only keep the name.
        "monitoraddedv2" => {
            let mut parts = data.splitn(3, ',');
            let _id = parts.next();
            CompositorEvent::OutputAdded(parts.next().unwrap_or_default().to_string())
        }
        "monitoradded" => CompositorEvent::OutputAdded(data.to_string()),
        "monitorremoved" | "monitorremovedv2" => {
            // The v2 variant carries "ID,NAME,DESCRIPTION".
            let name = if event.ends_with("v2") {
                data.split(',').nth(1).unwrap_or_default().to_string()
            } else {
                data.to_string()
            };
            CompositorEvent::OutputRemoved(name)
        }
        "configreloaded" => CompositorEvent::ConfigReloaded,
        _ => CompositorEvent::Other(line.to_string()),
    })
}

/// Blocking reader over `.socket2.sock`.
///
/// Blocking on purpose: see [`crate::session::EventStream`]. Hyprland's stream
/// is one event per line, so a `BufReader` is the whole implementation.
pub struct Events {
    lines: std::io::Lines<std::io::BufReader<UnixStream>>,
}

impl Events {
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream =
            UnixStream::connect(socket).with_context(|| t!("ipc.unreachable").to_string())?;
        Ok(Self {
            lines: std::io::BufRead::lines(std::io::BufReader::new(stream)),
        })
    }
}

impl crate::session::EventStream for Events {
    fn next_event(&mut self) -> Option<CompositorEvent> {
        // A line we cannot parse is not the end of the stream: skip it and keep
        // reading, or one odd event would stop hotplug for the whole session.
        loop {
            match self.lines.next() {
                Some(Ok(line)) => {
                    if let Some(event) = parse_event(&line) {
                        return Some(event);
                    }
                }
                Some(Err(_)) | None => return None,
            }
        }
    }
}

/// The keyboard and pointer settings Hyprland currently reports.
///
/// The live state is the source of truth: the user may well have set `kb_layout`
/// by hand in `hyprland.lua` long before hyprdmc existed, and the UI must show
/// that rather than a default we invented.
pub fn read_input(wire: &dyn Transport) -> Result<InputConfig> {
    Ok(InputConfig {
        kb_layout: get_string(wire, "input:kb_layout")?,
        kb_variant: get_string(wire, "input:kb_variant")?,
        kb_options: get_string(wire, "input:kb_options")?,
        natural_scroll: get_bool(wire, "input:natural_scroll")?,
        touchpad_natural_scroll: get_bool(wire, "input:touchpad:natural_scroll")?,
    })
}

/// Reads a string option through `getoption`.
fn get_string(backend: &dyn Transport, option: &str) -> Result<String> {
    let value = get_option(backend, option)?;
    Ok(value
        .get("str")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        // Hyprland reports an unset string option as the literal
        // `[[EMPTY]]`, which is not something to show in a text field.
        .replace("[[EMPTY]]", "")
        .to_string())
}

/// Reads a boolean option through `getoption`.
///
/// Hyprland answers with `bool` for these, but older versions used `int`;
/// both are accepted so a version bump does not silently read as `false`.
fn get_bool(backend: &dyn Transport, option: &str) -> Result<bool> {
    let value = get_option(backend, option)?;
    Ok(value
        .get("bool")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| value.get("int").and_then(|v| v.as_i64()).map(|i| i != 0))
        .unwrap_or(false))
}

fn get_option(wire: &dyn Transport, option: &str) -> Result<serde_json::Value> {
    let raw = wire.query(&format!("j/getoption {option}"))?;
    serde_json::from_str(&raw)
        .with_context(|| t!("input.unreadable_option", option = option).to_string())
}

#[cfg(test)]
pub mod fake {
    //! Test backend: replays canned responses and records the commands sent.

    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct FakeTransport {
        pub monitors_json: Mutex<String>,
        pub sent: Mutex<Vec<String>>,
        pub fail_with: Mutex<Option<String>>,
        /// Successive responses to `j/monitors`, to simulate a state that
        /// takes a few reads to converge. The last one keeps being served
        /// afterwards.
        pending_states: Mutex<Vec<String>>,
        /// Canned `j/getoption` replies, keyed by option name.
        options: Mutex<Vec<(String, String)>>,
    }

    impl FakeTransport {
        pub fn with_monitors(json: &str) -> Self {
            Self {
                monitors_json: Mutex::new(json.to_string()),
                ..Default::default()
            }
        }

        /// Backend that answers `j/getoption` from a fixed table.
        pub fn with_options(options: &[(&str, &str)]) -> Self {
            Self {
                options: Mutex::new(
                    options
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                        .collect(),
                ),
                ..Default::default()
            }
        }

        /// Backend that returns `stale` for `repeats` reads before returning
        /// `settled` — reproduces Hyprland's application latency.
        pub fn settling_after(repeats: usize, stale: &str, settled: &str) -> Self {
            let mut states = vec![stale.to_string(); repeats];
            states.reverse();
            Self {
                monitors_json: Mutex::new(settled.to_string()),
                pending_states: Mutex::new(states),
                ..Default::default()
            }
        }

        pub fn sent_commands(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }

        pub fn monitor_reads(&self) -> usize {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("j/monitors"))
                .count()
        }
    }

    /// A [`Session`](crate::session::Session) over a stubbed wire.
    pub struct FakeSession {
        pub(super) inner: super::super::HyprSession,
        pub(super) wire: Arc<FakeTransport>,
    }

    impl FakeSession {
        pub fn with_monitors(json: &str) -> Self {
            Self::build(Arc::new(FakeTransport::with_monitors(json)))
        }

        pub fn with_options(options: &[(&str, &str)]) -> Self {
            Self::build(Arc::new(FakeTransport::with_options(options)))
        }

        pub fn settling_after(repeats: usize, stale: &str, settled: &str) -> Self {
            Self::build(Arc::new(FakeTransport::settling_after(
                repeats, stale, settled,
            )))
        }

        pub fn sent_commands(&self) -> Vec<String> {
            self.wire.sent_commands()
        }

        pub fn monitor_reads(&self) -> usize {
            self.wire.monitor_reads()
        }
    }

    impl crate::session::Session for FakeSession {
        fn outputs(&self) -> Result<Vec<Monitor>> {
            self.inner.outputs()
        }
        fn apply(&self, directives: &[String]) -> Result<()> {
            self.inner.apply(directives)
        }
        fn focus(&self, output: &str) -> Result<()> {
            self.inner.focus(output)
        }
        fn read_input(&self) -> Result<InputConfig> {
            self.inner.read_input()
        }
        fn apply_input(&self, input: &InputConfig) -> Result<()> {
            self.inner.apply_input(input)
        }
        fn watch(&self) -> Result<Box<dyn crate::session::EventStream>> {
            self.inner.watch()
        }
    }

    impl Transport for FakeTransport {
        fn query(&self, cmd: &str) -> Result<String> {
            self.sent.lock().unwrap().push(cmd.to_string());
            if let Some(err) = self.fail_with.lock().unwrap().clone() {
                return Ok(err);
            }
            if cmd.starts_with("j/monitors") {
                if let Some(stale) = self.pending_states.lock().unwrap().pop() {
                    return Ok(stale);
                }
                return Ok(self.monitors_json.lock().unwrap().clone());
            }
            if let Some(option) = cmd.strip_prefix("j/getoption ") {
                let options = self.options.lock().unwrap();
                let found = options.iter().find(|(name, _)| name == option.trim());
                // An option nobody stubbed answers like Hyprland does for one
                // that was never set, rather than blowing up the test.
                return Ok(found.map_or_else(
                    || {
                        format!(
                            r#"{{"option":"{}","str":"[[EMPTY]]","set":false}}"#,
                            option.trim()
                        )
                    },
                    |(_, reply)| reply.clone(),
                ));
            }
            Ok("ok".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_monitor_added_v2() {
        assert_eq!(
            parse_event("monitoraddedv2>>1,DP-1,Dell Inc. U2723QE ABC"),
            Some(CompositorEvent::OutputAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_added_v1() {
        assert_eq!(
            parse_event("monitoradded>>DP-1"),
            Some(CompositorEvent::OutputAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_removed_both_variants() {
        assert_eq!(
            parse_event("monitorremoved>>DP-1"),
            Some(CompositorEvent::OutputRemoved("DP-1".into()))
        );
        assert_eq!(
            parse_event("monitorremovedv2>>1,DP-1,Dell"),
            Some(CompositorEvent::OutputRemoved("DP-1".into()))
        );
    }

    #[test]
    fn unrelated_events_are_ignored_by_the_daemon() {
        assert!(!parse_event("workspace>>2").unwrap().affects_outputs());
        assert!(parse_event("monitoradded>>DP-1").unwrap().affects_outputs());
    }

    #[test]
    fn malformed_lines_yield_nothing() {
        assert_eq!(parse_event("no separator"), None);
    }

    #[test]
    fn a_request_is_sent_verbatim() {
        // The transport does not compose requests: whatever the plugin phrased
        // is what goes on the wire, `;` and all — a `[[BATCH]]` here would cut
        // Lua in half at the first one.
        let fake = fake::FakeTransport::default();
        let request = "/eval hl.monitor({ output = \"A\" }) hl.monitor({ output = \"B\" })";
        fake.send(request).unwrap();
        assert_eq!(fake.sent_commands(), vec![request]);
    }

    #[test]
    fn an_empty_request_touches_no_socket() {
        let fake = fake::FakeTransport::default();
        fake.send("").unwrap();
        fake.send("   ").unwrap();
        assert!(fake.sent_commands().is_empty());
    }

    #[test]
    fn rejected_configuration_surfaces_hyprland_message() {
        let cmds = vec!["hl.monitor({ output = \"DP-1\", mode = \"bogus\" })".to_string()];
        let err = check_ok("error: hl.monitor: error applying field 'mode'", &cmds).unwrap_err();
        assert!(err.to_string().contains("error applying field"));
        assert!(check_ok("ok", &cmds).is_ok());
        assert!(check_ok("ok\n\n\nok", &cmds).is_ok());
    }
}

#[cfg(test)]
impl fake::FakeSession {
    /// A session whose wire is stubbed, with the recorder still reachable.
    ///
    /// Wraps a *real* [`super::HyprSession`] rather than reimplementing it, so
    /// tests that assert on the bytes sent are asserting the production
    /// formatting and not a double's imitation of it.
    fn build(wire: std::sync::Arc<fake::FakeTransport>) -> Self {
        Self {
            inner: super::HyprSession::with_transport(wire.clone()),
            wire,
        }
    }
}
