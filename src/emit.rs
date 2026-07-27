//! Persistence: generating the configuration files and wiring them into the
//! user's own configuration.
//!
//! Guiding principle: `hyprdmc` is the sole owner of the files it generates and
//! only touches the user's main configuration once, to add an include line.
//!
//! Nothing here knows a compositor's syntax. The directives, the comment marker,
//! the include statement and the shape of a directive worth adopting all come
//! from a [`Compositor`] plugin, which is what lets the same `init` and the same
//! `persist` serve Hyprland, sway, or whatever gets added next.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use rust_i18n::t;

use crate::compositor::Compositor;
use crate::config::home;
use crate::input::InputConfig;
use crate::layout::Layout;

/// Writes a file atomically: a sibling temporary file, then `rename`, so
/// that no reader ever sees partial content — the compositor could reload the
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

/// Prefixes every line of `text` with the compositor's comment marker.
///
/// The headers in `locales/app.yml` are plain sentences with no marker of their
/// own: one translated block serves every plugin, and the syntax is added here.
fn commented(c: &dyn Compositor, text: &str) -> String {
    text.lines()
        .map(|line| format!("{} {line}\n", c.comment()))
        .collect()
}

/// Renders the generated monitor configuration for a layout.
///
/// The main screen appears as a comment rather than as a directive: no
/// compositor here has a "primary output" keyword, and what makes a screen the
/// main one — being the output at (0, 0) — is already baked into the positions
/// below. The comment is there so someone reading the file can tell that the
/// origin was chosen rather than fallen into.
pub fn render(c: &dyn Compositor, layout: &Layout) -> String {
    let mut out = commented(c, &t!("emit.header"));
    if let Some(primary) = layout.primary_output() {
        out.push_str(&format!(
            "{} {}\n",
            c.comment(),
            t!("emit.primary_comment", name = &primary.name)
        ));
    }
    for directive in c.output_directives(layout) {
        out.push_str(&directive);
        out.push('\n');
    }
    out
}

/// Writes the layout to the generated monitor file.
pub fn persist(c: &dyn Compositor, layout: &Layout, path: &Path) -> Result<()> {
    write_atomic(path, &render(c, layout))
}

/// Renders the generated keyboard and pointer configuration.
pub fn render_input(c: &dyn Compositor, input: &InputConfig) -> String {
    let mut out = commented(c, &t!("emit.input_header"));
    for directive in c.input_directives(input) {
        out.push_str(&directive);
        out.push('\n');
    }
    out
}

/// Writes the keyboard and pointer settings to the generated input file.
pub fn persist_input(c: &dyn Compositor, input: &InputConfig, path: &Path) -> Result<()> {
    write_atomic(path, &render_input(c, input))
}

/// Replaces the home directory prefix with `~`, the way one would write it
/// by hand in a configuration file.
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
    /// The main configuration was already pulling in the generated file.
    pub already_wired: bool,
    /// Backup copy created.
    pub backup: Option<PathBuf>,
    /// Output directives lifted from the main configuration and then commented
    /// out, each flattened onto a single line.
    pub adopted: Vec<String>,
    /// New contents of the main configuration.
    pub new_conf: String,
}

