//! Persistence: generating `monitors.lua` and wiring that file into the
//! user's Hyprland configuration.
//!
//! Guiding principle: `hyprdmc` is the sole owner of `monitors.lua` and
//! only touches `hyprland.lua` once, to add a `require` line to it.
//!
//! Since Hyprland 0.55 the configuration is Lua; hyprlang — and with it
//! `monitor = …` directives and `source = …` — is gone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use rust_i18n::t;

use crate::config::home;
use crate::layout::{Layout, lua_string};

/// Writes a file atomically: a sibling temporary file, then `rename`, so
/// that no reader ever sees partial content — Hyprland could reload the
/// configuration mid-write.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| {
        t!("fs.create_dir_failed", path = dir.display().to_string()).to_string()
    })?;

    let tmp = dir.join(format!(
        ".{}.hyprdmc.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("out"),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    {
        let mut file = std::fs::File::create(&tmp).with_context(|| {
            t!("fs.create_dir_failed", path = tmp.display().to_string()).to_string()
        })?;
        file.write_all(content.as_bytes())
            .with_context(|| t!("fs.write_failed", path = tmp.display().to_string()).to_string())?;
        file.sync_all().ok();
    }

    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        t!("fs.rename_failed", path = path.display().to_string()).to_string()
    })
}

/// Renders the contents of `monitors.lua` for a layout.
pub fn render(layout: &Layout) -> String {
    let mut out = t!("emit.header").to_string();
    out.push('\n');
    for call in layout.to_lua_calls() {
        out.push_str(&call);
        out.push('\n');
    }
    out
}

/// Writes the layout to `monitors.lua`.
pub fn persist(layout: &Layout, path: &Path) -> Result<()> {
    write_atomic(path, &render(layout))
}

/// Renders the contents of `input.lua`.
pub fn render_input(input: &crate::input::InputConfig) -> String {
    let mut out = t!("emit.input_header").to_string();
    out.push('\n');
    out.push_str(&input.to_lua());
    out.push('\n');
    out
}

/// Writes the keyboard and pointer settings to `input.lua`.
pub fn persist_input(input: &crate::input::InputConfig, path: &Path) -> Result<()> {
    write_atomic(path, &render_input(input))
}

/// Replaces the home directory prefix with `~`, the way one would write it
/// by hand in `hyprland.lua`.
pub fn tildify(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// What `hyprdmc init` did (or would do, in a dry run).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InitReport {
    /// `hyprland.lua` was already pulling in the generated file.
    pub already_wired: bool,
    /// Backup copy created.
    pub backup: Option<PathBuf>,
    /// `hl.monitor{…}` calls lifted from `hyprland.lua` and then commented
    /// out, each flattened onto a single line.
    pub adopted: Vec<String>,
    /// New contents of `hyprland.lua`.
    pub new_conf: String,
}

/// The Lua statement that pulls the generated file into `hyprland.lua`.
///
/// Hyprland's `package.path` only covers its own configuration directory,
/// so `require` is available exactly when both files are neighbours; a
/// generated file placed anywhere else is loaded by absolute path.
pub fn require_statement(hyprland_lua: &Path, monitors_lua: &Path) -> String {
    let module = monitors_lua.file_stem().and_then(|s| s.to_str());
    match (module, monitors_lua.parent(), hyprland_lua.parent()) {
        (Some(m), Some(dir), Some(conf_dir)) if dir == conf_dir => {
            format!("require({})", lua_string(m))
        }
        _ => format!(
            "dofile({})",
            lua_string(&monitors_lua.display().to_string())
        ),
    }
}

/// Computes the transformation to apply to `hyprland.lua`.
///
/// Kept separate from the write so the result can be shown to the user
/// before touching their file.
pub fn plan_init(conf: &str, hyprland_lua: &Path, monitors_lua: &Path) -> InitReport {
    let statement = require_statement(hyprland_lua, monitors_lua);
    let target = monitors_lua
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("monitors.lua");
    let module = monitors_lua
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("monitors");

    let already = conf.lines().any(|l| {
        let l = l.trim();
        !l.starts_with("--")
            && (l.contains("require(") || l.contains("dofile("))
            && (l.contains(module) || l.contains(target))
    });
    if already {
        return InitReport {
            already_wired: true,
            new_conf: conf.to_string(),
            ..Default::default()
        };
    }

    let mut adopted = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    // A `hl.monitor{…}` call routinely spans several lines: it is commented
    // out as a block, tracked by counting the delimiters it leaves open.
    let mut block: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut after_adopted = None;
    let mut after_require = None;

    for line in conf.lines() {
        let trimmed = line.trim();
        let opening = depth == 0 && !trimmed.starts_with("--") && trimmed.contains("hl.monitor(");

        if opening || depth > 0 {
            if opening {
                lines.push(t!("emit.adopted_comment", target = target).to_string());
                depth = delimiter_delta(line);
            } else {
                depth += delimiter_delta(line);
            }
            block.push(trimmed.to_string());
            lines.push(format!("-- {line}"));
            if depth <= 0 {
                depth = 0;
                adopted.push(block.join(" "));
                block.clear();
                after_adopted = Some(lines.len());
            }
            continue;
        }

        lines.push(line.to_string());
        if !trimmed.starts_with("--")
            && (trimmed.contains("require(") || trimmed.contains("dofile("))
        {
            after_require = Some(lines.len());
        }
    }

    // An unterminated call means the file was already broken; everything
    // read so far is commented out, so nothing is lost.
    if !block.is_empty() {
        adopted.push(block.join(" "));
    }

    // The statement takes the place of the monitor configuration it
    // replaces; failing that, it joins the other `require`s, and failing
    // that it opens the file.
    let insert_at = after_adopted.or(after_require).unwrap_or(0);
    lines.insert(insert_at, statement);

    let mut new_conf = lines.join("\n");
    if conf.ends_with('\n') || new_conf.is_empty() {
        new_conf.push('\n');
    }

    InitReport {
        already_wired: false,
        backup: None,
        adopted,
        new_conf,
    }
}

