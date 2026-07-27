# Writing a compositor plugin

`hyprdmc` supports a Wayland compositor through **two traits in one directory**.
Everything else — geometry, overlap and scale validation, profiles, recall, history,
the main screen, the daemon, the web UI — already works on a `Layout` and needs no
change.

A plugin is a *compile-time* one: a module, a `static`, one line in the registry.
Not a `dlopen` shared object — `hyprdmc` ships as a single static binary with no
runtime dependencies, and a plugin ABI would trade that for an unstable Rust ABI
and a search path to get wrong.

- [The two traits](#the-two-traits)
- [Anatomy of a plugin](#anatomy-of-a-plugin)
- [Step by step](#step-by-step)
- [What the compositor must let you do](#what-the-compositor-must-let-you-do)
- [Mapping outputs onto the shared model](#mapping-outputs-onto-the-shared-model)
- [Rules learned the hard way](#rules-learned-the-hard-way)
- [Testing without the compositor](#testing-without-the-compositor)
- [Protocol references](#protocol-references)

## The two traits

| | `Compositor` | `Session` |
|---|---|---|
| Answers | "what does the config file look like?" | "how do I reach a running session?" |
| Purity | pure, no I/O | all I/O |
| Works when the compositor is not running | **yes** | no |
| Declared in | `src/compositor/mod.rs` | `src/session.rs` |

They are separate because they fail separately. Rendering a sway file from a Hyprland
machine is a useful thing (`compositor = "sway"`), not a bug. A session only exists
while its compositor does.

A plugin **may implement only `Compositor`**. Say so with
`fn drives_sessions() -> false`, and the callers refuse up front with a sentence
instead of failing at the socket. `src/compositor/mod.rs::testing::FileOnly` is a
worked example of exactly that shape.

## Anatomy of a plugin

```
src/compositor/<name>/
  mod.rs   impl Compositor  (syntax)  +  impl Session  (live connection)
  ipc.rs   the wire protocol: framing, requests, replies, events
```

Splitting `ipc.rs` out is not decoration. It is what lets the tests stub the wire and
still exercise the real request formatting — see [Testing](#testing-without-the-compositor).

## Step by step

### 1. Render the syntax

```rust
impl Compositor for Niri {
    fn id(&self) -> &'static str { "niri" }          // goes in config.toml
    fn label(&self) -> &'static str { "niri" }       // shown to humans
    fn running(&self) -> bool {                       // from the environment only
        std::env::var_os("NIRI_SOCKET").is_some() || super::desktop_is("niri")
    }

    fn config_dir(&self) -> PathBuf { super::config_subdir("niri") }
    fn main_config(&self) -> PathBuf { self.config_dir().join("config.kdl") }
    fn monitors_file(&self) -> &'static str { "monitors.kdl" }
    fn input_file(&self) -> &'static str { "input.kdl" }
    fn comment(&self) -> &'static str { "//" }

    fn output_directive(&self, o: &OutputState) -> String { … }
    fn input_directives(&self, i: &InputConfig) -> Vec<String> { … }

    fn include(&self, main: &Path, generated: &Path) -> String { … }
    fn includes(&self, line: &str, generated: &Path) -> bool { … }
    fn is_include(&self, line: &str) -> bool { … }
    fn opens_output(&self, line: &str) -> bool { … }
    …
}
```

`running()` reads the **environment only**. Probing sockets here would make detection
depend on whether a daemon happens to be up, and detection runs before anything is
connected.

`includes()` and `is_include()` answer different questions — "does this line pull in
*my* file?" versus "does this line pull in *some* file?". The second is what places a
new include next to the user's existing ones rather than at the top of their config.
Getting them confused is how the first version of this seam had a bug that only
looked correct for Lua.

### 2. Connect to a session

```rust
fn drives_sessions(&self) -> bool { true }

fn connect(&self) -> Result<Box<dyn Session>> {
    Ok(Box::new(NiriSession { socket: ipc::socket_path()? }))
}
```

```rust
impl Session for NiriSession {
    fn outputs(&self) -> Result<Vec<Monitor>>;               // read the screens
    fn apply(&self, directives: &[String]) -> Result<()>;    // your own directives
    fn focus(&self, output: &str) -> Result<()>;             // best-effort by contract
    fn read_input(&self) -> Result<InputConfig>;
    fn apply_input(&self, input: &InputConfig) -> Result<()>;
    fn watch(&self) -> Result<Box<dyn EventStream>>;         // hotplug
}
```

`apply` receives the directives **your own `Compositor` rendered**. One renderer
serves the file and the live path, which is why a layout written to disk and a layout
pushed to the session can never disagree.

### 3. Hotplug, without writing async

```rust
impl EventStream for Events {
    fn next_event(&mut self) -> Option<CompositorEvent> { … }   // blocks
}
```

Blocking on purpose. The daemon runs it on a blocking task and forwards each event
into a channel, so a plugin joins hotplug with no `async` of its own. `None` means the
compositor closed the connection — the daemon reconnects with backoff, which is the
right answer for a compositor restart.

Map onto `CompositorEvent::OutputAdded` / `OutputRemoved` when the protocol names the
output. When it does not — sway's `output` event is `{"change":"unspecified"}` and
says only that *something* changed — return something the daemon will re-read on,
and say so in a comment. Do not invent a name.

### 4. Register it

```rust
// src/compositor/mod.rs
static REGISTRY: &[&(dyn Compositor + Sync)] =
    &[&hyprland::Hyprland, &sway::Sway, &niri::Niri];
```

That is the whole registration mechanism. `hyprdmc compositor` picks it up
immediately, `compositor = "niri"` selects it, and detection finds it.

## What the compositor must let you do

Before starting, check the compositor's IPC offers these five. The first two are
non-negotiable; a plugin missing any of the last three is still worth having.

| Need | Hyprland | sway |
|---|---|---|
| List outputs with modes | `j/monitors all` | `GET_OUTPUTS` (3) |
| Configure an output at runtime | `/eval hl.monitor{…}` | `RUN_COMMAND` (0) with `output …` |
| Focus an output | `dispatch focusmonitor` | `RUN_COMMAND` with `focus output` |
| Read keyboard settings | `j/getoption input:kb_layout` | `GET_INPUTS` (100) — **partial** |
| Output add/remove events | `.socket2.sock` lines | `SUBSCRIBE` (2) `["output"]` |

Partial is fine, and being honest about it is the point. sway's `GET_INPUTS` reports a
keyboard's *active layout name* ("French (alt.)"), never the `xkb_layout` code that
produced it — so `read_input` cannot round-trip the keyboard, and
`src/compositor/sway/ipc.rs::read_input` says so where a reader will find it rather
than guessing a code that sway would then reject.

## Mapping outputs onto the shared model

`Monitor` is the shared model, and its field names follow **Hyprland's JSON** because
they are also what the web API serialises. They are not free to change. So unless your
compositor happens to use the same names, deserialize into a private type and map:

```rust
#[derive(Deserialize)]
struct SwayOutput { name: String, active: bool, rect: Rect, current_mode: …, … }

fn to_monitors(outputs: Vec<SwayOutput>) -> Vec<Monitor> { … }
```

Four conversions to get right, all of which bit this codebase at least once:

| | Watch out for |
|---|---|
| **Position** | `Monitor::x`/`y` are **logical** (post-scale). sway's `rect` already is; check yours. |
| **Size** | `Monitor::width`/`height` are the **mode**, in physical pixels — not the logical size. `OutputState::logical_size` divides by the scale itself. |
| **Refresh** | `Monitor::refresh_rate` is hertz. sway reports millihertz (`60056` → `60.056`). |
| **Transform** | `Monitor::transform` is Hyprland's 0..=7 encoding of rotation + flip. A string keyword (`"flipped-90"`) must be converted both ways, and the round trip is worth a test. |

`disabled`, not `active`: the shared model asks whether the output is *off*.
No mirroring? Report `mirror_of: "none"`.

## Rules learned the hard way

**State every field you own, every time.** Both compositors here keep whatever a
previous directive set, so an omitted `transform` lets a rotation outlive its own
removal. Write defaults out explicitly.

**Drop what you cannot express, out loud.** sway has no mirroring. The plugin writes
the output as the plain output it is and appends a comment naming what was lost —
because silently emitting the target's position would produce two overlapping screens
and call it a feature.

**Quote with your parser's rules, not Lua's.** Output names come from the compositor
and patterns come from the user; neither is trusted. Hyprland escapes `\` and `"`;
sway has no escape for a quote inside a quoted word, so the sway plugin drops it
rather than emit a line that breaks.

**Native byte order means native.** i3's spec says the header integers "are not
converted, so they are in native byte order" — `to_ne_bytes`, not the big-endian
reflex.

**Keep the event connection separate.** Where events share the reply channel (i3's do,
distinguished by the high bit of the reply type), subscribe on its own connection.
Otherwise every `GET_OUTPUTS` has to wade through events it did not ask for.

**No comment markers in `locales/app.yml`.** The generated-file headers are plain
sentences; `emit::commented` prefixes each line with your `comment()`. One translation
serves every syntax.

## Testing without the compositor

Split the wire behind a one-method trait and the whole plugin becomes testable:

```rust
pub trait Transport: Send + Sync {
    fn query(&self, cmd: &str) -> Result<String>;
}
```

The Hyprland plugin ships two doubles built on that, in
`src/compositor/hyprland/ipc.rs`:

- **`FakeTransport`** — replays canned replies, records what was sent, and can return
  a stale state for *n* reads to reproduce the compositor's apply latency
  (`settling_after`).
- **`FakeSession`** — a `Session` wrapping a **real** `HyprSession` over a
  `FakeTransport`. Tests assert on the bytes that production code formats, not on a
  double's imitation of them.

What is worth testing, from what has actually caught bugs here:

- every field appears in the directive, defaults included;
- the transform round trip, all eight values, renderer against parser;
- what the compositor cannot express is dropped *and* announced;
- an include you write is one your `includes()` recognises again — that is what makes
  `init` idempotent;
- your `opens_output()` matches your directives and nothing else (`workspace 1 output
  DP-1` assigns a workspace and is none of your business);
- an unknown enum value from the wire degrades rather than panics — compositors add
  keywords.

## Protocol references

- **Hyprland** — `hyprctl`'s socket protocol; `src/compositor/hyprland/ipc.rs`
  documents the two sockets. Since 0.55 the config is Lua and `keyword` is refused
  outright, so everything goes through `/eval`.
- **i3** — <https://i3wm.org/docs/ipc.html>. Framing, message type codes 0–12, the
  high-bit event convention, the `SUBSCRIBE` payload.
- **sway** — `sway-ipc(7)`. `SWAYSOCK`, type 100 `GET_INPUTS`, and the output fields
  i3 does not have (`make`, `model`, `serial`, `scale`, `transform`, `modes`,
  `current_mode`, `adaptive_sync_status`).
- **niri** — `NIRI_SOCKET`, newline-delimited JSON. Not implemented; the notes above
  are a sketch, not a tested plugin.
- **wlroots compositors generally** — `wlr-output-management-unstable-v1` is the
  protocol `wlr-randr` and `kanshi` use. A plugin built on it would cover several
  compositors at once, at the cost of speaking Wayland rather than a socket.