/// Computes the transformation to apply to the user's main configuration.
///
/// Kept separate from the write so the result can be shown to the user
/// before touching their file.
pub fn plan_init(c: &dyn Compositor, conf: &str, main: &Path, generated: &Path) -> InitReport {
    let statement = c.include(main, generated);
    let target = generated
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("the generated file");

    let is_comment = |line: &str| line.trim_start().starts_with(c.comment());

    let already = conf
        .lines()
        .any(|l| !is_comment(l) && c.includes(l, generated));
    if already {
        return InitReport {
            already_wired: true,
            new_conf: conf.to_string(),
            ..Default::default()
        };
    }

    let mut adopted = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    // A directive may span several lines — a `hl.monitor{…}` call routinely
    // does — so it is commented out as a block, tracked by counting the
    // delimiters it leaves open. A syntax whose directives are always one line
    // simply closes at depth 0 on the first line and never enters the block.
    let mut block: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut after_adopted = None;
    let mut after_include = None;

    for line in conf.lines() {
        let trimmed = line.trim();
        let opening = depth == 0 && !is_comment(line) && c.opens_output(line);

        if opening || depth > 0 {
            if opening {
                lines.push(format!(
                    "{} {}",
                    c.comment(),
                    t!("emit.adopted_comment", target = target)
                ));
                depth = delimiter_delta(line, c.comment());
            } else {
                depth += delimiter_delta(line, c.comment());
            }
            block.push(trimmed.to_string());
            lines.push(format!("{} {line}", c.comment()));
            if depth <= 0 {
                depth = 0;
                adopted.push(block.join(" "));
                block.clear();
                after_adopted = Some(lines.len());
            }
            continue;
        }

        lines.push(line.to_string());
        if !is_comment(line) && c.is_include(line) {
            after_include = Some(lines.len());
        }
    }

    // An unterminated directive means the file was already broken; everything
    // read so far is commented out, so nothing is lost.
    if !block.is_empty() {
        adopted.push(block.join(" "));
    }

    // The statement takes the place of the monitor configuration it replaces;
    // failing that, it joins the other includes, and failing that it opens the
    // file.
    let insert_at = after_adopted.or(after_include).unwrap_or(0);
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
fn delimiter_delta(line: &str, comment: &str) -> i32 {
    let code = line.split(comment).next().unwrap_or("");
    code.chars().fold(0, |acc, ch| match ch {
        '(' | '{' => acc + 1,
        ')' | '}' => acc - 1,
        _ => acc,
    })
}

/// Wires one generated file into the main configuration, with a prior backup.
pub fn run_init(
    c: &dyn Compositor,
    main: &Path,
    generated: &Path,
    dry_run: bool,
) -> Result<InitReport> {
    run_init_all(c, main, &[generated], dry_run)
}

/// Wires several generated files in one pass: one read, one backup, one write.
///
/// Doing them one at a time would back up the main configuration once per file,
/// and the second backup would be of the already-modified version — losing the
/// only copy of what the user actually wrote.
///
/// `already_wired` in the report means *everything* was already wired: one
/// file still missing its include is enough to make the pass worth running.
///
/// Order matters: the monitor file must come first, since that pass is also the
/// one that adopts the output directives already in the file, and the comment it
/// leaves behind names the file they moved to.
pub fn run_init_all(
    c: &dyn Compositor,
    main: &Path,
    generated: &[&Path],
    dry_run: bool,
) -> Result<InitReport> {
    let conf = std::fs::read_to_string(main)
        .with_context(|| t!("fs.read_failed", path = main.display().to_string()).to_string())?;

    // Each pass plans against the output of the previous one, so the second
    // include lands next to the first rather than at the top of the file.
    let mut report = InitReport {
        already_wired: true,
        new_conf: conf,
        ..Default::default()
    };
    for target in generated {
        let step = plan_init(c, &report.new_conf, main, target);
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
    let mut name = main.file_name().unwrap_or_default().to_os_string();
    name.push(".hyprdmc.bak");
    let backup = main.with_file_name(name);
    std::fs::copy(main, &backup)
        .with_context(|| t!("fs.backup_failed", path = backup.display().to_string()).to_string())?;
    report.backup = Some(backup);

    write_atomic(main, &report.new_conf)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::hyprland::Hyprland;
    use crate::compositor::sway::Sway;
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
        plan_init(
            &Hyprland,
            conf,
            Path::new(HYPRLAND_LUA),
            Path::new(MONITORS_LUA),
        )
    }

    #[test]
    fn render_emits_one_directive_per_output() {
        let mut b = out("DP-1", 1920, 0);
        b.transform = Transform::new(Rotation::R90, false);
        let layout = Layout::new(vec![out("eDP-1", 0, 0), b]);
        let text = render(&Hyprland, &layout);
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
    fn the_same_layout_renders_for_another_compositor() {
        // The point of the seam: one layout, two syntaxes, no branch in here.
        let layout = Layout::new(vec![out("eDP-1", 0, 0), out("DP-1", 1920, 0)]);
        let text = render(&Sway, &layout);
        assert!(
            text.starts_with("# Generated by hyprdmc"),
            "the header takes the compositor's comment marker: {text}"
        );
        assert!(text.contains(r#"output "eDP-1" enable mode 1920x1080@60.000Hz position 0 0"#));
        assert_eq!(text.lines().filter(|l| l.starts_with("output ")).count(), 2);
        assert!(
            !text.contains("hl.monitor"),
            "no Lua leaked into a sway file"
        );
    }

    #[test]
    fn the_main_screen_is_recorded_as_a_comment_in_either_syntax() {
        let layout = Layout::new(vec![out("eDP-1", 0, 0), out("DP-1", 1920, 0)])
            .with_primary(Some("DP-1".into()));
        assert!(render(&Hyprland, &layout).contains("-- main screen: DP-1"));
        assert!(render(&Sway, &layout).contains("# main screen: DP-1"));

        let plain = render(&Hyprland, &Layout::new(vec![out("eDP-1", 0, 0)]));
        assert!(!plain.contains("main screen"));
    }

    #[test]
    fn the_input_file_carries_only_input_settings() {
        let input = InputConfig {
            kb_layout: "fr".into(),
            kb_variant: "oss".into(),
            kb_options: String::new(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        };
        let text = render_input(&Hyprland, &input);
        assert!(text.starts_with("-- Generated by hyprdmc"));
        assert!(text.contains(r#"kb_layout = "fr""#));
        assert!(
            !text.contains("hl.monitor("),
            "a screen has no business in the input file"
        );

        let sway = render_input(&Sway, &input);
        assert!(sway.starts_with("# Generated by hyprdmc"));
        assert!(sway.contains("input type:keyboard {"));
        assert!(sway.contains(r#"xkb_layout "fr""#));
    }

    #[test]
    fn wiring_several_files_adds_one_include_each() {
        const INPUT_LUA: &str = "/home/u/.config/hypr/inputs.lua";
        let mut report = InitReport {
            already_wired: true,
            new_conf: "-- my config\n".to_string(),
            ..Default::default()
        };
        for target in [MONITORS_LUA, INPUT_LUA] {
            let step = plan_init(
                &Hyprland,
                &report.new_conf,
                Path::new(HYPRLAND_LUA),
                Path::new(target),
            );
            report.already_wired &= step.already_wired;
            report.new_conf = step.new_conf;
        }

        assert!(!report.already_wired);
        assert!(report.new_conf.contains(r#"require("monitors")"#));
        assert!(report.new_conf.contains(r#"require("inputs")"#));
    }

    #[test]
    fn a_file_already_wired_is_not_wired_twice() {
        let step = plan("require(\"monitors\")\n");
        assert!(step.already_wired);
        assert_eq!(step.new_conf.matches(r#"require("monitors")"#).count(), 1);
    }

    #[test]
    fn render_disables_switched_off_screens() {
        let mut a = out("eDP-1", 0, 0);
        a.enabled = false;
        let layout = Layout::new(vec![a, out("DP-1", 0, 0)]);
        assert!(
            render(&Hyprland, &layout)
                .contains("hl.monitor({ output = \"eDP-1\", disabled = true })\n")
        );
        assert!(render(&Sway, &layout).contains("output \"eDP-1\" disable\n"));
    }

    #[test]
    fn generated_file_is_valid_for_a_layout_with_quotes_in_it() {
        let mut a = out("desc:Acme \"27\"", 0, 0);
        a.mirror_of = Some("eDP-1".into());
        let text = render(&Hyprland, &Layout::new(vec![a]));
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
    fn init_adopts_a_one_line_syntax_without_delimiters_too() {
        // sway's directives never open a brace, so the block tracker has to
        // close them on their own line rather than swallowing the rest of the
        // file. This is the case a Lua-shaped `plan_init` would have got wrong.
        let conf = "# my config\ninclude colors\noutput DP-1 mode 1920x1080 position 0 0\nbindsym Mod4+Return exec foot\n";
        let generated = Path::new("/home/u/.config/sway/monitors.conf");
        let report = plan_init(
            &Sway,
            conf,
            Path::new("/home/u/.config/sway/config"),
            generated,
        );

        assert_eq!(
            report.adopted,
            vec!["output DP-1 mode 1920x1080 position 0 0"]
        );
        assert!(
            report.new_conf.contains("# output DP-1 mode"),
            "the directive is commented out: {}",
            report.new_conf
        );
        assert!(
            report.new_conf.contains("bindsym Mod4+Return exec foot"),
            "everything after it must survive untouched: {}",
            report.new_conf
        );
        // The include the plugin wrote is one the plugin recognises again, which
        // is what makes `init` idempotent whatever the syntax.
        let added = report
            .new_conf
            .lines()
            .find(|l| Sway.includes(l, generated))
            .unwrap_or_else(|| panic!("no include was added: {}", report.new_conf));
        assert!(added.starts_with("include "), "{added}");
        assert!(
            plan_init(&Sway, &report.new_conf, Path::new("/x/config"), generated).already_wired
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
    fn init_falls_back_to_the_last_include_then_to_the_top() {
        let after_include = plan("require(\"colors\")\nhl.config({})\n");
        assert_eq!(
            after_include.new_conf.lines().nth(1).unwrap(),
            r#"require("monitors")"#
        );

        let at_the_top = plan("hl.config({})\n");
        assert!(at_the_top.new_conf.starts_with("require(\"monitors\")\n"));
    }

    #[test]
    fn a_generated_file_outside_the_config_directory_is_loaded_by_path() {
        assert_eq!(
            Hyprland.include(
                Path::new(HYPRLAND_LUA),
                Path::new("/srv/shared/screens.lua")
            ),
            r#"dofile("/srv/shared/screens.lua")"#
        );
    }

    #[test]
    fn delimiter_counting_ignores_trailing_comments() {
        assert_eq!(delimiter_delta("hl.monitor({", "--"), 2);
        assert_eq!(delimiter_delta("}) -- (see above)", "--"), -2);
        assert_eq!(delimiter_delta("hl.monitor({ output = \"X\" })", "--"), 0);
        // A `#` comment must not be cut at a Lua marker, and vice versa.
        assert_eq!(delimiter_delta("output DP-1 # (note)", "#"), 0);
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
