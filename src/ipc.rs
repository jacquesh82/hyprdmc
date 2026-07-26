//! Talks to Hyprland over its two UNIX sockets, without going through
//! `hyprctl`.
//!
//! * `.socket.sock`  — requests/commands. One connection per command: write
//!   the command, read the response until EOF.
//! * `.socket2.sock` — event stream. Long-lived connection, lines shaped like
//!   `EVENT>>DATA\n`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rust_i18n::t;

use crate::monitor::Monitor;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Abstraction over the transport to Hyprland.
///
/// The rest of the program only depends on this trait, which lets us test
/// the business logic without a running compositor.
pub trait HyprBackend: Send + Sync {
    /// Sends a raw command and returns the textual response.
    fn query(&self, cmd: &str) -> Result<String>;

    /// Applies several commands in a single Hyprland transaction.
    ///
    /// Beware: Hyprland splits a batch on `;`, so this is only usable for
    /// commands that cannot contain one — Lua code notably cannot go
    /// through here.
    fn batch(&self, cmds: &[String]) -> Result<String> {
        if cmds.is_empty() {
            return Ok(String::new());
        }
        self.query(&format!("[[BATCH]]{}", cmds.join(";")))
    }

    /// State of all outputs, including the disabled ones.
    fn monitors(&self) -> Result<Vec<Monitor>> {
        let raw = self.query("j/monitors all")?;
        serde_json::from_str(&raw)
            .with_context(|| t!("ipc.unexpected_response", body = truncate(&raw)).to_string())
    }

    /// Applies a series of `hl.monitor{…}` calls and checks that none of
    /// them were rejected.
    ///
    /// Since Hyprland 0.55 the configuration is Lua and `keyword` is
    /// refused outright ("keyword can't work with non-legacy parsers"), so
    /// everything goes through `eval`. All the calls travel in a *single*
    /// request: `[[BATCH]]` would cut the Lua in half at the first `;`, and
    /// one request also means the compositor reconfigures the outputs once
    /// rather than once per screen.
    fn set_monitors(&self, calls: &[String]) -> Result<()> {
        if calls.is_empty() {
            return Ok(());
        }
        let reply = self.query(&format!("/eval {}", calls.join(" ")))?;
        check_ok(&reply, calls)
    }

    /// Runs one Lua statement and fails if the compositor rejected it.
    ///
    /// Same road as [`Self::set_monitors`], for the settings that are not
    /// monitors — keyboard and pointer (see [`crate::input`]).
    fn eval(&self, lua: &str) -> Result<()> {
        let reply = self.query(&format!("/eval {lua}"))?;
        check_ok(&reply, &[lua.to_string()])
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

impl HyprBackend for HyprSocket {
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

/// Events from socket 2 that we care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprEvent {
    /// An output appeared (connector name).
    MonitorAdded(String),
    /// An output disappeared (connector name).
    MonitorRemoved(String),
    /// The configuration was reloaded: the state may have changed under us.
    ConfigReloaded,
    /// Everything else, kept for verbose logging.
    Other(String),
}

impl HyprEvent {
    /// Parses a line shaped like `EVENT>>DATA`.
    pub fn parse(line: &str) -> Option<Self> {
        let (event, data) = line.split_once(">>")?;
        Some(match event {
            // `monitoraddedv2` carries "ID,NAME,DESCRIPTION"; we only keep the name.
            "monitoraddedv2" => {
                let mut parts = data.splitn(3, ',');
                let _id = parts.next();
                let name = parts.next().unwrap_or_default().to_string();
                HyprEvent::MonitorAdded(name)
            }
            "monitoradded" => HyprEvent::MonitorAdded(data.to_string()),
            "monitorremoved" | "monitorremovedv2" => {
                // The v2 variant carries "ID,NAME,DESCRIPTION".
                let name = if event.ends_with("v2") {
                    data.split(',').nth(1).unwrap_or_default().to_string()
                } else {
                    data.to_string()
                };
                HyprEvent::MonitorRemoved(name)
            }
            "configreloaded" => HyprEvent::ConfigReloaded,
            _ => HyprEvent::Other(line.to_string()),
        })
    }

    /// An event that should trigger a profile re-evaluation.
    pub fn affects_monitors(&self) -> bool {
        matches!(
            self,
            HyprEvent::MonitorAdded(_) | HyprEvent::MonitorRemoved(_)
        )
    }
}

/// Opens the event stream and invokes `on_event` for every line.
///
/// The function only returns once the socket closes (Hyprland restarting) or
/// `on_event` returns an error.
pub async fn stream_events<F>(socket: &Path, mut on_event: F) -> Result<()>
where
    F: FnMut(HyprEvent) -> Result<()>,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixStream as AsyncUnixStream;

    let stream = AsyncUnixStream::connect(socket)
        .await
        .with_context(|| t!("ipc.unreachable").to_string())?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(event) = HyprEvent::parse(&line) {
            on_event(event)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod fake {
    //! Test backend: replays canned responses and records the commands sent.

    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeBackend {
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

    impl FakeBackend {
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

    impl HyprBackend for FakeBackend {
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
            HyprEvent::parse("monitoraddedv2>>1,DP-1,Dell Inc. U2723QE ABC"),
            Some(HyprEvent::MonitorAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_added_v1() {
        assert_eq!(
            HyprEvent::parse("monitoradded>>DP-1"),
            Some(HyprEvent::MonitorAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_removed_both_variants() {
        assert_eq!(
            HyprEvent::parse("monitorremoved>>DP-1"),
            Some(HyprEvent::MonitorRemoved("DP-1".into()))
        );
        assert_eq!(
            HyprEvent::parse("monitorremovedv2>>1,DP-1,Dell"),
            Some(HyprEvent::MonitorRemoved("DP-1".into()))
        );
    }

    #[test]
    fn unrelated_events_are_ignored_by_the_daemon() {
        let ev = HyprEvent::parse("workspace>>2").unwrap();
        assert!(!ev.affects_monitors());
        assert!(
            HyprEvent::parse("monitoradded>>DP-1")
                .unwrap()
                .affects_monitors()
        );
    }

    #[test]
    fn malformed_lines_yield_nothing() {
        assert_eq!(HyprEvent::parse("no separator"), None);
    }

    #[test]
    fn batch_joins_commands() {
        let fake = fake::FakeBackend::default();
        fake.batch(&["dispatch A".into(), "dispatch B".into()])
            .unwrap();
        assert_eq!(fake.sent_commands(), vec!["[[BATCH]]dispatch A;dispatch B"]);
    }

    #[test]
    fn empty_batch_is_a_no_op() {
        let fake = fake::FakeBackend::default();
        fake.batch(&[]).unwrap();
        assert!(fake.sent_commands().is_empty());
    }

    #[test]
    fn monitors_are_applied_through_a_single_eval() {
        // A batch would split the Lua on its first `;`.
        let fake = fake::FakeBackend::default();
        fake.set_monitors(&[
            "hl.monitor({ output = \"A\" })".into(),
            "hl.monitor({ output = \"B\" })".into(),
        ])
        .unwrap();
        assert_eq!(
            fake.sent_commands(),
            vec!["/eval hl.monitor({ output = \"A\" }) hl.monitor({ output = \"B\" })"]
        );
    }

    #[test]
    fn applying_nothing_touches_no_socket() {
        let fake = fake::FakeBackend::default();
        fake.set_monitors(&[]).unwrap();
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
