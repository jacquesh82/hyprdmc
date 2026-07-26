# hyprdmc

**Dynamic monitor configuration for [Hyprland](https://hyprland.org/)** — CLI and web UI,
in a single binary.

Hyprland doesn't handle monitor hotplugging on its own: every configuration change (docking,
projector, external display) means hand-editing `hyprland.conf` and reloading. `hyprdmc`
detects monitors, positions them, rotates and flips them — and, most importantly, **reapplies
the right profile on its own** whenever the hardware changes.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Output   State              Mode              Position   Scale   Orientation │
╞══════════════════════════════════════════════════════════════════════════════╡
│ eDP-1    active (focused)   1920x1080@60.06   0x0        1       0°          │
│ DP-3     active             3840x2160@60.00   1920x0     1.5     90° flipped │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Table of Contents

- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Web UI](#web-ui)
- [Command Line](#command-line)
- [Profiles](#profiles)
- [Daemon and Hotplugging](#daemon-and-hotplugging)
- [Persistence](#persistence)
- [Rotation and Flipping](#rotation-and-flipping)
- [Safety Net](#safety-net)
- [HTTP API](#http-api)
- [Configuration](#configuration)
- [Language](#language)
- [Development](#development)
- [Under the Hood](#under-the-hood)
- [Troubleshooting](#troubleshooting)
- [License](#license)

## Features

- **Detection** — lists connected monitors, their modes, position, and orientation.
- **Positioning** — pixel-precise, relative (`right-of`, `below`, …), or automatic arrangement.
- **Rotation and flipping** — 0/90/180/270°, with or without a mirror effect.
- **Mirroring** — one output can duplicate another's image.
- **Web UI** — a drag-and-drop canvas with snapping, right in the browser.
- **Profiles** — one layout per situation, identified by the monitors plugged in.
- **Hotplugging** — a daemon watches Hyprland and applies the right profile automatically.
- **Safety net** — any change is rolled back automatically if you don't confirm it.
- **Persistence** — writes `monitors.conf` without ever touching your `hyprland.conf`.
- **No dependencies** — talks directly to Hyprland's sockets, `hyprctl` isn't required.
  No JavaScript toolchain either: the web UI is embedded in the binary.

## Installation

### From source

```sh
git clone https://github.com/jacquesh82/hyprdmc.git
cd hyprdmc
cargo build --release
install -Dm755 target/release/hyprdmc ~/.local/bin/hyprdmc
```

Rust 1.87 or newer (2024 edition). Hyprland 0.40+; developed and tested against 0.56.

### With cargo

```sh
cargo install --path .
```

## Quick Start

```sh
# 1. What's connected?
hyprdmc list

# 2. Arrange the outputs
hyprdmc arrange DP-1 right-of eDP-1

# 3. Save the current layout under a name
hyprdmc profile save desk

# 4. Wire monitors.conf into hyprland.conf (backs it up automatically)
hyprdmc init

# 5. Start the daemon: hotplug watcher + web UI on http://127.0.0.1:8787
hyprdmc daemon
```

## Web UI

```sh
hyprdmc web            # UI only
hyprdmc daemon         # UI + hotplug watcher
```

Then open <http://127.0.0.1:8787>.

- Drag monitors on the canvas: they **snap** to neighboring edges.
- Arrow keys for fine adjustment (`Shift` for 100 px steps).
- Side panel: mode, scale, rotation, flip, mirroring, VRR, enable/disable.
- Overlaps are flagged in red and block the **Apply** button.
- After applying, a banner lets you **keep** the change or **revert** it; if you don't
  answer, the previous configuration is restored automatically.
- State updates live (SSE) whenever a monitor is plugged in or unplugged.

Listening is restricted to `127.0.0.1` by default. To open it up to your local network —
knowingly, since the API has **no authentication whatsoever**:

```sh
hyprdmc web --bind 0.0.0.0 --port 8787
```

## Command Line

### Reading

```sh
hyprdmc list                  # table of monitors
hyprdmc list --json           # raw output
hyprdmc modes eDP-1           # available modes
```

### Modifying

```sh
hyprdmc set DP-1 --mode 3840x2160@60 --scale 1.5
hyprdmc set DP-1 --rotate 90              # portrait
hyprdmc set DP-1 --rotate 90 --flip       # portrait, flipped image
hyprdmc set DP-1 --pos 1920x0
hyprdmc set DP-1 --mirror eDP-1           # duplicate the laptop's screen
hyprdmc set DP-1 --no-mirror
hyprdmc set eDP-1 --disable               # laptop lid closed on the dock
hyprdmc set DP-1 --vrr on
hyprdmc set DP-1 --rotate 270 --save desk     # apply and save into the profile
```

### Relative positioning

```sh
hyprdmc arrange DP-1 right-of eDP-1
hyprdmc arrange DP-1 above eDP-1 DP-2 right-of DP-1     # multiple triples
hyprdmc auto                                            # horizontal auto-arrange
```

Relations: `left-of`, `right-of`, `above`, `below`, `same-as`
(also available in French: `gauche-de`, `droite-de`, `au-dessus-de`, `en-dessous-de`).

### Common options

| Option | Effect |
|---|---|
| `--force` | apply despite warnings and detected mismatches |
| `--no-confirm` | skip the confirmation prompt and automatic rollback |
| `-v`, `--verbose` | verbose logging |

## Profiles

A profile describes a layout and how to recognize the monitors it applies to.

```sh
hyprdmc profile save desk            # save the current layout
hyprdmc profile save solo --exact    # only applies when no other monitor is connected
hyprdmc profile list
hyprdmc profile show desk
hyprdmc profile apply desk
hyprdmc profile rename desk dock
hyprdmc profile delete dock
hyprdmc apply                        # apply the profile matching the connected hardware
```

### Matching monitors

A monitor is identified by its **fingerprint** — `make model serial-number` — rather than
its connector: plugging the same monitor into a different port doesn't break the profile.
The connector name still works too, as do glob patterns:

```toml
match = "Dell Inc. U2723QE H7X2K93"    # full fingerprint, unambiguous
match = "Dell*"                        # any Dell
match = "eDP-1"                        # by connector
```

### Choosing a profile

Among the profiles whose rules **all** match a connected monitor, `hyprdmc` picks the one
covering the most monitors. Ties are broken in favor of an `exact` profile, then by
declaration order.

A connected monitor the profile doesn't mention isn't ignored: it's enabled at its preferred
mode and placed to the right of the layout.

If no profile matches, monitors are simply arranged left to right.

## Daemon and Hotplugging

```sh
hyprdmc daemon                    # hotplug watcher + web UI
hyprdmc daemon --no-web           # hotplug watcher only
hyprdmc daemon --port 9000
```

The daemon listens on Hyprland's event socket. On every connect or disconnect, it waits
500 ms for things to settle — a dock fires several events in a row — then selects and
applies the matching profile. It reconnects on its own if Hyprland restarts.

### Starting automatically

With Hyprland, in `hyprland.conf`:

```conf
exec-once = hyprdmc daemon
```

Or as a systemd user service — `~/.config/systemd/user/hyprdmc.service`:

```ini
[Unit]
Description=Dynamic monitor configuration for Hyprland
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/hyprdmc daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now hyprdmc.service
```

## Persistence

`hyprdmc` never rewrites your `hyprland.conf`. It manages its own file,
`~/.config/hypr/monitors.conf`, and wires it in only once:

```sh
hyprdmc init --dry-run     # show what would be done
hyprdmc init               # back up, then modify
```

`init` is idempotent and:

1. copies `hyprland.conf` to `hyprland.conf.hyprdmc.bak`;
2. comments out existing `monitor =` directives, carrying them over into `monitors.conf`;
3. inserts `source = ~/.config/hypr/monitors.conf` after your other `source` lines.

From then on, `hyprdmc persist` rewrites `monitors.conf` from the current state. The file is
written atomically: Hyprland can never read a partial version of it.

```conf
# Generated by hyprdmc — DO NOT EDIT BY HAND.
monitor = eDP-1,1920x1080@60.06,0x0,1
monitor = DP-3,3840x2160@60.00,1920x0,1.5,transform,5
```

## Rotation and Flipping

Hyprland encodes orientation as a single integer. `hyprdmc` exposes two independent settings
and handles the conversion:

| `transform` | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
|---|---|---|---|---|---|---|---|---|
| **rotation** | 0° | 90° | 180° | 270° | 0° | 90° | 180° | 270° |
| **flipped** | no | no | no | no | **yes** | **yes** | **yes** | **yes** |

```sh
hyprdmc set DP-1 --rotate 90              # transform 1
hyprdmc set DP-1 --rotate 90 --flip       # transform 5
hyprdmc set DP-1 --rotate 0 --no-flip     # transform 0
```

Rotation swaps width and height in the workspace's coordinate space: `hyprdmc` accounts for
that when positioning monitors and detecting overlaps.

## Safety Net

A bad rotation or a stray position can leave a monitor unreadable. Three safeguards:

1. **Pre-flight validation** — overlaps, unreachable monitors, impossible scales, mirroring
   a monitor that isn't connected, turning off every monitor at once. Errors block the
   change, warnings just inform. `--force` overrides them.

2. **Post-apply verification** — Hyprland replies `ok` even when it didn't actually comply:
   a nonexistent mode is silently accepted, an invalid scale gets rounded without saying so.
   `hyprdmc` re-reads the state and compares it against what was requested. If it doesn't
   match, it rolls back immediately.

3. **Delayed confirmation** — after a successful apply, you have 10 seconds to confirm it.
   If you don't respond, the previous configuration is restored. Doing nothing is enough
   to get back to safety.

```sh
hyprdmc set DP-1 --rotate 90
# Keep this configuration? [y/N] (auto-revert in 10 s)
```

The delay is controlled by `confirm_timeout_secs`; `0` disables the mechanism, `--no-confirm`
bypasses it for a single command.

## HTTP API

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/state` | full state: monitors, layout, issues, profiles |
| `GET` | `/api/monitors` | raw monitors as reported by Hyprland |
| `POST` | `/api/apply` | apply a layout (`{outputs, force, guard}`) |
| `POST` | `/api/confirm` | confirm the last apply |
| `POST` | `/api/revert` | roll back immediately |
| `POST` | `/api/persist` | write `monitors.conf` |
| `GET` | `/api/profiles` | list of profiles and the active one |
| `PUT` | `/api/profiles/{name}` | save a profile |
| `DELETE` | `/api/profiles/{name}` | delete a profile |
| `POST` | `/api/profiles/{name}/apply` | apply a profile |
| `GET` | `/api/events` | SSE stream pushing state on every change |

```sh
curl -s localhost:8787/api/state | jq '.monitors[].name'
curl -X POST localhost:8787/api/profiles/desk/apply
```

## Configuration

`~/.config/hyprdmc/config.toml` (created on the first `profile save`):

```toml
[settings]
web_port = 8787
bind = "127.0.0.1"
auto_apply = true               # the daemon applies the matching profile on hotplug
confirm_timeout_secs = 10       # 0 = no automatic rollback
monitors_conf = "/home/you/.config/hypr/monitors.conf"

[[profile]]
name = "desk"
exact = false

[[profile.output]]
match = "AU Optronics 0x5799"   # laptop panel
enabled = false                 # lid closed on the dock

[[profile.output]]
match = "Dell Inc. U2723QE H7X2K93"
mode = "3840x2160@60"
position = "0x0"
scale = 1.5
rotation = 0
flipped = false
vrr = true
```

Fields of a rule: `match` (required), `enabled`, `mode`, `position`, `scale`, `rotation`,
`flipped`, `mirror_of`, `vrr`. `mode` and `position` accept `"auto"` to let `hyprdmc` decide.

Logging: `HYPRDMC_LOG=hyprdmc=debug hyprdmc daemon`.

## Language

`hyprdmc` ships with English and French translations. It picks a language in this order:

1. the `HYPRDMC_LANG` environment variable, if set;
2. otherwise the `language` key in `config.toml`;
3. otherwise the usual `LC_ALL` / `LC_MESSAGES` / `LANG` locale variables;
4. and English if none of the above resolve to a supported language.

This affects runtime messages: CLI feedback, validation warnings and the web UI. `--help`
output and log lines are always in English, regardless of the resolved language — the former
because clap needs its help text at compile time, the latter so that a pasted log stays
readable in a bug report.

Adding a new language just means adding a section to `locales/app.yml` and registering its
code in the `AVAILABLE` list in `src/i18n.rs` — contributions welcome.

## Development

```sh
cargo test              # the full test suite runs without a compositor
cargo clippy --all-targets
cargo fmt
```

The business logic only depends on the `HyprBackend` trait, which makes it possible to test
everything against a simulated backend — including the compositor's apply latency.

### Testing multi-monitor setups without hardware

Hyprland can create virtual outputs:

```sh
hyprctl output create headless test-1
hyprdmc list
hyprdmc set test-1 --rotate 90 --no-confirm
hyprctl output remove test-1
```

To avoid touching your real configuration while experimenting:

```sh
XDG_CONFIG_HOME=/tmp/scratch hyprdmc profile save draft
```

## Under the Hood

```
src/
  ipc.rs       Hyprland sockets: requests (.socket.sock) and events (.socket2.sock)
  monitor.rs   monitor model, rotation, modes, fingerprint
  layout.rs    layout, logical sizes, validation, arrangement
  apply.rs     batch sending, verification, rollback
  config.rs    TOML profiles, matching against hardware
  emit.rs      monitors.conf generation, wiring into hyprland.conf
  daemon.rs    event loop, debouncing, shared state
  web/         axum API, SSE stream, embedded UI
```

Three Hyprland quirks shaped this design, all verified against version 0.56:

- **`ok` doesn't mean "done"** — a nonexistent mode, a nonsensical position, or an invalid
  scale are all accepted without error. Hence the systematic re-reading of state after
  every change.
- **Applying a change is asynchronous** — a rotation takes roughly 50 ms to show up in
  `j/monitors`. `hyprdmc` polls until the state converges rather than checking just once,
  without wasting time waiting on corrections the compositor will never make.
- **`mirrorOf` reports a numeric id, not a name** — Hyprland publishes `"0"` where the
  configuration expects `eDP-1`. The id is resolved back to a connector name on read.

## Troubleshooting

**"Hyprland doesn't seem to be reachable"**
`HYPRLAND_INSTANCE_SIGNATURE` isn't set (a systemd service started too early, a remote
session, …). The daemon finds the running instance on its own if there's only one;
otherwise, export the variable yourself.

**My scale keeps getting changed**
Hyprland only accepts scales that produce an integer logical size. `hyprdmc` warns before
sending the change and suggests the nearest valid value.

**The profile doesn't apply on hotplug**
Check that the daemon is running (`systemctl --user status hyprdmc`), that `auto_apply` is
`true`, and that the profile actually matches: `hyprdmc profile list` shows which profiles
are compatible with the currently connected hardware.

**My settings disappear when Hyprland restarts**
Run `hyprdmc init` then `hyprdmc persist`: without that, changes only ever live in memory.

## License

MIT — see [LICENSE](LICENSE).