/// How many delimiters the line leaves open, trailing comment excluded.
fn delimiter_delta(line: &str) -> i32 {
    let code = line.split("--").next().unwrap_or("");
    code.chars().fold(0, |acc, c| match c {
        '(' | '{' => acc + 1,
        ')' | '}' => acc - 1,
        _ => acc,
    })
}

/// Wires `monitors.lua` into `hyprland.lua`, with a prior backup.
pub fn run_init(hyprland_lua: &Path, monitors_lua: &Path, dry_run: bool) -> Result<InitReport> {
    run_init_all(hyprland_lua, &[monitors_lua], dry_run)
}

/// Wires several generated files in one pass: one read, one backup, one write.
///
/// Doing them one at a time would back up `hyprland.lua` once per file, and
/// the second backup would be of the already-modified version — losing the
/// only copy of what the user actually wrote.
///
/// `already_wired` in the report means *everything* was already wired: one
/// file still missing its `require` is enough to make the pass worth running.
///
/// Order matters: `monitors.lua` must come first, since that pass is also the
/// one that adopts the `hl.monitor{…}` calls already in the file, and the
/// comment it leaves behind names the file they moved to.
pub fn run_init_all(hyprland_lua: &Path, generated: &[&Path], dry_run: bool) -> Result<InitReport> {
    let conf = std::fs::read_to_string(hyprland_lua).with_context(|| {
        t!("fs.read_failed", path = hyprland_lua.display().to_string()).to_string()
    })?;

    // Each pass plans against the output of the previous one, so the second
    // `require` lands next to the first rather than at the top of the file.
    let mut report = InitReport {
        already_wired: true,
        new_conf: conf,
        ..Default::default()
    };
    for target in generated {
        let step = plan_init(&report.new_conf, hyprland_lua, target);
        report.already_wired &= step.already_wired;
        report.adopted.extend(step.adopted);
        report.new_conf = step.new_conf;
    }

    if report.already_wired || dry_run {
        return Ok(report);
    }

    // Appended rather than substituted: `with_extension` would turn
    // `hyprland.lua` into `hyprland.hyprdmc.bak` and lose which file it
    // came from.
    let mut name = hyprland_lua.file_name().unwrap_or_default().to_os_string();
    name.push(".hyprdmc.bak");
    let backup = hyprland_lua.with_file_name(name);
    std::fs::copy(hyprland_lua, &backup)
        .with_context(|| t!("fs.backup_failed", path = backup.display().to_string()).to_string())?;
    report.backup = Some(backup);

    write_atomic(hyprland_lua, &report.new_conf)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::OutputState;
    use crate::monitor::{Mode, Rotation, Transform};

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

    const HYPRLAND_LUA: &str = "/home/u/.config/hypr/hyprland.lua";
    const MONITORS_LUA: &str = "/home/u/.config/hypr/monitors.lua";

    fn plan(conf: &str) -> InitReport {
        plan_init(conf, Path::new(HYPRLAND_LUA), Path::new(MONITORS_LUA))
    }

    #[test]
    fn render_emits_one_call_per_output() {
        let mut b = out("DP-1", 1920, 0);
        b.transform = Transform::new(Rotation::R90, false);
        let layout = Layout::new(vec![out("eDP-1", 0, 0), b]);
        let text = render(&layout);
        assert!(text.starts_with("-- Generated by hyprdmc"));
        assert!(text.contains("output = \"eDP-1\""));
        assert!(text.contains("position = \"1920x0\", scale = 1, transform = 1"));
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("hl.monitor("))
                .count(),
            2
        );
    }

    #[test]
    fn the_input_file_carries_only_input_settings() {
        let text = render_input(&crate::input::InputConfig {
            kb_layout: "fr".into(),
            kb_variant: "oss".into(),
            kb_options: String::new(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        });
        assert!(text.starts_with("-- Generated by hyprdmc"));
        assert!(text.contains(r#"kb_layout = "fr""#));
        assert!(
            !text.contains("hl.monitor("),
            "a screen has no business in input.lua"
        );
    }

    #[test]
    fn wiring_several_files_adds_one_require_each() {
        const INPUT_LUA: &str = "/home/u/.config/hypr/input.lua";
        let mut report = InitReport {
            already_wired: true,
            new_conf: "-- my config\n".to_string(),
            ..Default::default()
        };
        for target in [MONITORS_LUA, INPUT_LUA] {
            let step = plan_init(&report.new_conf, Path::new(HYPRLAND_LUA), Path::new(target));
            report.already_wired &= step.already_wired;
            report.new_conf = step.new_conf;
        }

        assert!(!report.already_wired);
        assert!(report.new_conf.contains(r#"require("monitors")"#));
        assert!(report.new_conf.contains(r#"require("input")"#));
    }

    #[test]
    fn a_file_already_wired_is_not_wired_twice() {
        let conf = "require(\"monitors\")\n".to_string();
        let step = plan_init(&conf, Path::new(HYPRLAND_LUA), Path::new(MONITORS_LUA));
        assert!(step.already_wired);
        assert_eq!(step.new_conf.matches(r#"require("monitors")"#).count(), 1);
    }

    #[test]
    fn render_disables_switched_off_screens() {
        let mut a = out("eDP-1", 0, 0);
        a.enabled = false;
        let text = render(&Layout::new(vec![a, out("DP-1", 0, 0)]));
        assert!(text.contains("hl.monitor({ output = \"eDP-1\", disabled = true })\n"));
    }

    #[test]
    fn generated_file_is_valid_lua_for_a_layout_with_quotes_in_it() {
        let mut a = out("desc:Acme \"27\"", 0, 0);
        a.mirror_of = Some("eDP-1".into());
        let text = render(&Layout::new(vec![a]));
        assert!(text.contains(r#"output = "desc:Acme \"27\"""#));
    }

    const SAMPLE: &str = r#"-- my config
require("colors")
require("binds")

hl.monitor({
    output   = "eDP-1",
    mode     = "preferred",
    position = "auto",
    scale    = "auto",
})

hl.config({ general = { gaps_in = 5 } })
"#;

    #[test]
    fn init_takes_the_place_of_the_monitor_configuration() {
        let report = plan(SAMPLE);
        let lines: Vec<&str> = report.new_conf.lines().collect();
        let req = lines
            .iter()
            .position(|l| l.trim() == r#"require("monitors")"#)
            .unwrap();
        let last_commented = lines.iter().rposition(|l| l.contains("-- })")).unwrap();
        assert_eq!(req, last_commented + 1);
    }

    #[test]
    fn init_adopts_a_multi_line_call_as_one_entry() {
        let report = plan(SAMPLE);
        assert_eq!(report.adopted.len(), 1);
        assert!(report.adopted[0].starts_with("hl.monitor({"));
        assert!(report.adopted[0].contains(r#"output   = "eDP-1","#));
        // Not a single live `hl.monitor` call left.
        assert!(
            !report
                .new_conf
                .lines()
                .any(|l| !l.trim().starts_with("--") && l.contains("hl.monitor("))
        );
    }

    #[test]
    fn init_adopts_a_single_line_call_too() {
        let report = plan("hl.monitor({ output = \"DP-1\", scale = 1 })\n");
        assert_eq!(
            report.adopted,
            vec!["hl.monitor({ output = \"DP-1\", scale = 1 })"]
        );
        assert!(
            report
                .new_conf
                .contains("-- hl.monitor({ output = \"DP-1\"")
        );
    }

    #[test]
    fn init_is_idempotent() {
        let first = plan(SAMPLE);
        let second = plan(&first.new_conf);
        assert!(second.already_wired);
        assert_eq!(second.new_conf, first.new_conf);
    }

    #[test]
    fn init_ignores_commented_out_calls() {
        let conf = "-- hl.monitor({ output = \"eDP-1\" })\n-- require(\"monitors\")\n";
        let report = plan(conf);
        assert!(!report.already_wired);
        assert!(report.adopted.is_empty());
        assert!(
            report
                .new_conf
                .lines()
                .any(|l| l.trim() == r#"require("monitors")"#)
        );
    }

    #[test]
    fn init_falls_back_to_the_last_require_then_to_the_top() {
        let after_require = plan("require(\"colors\")\nhl.config({})\n");
        assert_eq!(
            after_require.new_conf.lines().nth(1).unwrap(),
            r#"require("monitors")"#
        );

        let at_the_top = plan("hl.config({})\n");
        assert!(at_the_top.new_conf.starts_with("require(\"monitors\")\n"));
    }

    #[test]
    fn a_generated_file_outside_the_hypr_directory_is_loaded_by_path() {
        assert_eq!(
            require_statement(
                Path::new(HYPRLAND_LUA),
                Path::new("/srv/shared/screens.lua")
            ),
            r#"dofile("/srv/shared/screens.lua")"#
        );
    }

    #[test]
    fn delimiter_counting_ignores_trailing_comments() {
        assert_eq!(delimiter_delta("hl.monitor({"), 2);
        assert_eq!(delimiter_delta("}) -- (see above)"), -2);
        assert_eq!(delimiter_delta("hl.monitor({ output = \"X\" })"), 0);
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("hyprdmc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("monitors.lua");
        write_atomic(&target, "premier").unwrap();
        write_atomic(&target, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
