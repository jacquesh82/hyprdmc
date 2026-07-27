# hyprdmc

**Dynamic monitor configuration for [Hyprland](https://hyprland.org/)** — CLI and web UI,
in a single binary.

Hyprland doesn't handle monitor hotplugging on its own: every configuration change (docking,
projector, external display) means hand-editing `hyprland.lua` and reloading. `hyprdmc`
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
- [Keyboard and Pointer](#keyboard-and-pointer)
- [Command Line](#command-line)
- [Main Screen](#main-screen)
- [Compositor Plugins](#compositor-plugins)
- [Profiles](#profiles)
- [History and Recall](#history-and-recall)
- [Notifications](#notifications)
- [Daemon and Hotplugging](#daemon-and-hotplugging)
- [Persistence](#persistence)
- [Rotation and Flipping](#rotation-and-flipping)
- [Safety Net](#safety-net)
- [HTTP API](#http-api)
- [Configuration](#configuration)
- [Import and Export](#import-and-export)
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
- **Main screen** — designate one of the detected screens as the main one: it anchors the
  workspace at 0×0, opens the row on automatic arrangement, and takes the focus after an apply.
- **Web UI** — a drag-and-drop canvas with snapping, right in the browser, plus a tab for
  the keyboard and pointer.
- **Profiles** — one layout per situation, identified by the monitors plugged in.
- **Hotplugging** — a daemon watches Hyprland and applies the right profile automatically.
- **Recall** — arrange your screens once; the same set of screens gets that layout back on
  its own, with no profile to name and nothing to configure.
- **History** — the last five layouts applied, restorable one command away.
- **Notifications** — the daemon says on your desktop what it detected and what it applied.
- **Safety net** — any change is rolled back automatically if you don't confirm it.
- **Persistence** — writes `monitors.lua` without ever touching the rest of your `hyprland.lua`.
- **Keyboard & pointer** — layout, variant, xkb options and scroll direction (touchpad and mouse
  separately), in their own `inputs.lua`. Never part of a screen profile: docking never changes
  what you type in.
- **Import / export** — the whole configuration in one JSON file, to back up or move to
  another machine. The listening port and the generated-file paths stay local on import.
- **Autostart** — `hyprdmc service install` writes a systemd *user* unit; the same one works
  on Debian, Fedora/RHEL and Arch. A Hyprland autostart line does the job too.
- **Compositor plugins** — everything compositor-specific lives behind two traits, one
  directory per compositor: Hyprland and sway ship in the box, both driving live sessions.
  Adding another is [one directory](docs/writing-a-plugin.md).
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

### From a distribution package

Definitions for Arch (`PKGBUILD`), Fedora/RHEL (`.spec`) and Debian/Ubuntu
(`debian/`) live in [`packaging/`](packaging/README.md), which also documents what
it takes to get each one into the AUR, COPR, a PPA or an APT repository.

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
```

**Make it survive a reboot.** `init` writes the two generated files and adds one `require`
each to your `hyprland.lua`, after backing it up. Run it once:

```sh
hyprdmc init --dry-run    # show the modified hyprland.lua without touching it
hyprdmc init              # back up, wire monitors.lua and inputs.lua, write both
```

**Start the daemon**, which watches for hotplug events *and* serves the web UI:

```sh
hyprdmc daemon            # UI on http://127.0.0.1:28787, no browser opened
hyprdmc daemon --open     # …and open it
hyprdmc web               # UI only, no hotplug watcher — opens the browser by default
```

Open <http://127.0.0.1:28787> and arrange your screens with the mouse:

![The hyprdmc web interface: two screens on the arrangement canvas, the settings panel on
the right](docs/web-ui.png)

**Have it start with your session** — see [Starting automatically](#starting-automatically):

```sh
hyprdmc service install --enable
```

## Web UI

```sh
hyprdmc web            # UI only
hyprdmc daemon         # UI + hotplug watcher
```

Then open <http://127.0.0.1:28787>.

Two tabs: **Displays** and **Keyboard & pointer**. Switching between them keeps an
unapplied arrangement intact.

- Drag monitors on the canvas: they **snap** to neighboring edges.
- Arrow keys for fine adjustment (`Shift` for 100 px steps).
- Side panel: mode, scale, rotation, flip, mirroring, VRR, enable/disable, main screen.
- The **main screen** carries a ★ on the canvas and is the point the whole arrangement is
  anchored on — see [Main Screen](#main-screen).
- Overlaps are flagged in red and block the **Apply** button, which carries a count of the
  outputs the pending change would touch.
- After applying, a **centred dialog** counts down: keep the change or revert it, `Enter`
  and `Escape` respectively. If you don't answer, the previous configuration comes back on
  its own. `Ctrl+Enter` applies from anywhere in the page.
- **Detect new displays** re-reads the outputs on demand, for the rare case where the
  compositor swallowed the hotplug event. It only reads — detecting a screen never moves
  the ones already placed, and a screen that appears while you have unapplied changes
  joins your work instead of erasing it.
- The **history** lives in a drawer opened from the header, listing the last layouts one
  click away from being restored. It is remembered open or closed between visits.
- **Export** / **Import** in the header: the whole configuration as a JSON file.
- State updates live (SSE) whenever a monitor is plugged in or unplugged.

The interface follows the system's light or dark theme, or an explicit choice made with the
toggle in the header.

`hyprdmc web` opens the page in your default browser once the port is actually listening.
`hyprdmc daemon` does not: a background service that pops a window open on every session
start would be intrusive. `--open` and `--no-open` override either default, and a missing
browser only prints the URL instead of failing the command.

Listening is restricted to `127.0.0.1` by default. To open it up to your local network —
knowingly, since the API has **no authentication whatsoever**:

```sh
hyprdmc web --bind 0.0.0.0 --port 28787
```

## Keyboard and Pointer

The second tab of the web UI sets what you type with and how you scroll:

- **Keyboard** — xkb layout, variant, and options (compose key, `ctrl:nocaps`, …). The
  catalogue comes from `/usr/share/X11/xkb/rules/base.lst`, so the list is the same one
  `setxkbmap` uses; variants are filtered down to the selected layout.
- **Pointer** — scroll direction for the touchpad and for the mouse, side by side. These
  are two independent Hyprland settings, and "natural on the touchpad, normal on the wheel"
  is a common pairing — so both are set separately and both stay visible.

**This is deliberately not part of a screen profile.** Docking a laptop must not change the
keyboard layout. The settings live in their own `[input]` section of `config.toml` and their
own generated file, `inputs.lua`, which `hyprdmc init` wires into `hyprland.lua` next to
`monitors.lua`.

Changes apply immediately, with no revert countdown: a layout you cannot type in is
annoying, not a lock-out — the mouse still works and the page is still readable. The
countdown is reserved for changes that can leave you staring at a black screen. Press
**Make permanent** to write `inputs.lua` so the settings survive a compositor restart.

```lua
-- Generated by hyprdmc — DO NOT EDIT BY HAND.
-- Keyboard and pointer only: the screens live in monitors.lua.
hl.config({ input = { kb_layout = "fr", kb_variant = "oss", kb_options = "compose:ralt", natural_scroll = false, touchpad = { natural_scroll = true } } })
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

`--no-confirm` means "don't ask", not "don't record": a layout applied that way still lands
in the history.

## Main Screen

Hyprland has no notion of a "primary output", so `hyprdmc` provides one. Designating a main
screen among the detected ones does three concrete things:

- it sits at **0×0** — the workspace is built around it, and screens to its left or above it
  take negative coordinates, which Hyprland accepts;
- it **opens the row** when the outputs are arranged automatically, instead of ending up
  wherever the compositor happened to list it;
- it **takes the focus** once a layout has been applied.

```sh
hyprdmc primary               # which screen is the main one
hyprdmc primary DP-1          # designate it, and rebuild the layout around it
hyprdmc primary "Dell*"       # by fingerprint or pattern, like a profile rule
hyprdmc primary --none        # back to anchoring on the top-left corner
```

In the web UI it is a checkbox in the panel of the selected display; the main screen carries
a ★ in the arrangement. It is applied with everything else, by the **Apply** button.

The choice is recorded once, as `primary` under `[settings]`, and it is stored as the
screen's **fingerprint** rather than its connector — so it survives being plugged into
another port. It is global rather than per-profile on purpose: which screen you sit in front
of is a property of your desk, not of an arrangement.

`hyprdmc list` marks it with a ★. A main screen that is unplugged, switched off, or
mirroring another one anchors nothing: the layout falls back to its top-left corner and
`hyprdmc` says so instead of failing.

## Compositor Plugins

Turning a layout into a configuration file is the *only* part of `hyprdmc` that is
compositor-specific. It lives behind one trait, one file per compositor, in
[`src/compositor/`](src/compositor/):

```sh
hyprdmc compositor
```
```
Compositor: Hyprland (detected)

│ Compositor   In use     Detected   Applies live   Generated files           │
│ Hyprland     ← active   yes        yes            monitors.lua, inputs.lua  │
│ sway                    no         no             monitors.conf, input.conf │
```

The plugin is detected from the session (`HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`,
`XDG_CURRENT_DESKTOP`). Pin it when the environment does not say — a systemd unit started
before the session — or to generate another compositor's file on purpose:

```toml
[settings]
compositor = "sway"       # omit, or "auto", to detect
```

Everything else — geometry, validation, overlap and scale checks, profiles, recall, history,
the main screen, the web UI — works on a layout and never sees a directive. The same
arrangement comes out as either:

```lua
hl.monitor({ output = "DP-1", mode = "1920x1080@60.00", position = "1920x0", scale = 1, … })
```
```
output "DP-1" enable mode 1920x1080@60.000Hz position 1920 0 scale 1 transform normal adaptive_sync off
```

### Two traits, because they fail separately

A plugin is two halves:

| | `Compositor` | `Session` |
|---|---|---|
| Answers | what the config file looks like | how to reach a running session |
| | pure, no I/O | all I/O |
| Works when that compositor is not running | **yes** | no |

That is what makes `compositor = "sway"` on a Hyprland machine useful rather than broken:
rendering never needs a live compositor. A plugin may implement only the first half —
`drives_sessions()` says so and the callers refuse up front, with the web UI disabling
**Apply** and explaining why instead of offering a button that always fails.

Both shipped plugins implement both halves. sway is driven through i3's IPC — a
length-framed binary protocol, nothing like Hyprland's line-oriented socket — and the same
`output …` directives that go into the file are what `RUN_COMMAND` accepts at runtime.

One honest gap, and it is sway's rather than ours: `GET_INPUTS` reports a keyboard's *active
layout name* ("French (alt.)"), never the `xkb_layout` code behind it, so reading the
keyboard back on sway is partial.

### Adding a compositor

**[docs/writing-a-plugin.md](docs/writing-a-plugin.md)** is the guide: the two traits
method by method, what a compositor's IPC has to offer, how to map its outputs onto the
shared model (logical versus physical coordinates, millihertz, transform encodings), the
rules that bit this codebase, and how to test a plugin with no compositor running.

`src/compositor/sway/` is the worked example, and it is deliberately unlike Hyprland —
space-separated words, `#` comments, one keyword for rotation *and* flipping, no mirroring
at all — so the seam cannot quietly assume Lua. It has already caught two accidental
couplings that only looked correct with one plugin behind them.

The file names in `[settings]` still read `monitors_lua` and `input_lua` for compatibility
with configurations already written; `monitors_file` and `input_file` are accepted synonyms,
and unset they default to the active plugin's own paths.

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

## History and Recall

`hyprdmc` remembers two things, in `$XDG_STATE_HOME/hyprdmc/state.json`. This is derived
data you never edit — it lives apart from your configuration on purpose.

### Recall — the part you never have to set up

Every layout that gets applied is filed under the set of screens it was applied to,
identified by their fingerprints rather than their connectors. Plug the same screens back
in and that layout comes back on its own, whatever port they land on.

This is what makes the tool useful before you have configured anything: arrange your
screens once, and redocking just works. When a named profile also matches, the profile
wins — an explicit choice outranks an implicit one. Set `remember = false` to switch the
behaviour off.

### History — five steps of undo

The last five layouts applied are kept, newest first, and can be restored at any time —
including long after the confirmation window has closed.

```sh
hyprdmc history                  # list them
hyprdmc history restore 1        # go back one step
hyprdmc history clear            # forget everything, recall included
```

```
┌───────────────────────────────────────────────────────────────────────────┐
│ #   When       Origin   Layout                                            │
╞═══════════════════════════════════════════════════════════════════════════╡
│ 0   just now   manual   eDP-1 1920x1080@0x0, DP-1 540x960@1920x0 90°      │
│ 1   4 min ago  desk     eDP-1 1920x1080@0x0, DP-1 1920x1080@1920x0        │
└───────────────────────────────────────────────────────────────────────────┘
```

Everything that gets applied is filed, whichever way it was applied: the CLI, a hotplug
reconcile, or the web UI. A layout applied with the safety net armed is filed **when you
confirm it**, not when it is applied — an arrangement you rejected, or let revert on its
own, has no business in an undo list. A layout Hyprland rolled back is never filed either.

Identical consecutive layouts are collapsed: every hotplug event triggers a reconcile, and
without that the list would fill up with five copies of the same thing and push the entry
you actually want out of reach. Restoring goes through the same safety net as any other
change, so an unwanted restore reverts on its own.

## Notifications

The daemon acts while you are looking elsewhere, so it says what it did. Each notification
names what changed and which layout was chosen:

- `connected: DP-1` — *Profile "desk" applied*
- `disconnected: DP-1` — *Restored the layout you last used with these displays*
- `connected: DP-1` — *No known layout: displays arranged left to right*

They replace one another rather than stacking, since docking a laptop fires several events
in a row. Notifications need `notify-send` (libnotify) and a running notification daemon;
without either, they are silently skipped. Set `notifications = false` to turn them off.

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

Two ways, and which one is right depends on your session.

**The Hyprland autostart** works everywhere, because Hyprland runs it itself, once it is up
and with the right environment. In `hyprland.lua`:

```lua
hl.on("hyprland.start", function ()
  hl.exec_cmd("hyprdmc daemon")
end)
```

**The systemd user service** buys you restart-on-failure and `journalctl` logs. Debian,
Fedora/RHEL and Arch all run systemd and all three give each login session a
`systemd --user` instance, so the same *user* unit covers the three — there is nothing
distribution-specific in it, and nothing installed as root: the daemon drives one session's
displays.

```sh
hyprdmc service install --enable      # write the unit, enable and start it
hyprdmc service install --dry-run     # print it instead, and write nothing
hyprdmc service status                # installed? enabled? running?
hyprdmc service uninstall             # disable and remove
```

The unit is generated rather than copied from this README, because its one important line
is the path to the binary — `/usr/bin`, `~/.cargo/bin` or `~/.local/bin` depending on how
you installed it. `service install` reads that path from the running executable.

> **The catch, and it is a real one.** The unit hooks onto `graphical-session.target`, which
> is only reached if something in your session activates it. `uwsm` does; a plain
> `exec-once = Hyprland` does not — and then the service sits there, enabled, never
> starting, with nothing that looks wrong. `hyprdmc service install` checks and tells you
> when that is the case.
>
> If your session does not activate it, either point the unit at a target that is reached:
>
> ```sh
> hyprdmc service install --wanted-by default.target --enable
> ```
>
> (systemd reaches `default.target` well before the compositor exists, so the daemon will
> fail its first attempts; the unit retries for about a minute, which covers a normal
> login), or simply use the Hyprland autostart above.

A packaged reference unit for `/usr/lib/systemd/user/` lives in
[`packaging/systemd/hyprdmc.service`](packaging/systemd/hyprdmc.service).

## Persistence

`hyprdmc` never rewrites the rest of your `hyprland.lua`. It manages its own files —
`~/.config/hypr/monitors.lua` for the screens and `~/.config/hypr/inputs.lua` for the
keyboard and pointer — and wires them in only once, in a single pass:

```sh
hyprdmc init --dry-run     # show what would be done
hyprdmc init               # back up, then modify
```

`init` is idempotent and:

1. copies `hyprland.lua` to `hyprland.lua.hyprdmc.bak`;
2. comments out existing `hl.monitor{…}` calls — multi-line ones included — carrying them
   over into `monitors.lua`;
3. inserts `require("monitors")` where that configuration used to be, and `require("inputs")`
   next to it for the keyboard and pointer settings.

A generated file kept outside `~/.config/hypr` is loaded with `dofile("…")` instead: Hyprland's
`package.path` only covers its own configuration directory.

From then on, `hyprdmc persist` rewrites `monitors.lua` from the current state. The file is
written atomically: Hyprland can never read a partial version of it.

```lua
-- Generated by hyprdmc — DO NOT EDIT BY HAND.
hl.monitor({ output = "eDP-1", mode = "1920x1080@60.06", position = "0x0", scale = 1, transform = 0, mirror = "", vrr = 0, disabled = false })
hl.monitor({ output = "DP-3", mode = "3840x2160@60.00", position = "1920x0", scale = 1.5, transform = 5, mirror = "", vrr = 0, disabled = false })
```

Every field is spelled out, defaults included: Lua monitor rules are cumulative, so a field
left unsaid would keep whatever an earlier call gave it — a mirror would survive its own
removal.

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
| `GET` | `/api/state` | full state: monitors, layout, issues, profiles, compositor |
| `GET` | `/api/monitors` | raw monitors as reported by Hyprland |
| `POST` | `/api/apply` | apply a layout (`{outputs, primary, force, guard}`) |
| `POST` | `/api/confirm` | confirm the last apply |
| `POST` | `/api/revert` | roll back immediately |
| `POST` | `/api/persist` | write `monitors.lua` |
| `GET` | `/api/input` | keyboard/pointer settings plus the xkb catalogue |
| `PUT` | `/api/input` | apply keyboard/pointer settings |
| `POST` | `/api/input/persist` | write `inputs.lua` |
| `GET` | `/api/config` | export the whole configuration as JSON |
| `POST` | `/api/config` | import an exported configuration |
| `GET` | `/api/profiles` | list of profiles and the active one |
| `PUT` | `/api/profiles/{name}` | save a profile |
| `DELETE` | `/api/profiles/{name}` | delete a profile |
| `POST` | `/api/profiles/{name}/apply` | apply a profile |
| `GET` | `/api/history` | the last few applied layouts |
| `POST` | `/api/history/{index}/restore` | reapply a recorded layout |
| `GET` | `/api/i18n` | UI strings for the active language |
| `GET` | `/api/events` | SSE stream pushing state on every change |

```sh
curl -s localhost:28787/api/state | jq '.monitors[].name'
curl -X POST localhost:28787/api/profiles/desk/apply
```

## Configuration

`~/.config/hyprdmc/config.toml` (created on the first `profile save`):

```toml
[settings]
web_port = 28787               # high on purpose — see below
bind = "127.0.0.1"
auto_apply = true               # the daemon applies the matching profile on hotplug
compositor = "hyprland"         # omit for auto-detection — see "Compositor Plugins"
confirm_timeout_secs = 10       # 0 = no automatic rollback
monitors_lua = "/home/you/.config/hypr/monitors.lua"
input_lua = "/home/you/.config/hypr/inputs.lua"
notifications = true            # announce changes on the desktop
remember = true                 # recall the layout last used with these screens
primary = "Dell Inc. U2723QE ABC123"   # main screen; omit for none — see "Main Screen"
language = "en"                 # omit to follow the system locale

# Keyboard and pointer. Outside the profiles on purpose: plugging in a
# monitor has no business changing your keyboard layout.
[input]
kb_layout = "fr"
kb_variant = "oss"              # "" for the plain layout
kb_options = "compose:ralt"     # comma-separated, "" for none
natural_scroll = false          # mice
touchpad_natural_scroll = true  # touchpads are a separate setting

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

**Why 28787.** A developer machine already has something on 3000, 5173, 8000 and 8080, and
8787 itself belongs to RStudio Server — a UI that refuses to start because a dev server got
there first is a poor first impression. It stays below 32768 all the same: above that is the
range the kernel hands out to outgoing connections (`ip_local_port_range`), where the port
you meant to listen on may already be taken by something else's socket. Override it per run
with `--port`, or once and for all in `config.toml`.

Fields of a rule: `match` (required), `enabled`, `mode`, `position`, `scale`, `rotation`,
`flipped`, `mirror_of`, `vrr`. `mode` and `position` accept `"auto"` to let `hyprdmc` decide.

Logging: `HYPRDMC_LOG=hyprdmc=debug hyprdmc daemon`.

## Import and Export

The **Export** button in the web UI downloads everything — settings, profiles, keyboard and
pointer — as a single JSON file, indented so it can be read, diffed and edited by hand:

```json
{
  "kind": "hyprdmc-config",
  "version": 1,
  "config": {
    "settings": { "auto_apply": true, "confirm_timeout_secs": 10, ... },
    "input": { "kb_layout": "fr", "touchpad_natural_scroll": true, ... },
    "profile": [ { "name": "desk", "output": [ ... ] } ]
  }
}
```

**Import** replaces the configuration with such a file, after confirming. The `kind` and
`version` markers mean a file that is not a hyprdmc export is refused with a sentence
rather than a parse error.

Four fields are deliberately **not** imported: `web_port`, `bind`, `monitors_lua` and
`input_lua`. A configuration exported on another machine carries that machine's home
directory, and silently writing `monitors.lua` into a path that does not exist here would
look like hyprdmc breaking. Everything you actually meant to move — profiles, the main
screen, keyboard and pointer, behaviour — comes across.

The main screen travels like a profile rule does, as a fingerprint: if it names hardware that
is not plugged in here, it simply resolves to no main screen rather than anchoring the layout
on something absent.

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
  monitor.rs   monitor model, rotation, modes, fingerprint
  layout.rs    layout, logical sizes, validation, arrangement
  apply.rs     sending, verification, rollback
  config.rs    TOML profiles, matching against hardware
  emit.rs      generated-file writing, wiring into the compositor's config
  input.rs     keyboard and pointer settings, xkb catalogue
  daemon.rs    event loop, debouncing, shared state
  session.rs   the Session trait: reach a live compositor, watch for hotplug
  compositor/  one plugin per compositor — the only protocol-aware code
    mod.rs       the Compositor trait, the registry, session detection
    hyprland/    hl.monitor{…}, require/dofile; /eval over .socket.sock
    sway/        output …, include; i3's framed IPC over $SWAYSOCK
  web/         axum API, SSE stream, embedded UI
```

**One protocol boundary.** `compositor/` is the only place that knows what a directive looks
like *or* how to talk to a compositor. Everything else operates on a `Layout` and a
`Session` — which is why the same validation, the same profiles and the same UI serve every
plugin, and why a new compositor is one directory rather than a fork. See
[Compositor Plugins](#compositor-plugins) and
[docs/writing-a-plugin.md](docs/writing-a-plugin.md).

Four Hyprland quirks shaped this design, all verified against version 0.56:

- **Lua replaced hyprlang in 0.55** — `keyword` is refused outright ("keyword can't work with
  non-legacy parsers"), so changes go through `eval`. All the `hl.monitor{…}` calls travel in
  a single request: `[[BATCH]]` splits on `;` and would cut the Lua in half.
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

**The UI is not on the port the README says**
Defaults only apply to keys your `config.toml` does not have. A file written before the
default moved still says `web_port = 8787`, and that is what wins — change the line, or
delete it to follow the default from now on.

**The keyboard file is still called `input.lua`**
Same reason: an `input_lua` path already written to `config.toml` keeps pointing where it
points. The generated file is now `inputs.lua`, because `require("input")` resolves to a
module Hyprland's Lua environment already defines rather than to ours. To move over: update
`input_lua` in `config.toml`, delete the old `input.lua`, drop the `require("input")` line
from `hyprland.lua`, then run `hyprdmc init` — it wires the new one in and leaves the rest
of your configuration alone.

## License

MIT — see [LICENSE](LICENSE).
