//! Entry point: translates commands into operations on the layout.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_BORDERS_ONLY};
use rust_i18n::t;

// `main.rs` is its own crate root (the binary target), separate from the
// `hyprdmc` library crate: `t!()` needs this invocation here too, even
// though the library already has one in `src/lib.rs`.
rust_i18n::i18n!("locales", fallback = "en");

use hyprdmc::apply::{self, ApplyReport};
use hyprdmc::browser;
use hyprdmc::cli::{Cli, Command, HistoryAction, ProfileAction, SafetyArgs, SetArgs, WebArgs};
use hyprdmc::compositor::{self, Compositor};
use hyprdmc::config::{Config, OutputRule, Profile, config_path, parse_position};
use hyprdmc::daemon::{self, AppState};
use hyprdmc::emit;
use hyprdmc::history::Store;
use hyprdmc::layout::{Layout, Relation, Severity, format_scale};
use hyprdmc::monitor::{Mode, Monitor, Rotation, Transform};
use hyprdmc::session::Session;

#[tokio::main]
async fn main() -> Result<()> {
    // Language first, so that even a failure reported below is readable.
    // A broken config must not prevent the program from speaking: fall back
    // to the environment rather than propagating the error here — the real
    // parse error surfaces later, when the command actually needs the config.
    let preference = Config::load().ok().and_then(|c| c.settings.language);
    hyprdmc::i18n::init(preference.as_deref());

    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::List { json } => cmd_list(json),
        Command::Modes { output } => cmd_modes(&output),
        Command::Set(args) => cmd_set(args),
        Command::Arrange { spec, safety } => cmd_arrange(&spec, safety),
        Command::Auto { safety } => cmd_auto(safety),
        Command::Primary {
            output,
            none,
            safety,
        } => cmd_primary(output.as_deref(), none, safety),
        Command::Compositor => cmd_compositor(),
        Command::Profile { action } => cmd_profile(action),
        Command::Apply { safety } => cmd_apply_for_hardware(safety),
        Command::Persist => cmd_persist(),
        Command::History { action } => cmd_history(action),
        Command::Init { dry_run } => cmd_init(dry_run),
        Command::Daemon { web, no_web } => cmd_daemon(web, !no_web).await,
        Command::Web { web } => cmd_web(web).await,
        Command::Service { action } => cmd_service(action),
    }
}

// --------------------------------------------------------------- service --

/// Installs, removes or inspects the systemd user service.
///
/// No argument means `install`: that is what someone typing `hyprdmc service`
/// is after, and the command is idempotent — rewriting the same unit costs
/// nothing.
fn cmd_service(action: Option<hyprdmc::cli::ServiceAction>) -> Result<()> {
    use hyprdmc::cli::ServiceAction;
    use hyprdmc::service;

    match action.unwrap_or(ServiceAction::Install {
        enable: false,
        wanted_by: service::DEFAULT_TARGET.to_string(),
        dry_run: false,
    }) {
        ServiceAction::Install {
            enable,
            wanted_by,
            dry_run: true,
        } => {
            let _ = enable;
            print!(
                "{}",
                service::render_unit(&std::env::current_exe()?, &wanted_by)
            );
            println!(
                "{}",
                t!(
                    "cli.service.dry_run",
                    path = service::unit_path().display().to_string()
                )
            );
        }

        ServiceAction::Install {
            enable, wanted_by, ..
        } => {
            let done = service::install(&wanted_by, enable)?;
            println!(
                "{}",
                t!(
                    "cli.service.installed",
                    path = done.path.display().to_string()
                )
            );
            if done.enabled {
                println!("{}", t!("cli.service.enabled"));
            } else {
                println!("{}", t!("cli.service.enable_hint", unit = service::UNIT));
            }
            // A unit whose target never activates is a unit that never runs,
            // and nothing about it looks wrong until you reboot and wonder.
            if done.target_inactive {
                println!("{}", t!("cli.service.target_inactive", target = wanted_by));
            }
        }

        ServiceAction::Uninstall => match service::uninstall()? {
            Some(path) => println!(
                "{}",
                t!("cli.service.uninstalled", path = path.display().to_string())
            ),
            None => println!("{}", t!("cli.service.not_installed")),
        },

        ServiceAction::Status => {
            let status = service::status();
            let state = |yes: bool| {
                if yes {
                    t!("cli.service.yes")
                } else {
                    t!("cli.service.no")
                }
            };
            println!(
                "{}",
                t!(
                    "cli.service.status",
                    path = status.path.display().to_string(),
                    installed = state(status.installed),
                    enabled = state(status.enabled),
                    active = state(status.active)
                )
            );
        }
    }
    Ok(())
}

fn init_tracing(verbose: bool) {
    let default = if verbose {
        "hyprdmc=debug"
    } else {
        "hyprdmc=info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("HYPRDMC_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

/// Opens a session on the running compositor, through whichever plugin applies.
///
/// The error names the compositor: "Hyprland is not reachable" and "sway is not
/// reachable" send the user to different places.
fn session() -> Result<Box<dyn Session>> {
    let cfg = Config::load()?;
    let compositor = cfg.compositor()?;
    compositor
        .connect()
        .with_context(|| t!("compositor.unreachable", name = compositor.label()).to_string())
}

// ------------------------------------------------------------------ reading --

fn cmd_list(json: bool) -> Result<()> {
    let monitors = session()?.outputs()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&monitors)?);
        return Ok(());
    }
    if monitors.is_empty() {
        println!("{}", t!("cli.no_outputs"));
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            t!("cli.table.output").to_string(),
            t!("cli.table.state").to_string(),
            t!("cli.table.mode").to_string(),
            t!("cli.table.position").to_string(),
            t!("cli.table.scale").to_string(),
            t!("cli.table.orientation").to_string(),
            t!("cli.table.identifier").to_string(),
        ]);

    // Marked in the table rather than printed on its own line: what you want to
    // know is which of these screens is the main one, not that one exists.
    let primary = primary_of(&monitors);
    for m in &monitors {
        let state = if m.disabled {
            t!("cli.state.disabled").to_string()
        } else if let Some(target) = m.mirror_target(&monitors) {
            t!("cli.state.mirror", target = target).to_string()
        } else if m.focused {
            t!("cli.state.focused").to_string()
        } else {
            t!("cli.state.active").to_string()
        };
        let dash = |s: String| if m.disabled { "—".to_string() } else { s };
        let name = if primary.as_deref() == Some(m.name.as_str()) {
            format!("{} ★", m.name)
        } else {
            m.name.clone()
        };
        table.add_row(vec![
            Cell::new(name),
            Cell::new(state),
            Cell::new(dash(m.mode().to_string())),
            Cell::new(dash(format!("{}x{}", m.x, m.y))),
            Cell::new(format_scale(m.scale)),
            Cell::new(m.transform().to_string()),
            Cell::new(m.fingerprint()),
        ]);
    }
    println!("{table}");
    // A star nobody explained is just a star.
    if primary.is_some() {
        println!("{}", t!("cli.primary.legend"));
    }

    print_issues(&Layout::from_monitors(&monitors).with_primary(primary));
    Ok(())
}

fn print_issues(layout: &Layout) {
    for issue in layout.validate() {
        eprintln!("{}: {}", severity_label(issue.severity), issue.message);
    }
}

fn severity_label(severity: Severity) -> String {
    match severity {
        Severity::Error => t!("layout.severity.error").to_string(),
        Severity::Warning => t!("layout.severity.warning").to_string(),
    }
}

fn cmd_modes(output: &str) -> Result<()> {
    let monitors = session()?.outputs()?;
    let m = find(&monitors, output)?;
    if m.available_modes.is_empty() {
        println!("{}", t!("cli.no_modes", name = output));
        return Ok(());
    }
    let current = m.mode();
    for mode in m.parsed_modes() {
        let is_current = mode.width == current.width
            && mode.height == current.height
            && (mode.refresh - current.refresh).abs() < 0.5;
        let marker = if is_current {
            format!(" ({})", t!("cli.current"))
        } else {
            String::new()
        };
        println!("{mode}{marker}");
    }
    Ok(())
}

fn find<'a>(monitors: &'a [Monitor], name: &str) -> Result<&'a Monitor> {
    monitors.iter().find(|m| m.name == name).ok_or_else(|| {
        let known = monitors
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow!(t!("cli.unknown_output", name = name, known = known).to_string())
    })
}

// -------------------------------------------------------------- modification --

fn cmd_set(args: SetArgs) -> Result<()> {
    let live = session()?;
    let monitors = live.outputs()?;
    let target = find(&monitors, &args.output)?;
    let preferred = target.preferred_mode();

    // The main screen comes along but the layout is not re-anchored: `set`
    // changes one field of one output, and silently shifting every other screen
    // would be a surprise.
    let mut layout = Layout::from_monitors(&monitors).with_primary(primary_of(&monitors));
    {
        let o = layout
            .get_mut(&args.output)
            .expect("the output was just checked");

        if let Some(mode) = &args.mode {
            o.mode = match mode.as_str() {
                "preferred" | "auto" => None,
                other => Some(other.parse::<Mode>()?),
            };
        }
        if let Some(pos) = &args.pos {
            let (x, y) = parse_position(pos)?;
            o.x = x;
            o.y = y;
        }
        if let Some(scale) = args.scale {
            o.scale = scale;
        }
        if let Some(deg) = &args.rotate {
            o.transform = Transform::new(
                Rotation::from_degrees(deg.parse::<u16>()?)?,
                o.transform.flipped,
            );
        }
        if args.flip {
            o.transform.flipped = true;
        }
        if args.no_flip {
            o.transform.flipped = false;
        }
        if let Some(mirror) = &args.mirror {
            o.mirror_of = Some(mirror.clone());
        }
        if args.no_mirror {
            o.mirror_of = None;
        }
        if args.enable {
            o.enabled = true;
        }
        if args.disable {
            o.enabled = false;
        }
        if let Some(vrr) = args.vrr {
            o.vrr = vrr;
        }

        // The logical size — and therefore overlap detection — depends on the
        // mode: it must be resolved before validating.
        if o.mode.is_none() {
            o.mode = preferred;
        }
    }

    apply_interactively(live.as_ref(), &layout, args.safety, None)?;

    if let Some(name) = &args.save {
        save_profile(name, false, &layout, &monitors)?;
        println!("{}", t!("cli.profile_saved", name = name));
    }
    Ok(())
}

fn cmd_arrange(spec: &[String], safety: SafetyArgs) -> Result<()> {
    if !spec.len().is_multiple_of(3) {
        bail!(t!("cli.arrange_expects_triples", count = spec.len()).to_string());
    }
    let live = session()?;
    let monitors = live.outputs()?;
    let mut layout = Layout::from_monitors(&monitors).with_primary(primary_of(&monitors));

    for triple in spec.chunks(3) {
        let relation: Relation = triple[1].parse()?;
        layout.place(&triple[0], relation, &triple[2])?;
    }
    layout.normalize();

    apply_interactively(live.as_ref(), &layout, safety, None)
}

fn cmd_auto(safety: SafetyArgs) -> Result<()> {
    let live = session()?;
    let monitors = live.outputs()?;
    let mut layout = Layout::from_monitors(&monitors).with_primary(primary_of(&monitors));
    layout.auto_arrange();
    apply_interactively(live.as_ref(), &layout, safety, None)
}

/// The connector the configured main screen resolves to, if any.
///
/// Every command that builds a layout goes through this, so the anchor and the
/// focus follow the user's choice whether the layout came from a profile, from
/// `arrange`, or from a single `set`.
///
/// A configuration we cannot read means no main screen rather than a failure:
/// the real parse error is reported by whatever else needs the file.
fn primary_of(monitors: &[Monitor]) -> Option<String> {
    Config::load().ok()?.primary_output(monitors)
}

/// Lists the compositor plugins, marking the one in force.
///
/// Live-apply support is spelled out per plugin: it is the difference between
/// "hyprdmc drives your session" and "hyprdmc writes your configuration file",
/// and nothing else on screen would tell you which one you get.
fn cmd_compositor() -> Result<()> {
    let cfg = Config::load()?;
    let active = cfg.compositor()?;
    println!("{}", describe_compositor(&cfg, active));
    println!();

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            t!("cli.table.compositor").to_string(),
            t!("cli.table.active").to_string(),
            t!("cli.table.detected").to_string(),
            t!("cli.table.live_apply").to_string(),
            t!("cli.table.generated").to_string(),
        ]);

    let mark = |yes: bool| {
        if yes {
            t!("cli.yes").to_string()
        } else {
            t!("cli.no").to_string()
        }
    };
    for plugin in compositor::all() {
        table.add_row(vec![
            Cell::new(plugin.label()),
            Cell::new(if plugin.id() == active.id() {
                t!("cli.active_marker").to_string()
            } else {
                String::new()
            }),
            Cell::new(mark(plugin.running())),
            Cell::new(mark(plugin.drives_sessions())),
            Cell::new(format!(
                "{}, {}",
                plugin.monitors_file(),
                plugin.input_file()
            )),
        ]);
    }
    println!("{table}");
    println!("{}", t!("cli.compositor.hint"));
    Ok(())
}

/// Shows or changes the main screen.
fn cmd_primary(output: Option<&str>, none: bool, safety: SafetyArgs) -> Result<()> {
    let live = session()?;
    let monitors = live.outputs()?;
    let mut cfg = Config::load()?;

    if none {
        cfg.settings.primary = None;
        cfg.save()?;
        println!("{}", t!("cli.primary.cleared"));
        return Ok(());
    }

    let Some(wanted) = output else {
        match (&cfg.settings.primary, cfg.primary_output(&monitors)) {
            (Some(pattern), Some(name)) => {
                println!(
                    "{}",
                    t!("cli.primary.current", name = name, pattern = pattern)
                );
            }
            // Set, but the screen it names is not plugged in right now.
            (Some(pattern), None) => {
                println!("{}", t!("cli.primary.absent", pattern = pattern));
            }
            _ => println!("{}", t!("cli.primary.none")),
        }
        return Ok(());
    };

    let target = monitors
        .iter()
        .find(|m| hyprdmc::config::matches_pattern(wanted, m))
        .ok_or_else(|| {
            let known = monitors
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!(t!("cli.primary.unknown", name = wanted, known = known).to_string())
        })?;

    // Recorded by fingerprint, like a profile rule: the choice is about a screen,
    // not about the port it happens to be plugged into today.
    cfg.settings.primary = Some(target.fingerprint());
    cfg.save()?;
    println!(
        "{}",
        t!(
            "cli.primary.set",
            name = target.name,
            pattern = target.fingerprint()
        )
    );

    // Applied straight away: the point of naming a main screen is that the
    // workspace gets rebuilt around it, and printing "noted" while nothing moves
    // would leave the user wondering what the setting did.
    let mut layout = Layout::from_monitors(&monitors).with_primary(Some(target.name.clone()));
    layout.normalize();
    apply_interactively(live.as_ref(), &layout, safety, None)
}

/// Applies a layout then, in a terminal, offers to revert — this is what
/// avoids being stuck in front of a black screen.
fn apply_interactively(
    live: &dyn Session,
    layout: &Layout,
    safety: SafetyArgs,
    profile: Option<&str>,
) -> Result<()> {
    let compositor = Config::load()?.compositor()?;
    let previous = apply::snapshot(live)?;
    let report = apply::apply(live, compositor, layout, safety.force)?;
    print_report(&report);

    if report.rolled_back {
        bail!(t!("apply.not_applied").to_string());
    }

    // Skipping the prompt means "do not ask", not "do not record": a layout
    // applied with --no-confirm is still one the user may want to undo.
    if !safety.no_confirm {
        let timeout = Duration::from_secs(Config::load()?.settings.confirm_timeout_secs);
        if !apply::confirm_or_revert(live, compositor, &previous, timeout)? {
            println!("{}", t!("apply.reverted"));
            return Ok(());
        }
    }

    // Only file a layout that survived: the history is an undo list, and an
    // entry the user already rejected has no business in it.
    remember(live, layout, profile);
    Ok(())
}

/// Files an applied layout in the history and the recall map.
///
/// Best-effort: failing to record must not turn a successful apply into an
/// error, so problems are logged and swallowed.
fn remember(live: &dyn Session, layout: &Layout, profile: Option<&str>) {
    let Ok(monitors) = live.outputs() else {
        return;
    };
    let mut store = Store::load();
    store.record(hyprdmc::history::Snapshot::new(
        layout.clone(),
        hyprdmc::history::signature(&monitors),
        profile.map(str::to_string),
    ));
    if let Err(err) = store.save() {
        tracing::warn!("could not record the layout: {err:#}");
    }
}

// ---------------------------------------------------------------- history --

fn cmd_history(action: Option<HistoryAction>) -> Result<()> {
    match action.unwrap_or(HistoryAction::List) {
        HistoryAction::List => cmd_history_list(),

        HistoryAction::Restore { index, safety } => {
            let store = Store::load();
            let snapshot = store
                .entry(index)
                .ok_or_else(|| anyhow!(t!("history.unknown_entry", index = index).to_string()))?;
            let when = snapshot.age_label();
            let live = session()?;
            apply_interactively(
                live.as_ref(),
                &snapshot.layout,
                safety,
                snapshot.profile.as_deref(),
            )?;
            println!("{}", t!("history.restored", index = index, when = when));
            Ok(())
        }

        HistoryAction::Clear => {
            let mut store = Store::load();
            store.clear();
            store.save()?;
            println!("{}", t!("history.cleared"));
            Ok(())
        }
    }
}

fn cmd_history_list() -> Result<()> {
    let store = Store::load();
    if store.history.is_empty() {
        println!("{}", t!("history.empty"));
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            t!("history.table.position").to_string(),
            t!("history.table.when").to_string(),
            t!("history.table.origin").to_string(),
            t!("history.table.layout").to_string(),
        ]);

    for (index, snapshot) in store.history.iter().enumerate() {
        table.add_row(vec![
            Cell::new(index),
            Cell::new(snapshot.age_label()),
            Cell::new(
                snapshot
                    .profile
                    .clone()
                    .unwrap_or_else(|| t!("history.origin.manual").to_string()),
            ),
            Cell::new(snapshot.describe()),
        ]);
    }
    println!("{table}");
    println!("{}", t!("history.recall_known", count = store.recall.len()));
    Ok(())
}

fn print_report(report: &ApplyReport) {
    for issue in report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
    {
        eprintln!("{}: {}", t!("layout.severity.warning"), issue.message);
    }
    for drift in &report.drifts {
        eprintln!("{}: {}", severity_label(drift.severity), drift.message);
    }
    if report.succeeded() {
        for call in &report.specs {
            println!("{call}");
        }
    }
}

// ------------------------------------------------------------------ profiles --

fn cmd_profile(action: ProfileAction) -> Result<()> {
    match action {
        ProfileAction::List => cmd_profile_list(),

        ProfileAction::Show { name } => {
            let cfg = Config::load()?;
            let profile = cfg
                .profile(&name)
                .ok_or_else(|| anyhow!(t!("config.unknown_profile", name = name).to_string()))?;
            print!("{}", toml::to_string_pretty(profile)?);
            Ok(())
        }

        ProfileAction::Save { name, exact } => {
            let monitors = session()?.outputs()?;
            save_profile(&name, exact, &Layout::from_monitors(&monitors), &monitors)?;
            let path = config_path().display().to_string();
            println!("{}", t!("cli.profile_saved_in", name = name, path = path));
            Ok(())
        }

        ProfileAction::Apply { name, safety } => {
            let live = session()?;
            let monitors = live.outputs()?;
            let cfg = Config::load()?;
            let profile = cfg
                .profile(&name)
                .ok_or_else(|| anyhow!(t!("config.unknown_profile", name = name).to_string()))?;
            let mut layout = profile
                .resolve(&monitors)?
                .with_primary(cfg.primary_output(&monitors));
            layout.normalize();
            apply_interactively(live.as_ref(), &layout, safety, Some(&name))
        }

        ProfileAction::Delete { name } => {
            let mut cfg = Config::load()?;
            cfg.remove(&name)?;
            cfg.save()?;
            println!("{}", t!("cli.profile_deleted", name = name));
            Ok(())
        }

        ProfileAction::Rename { from, to } => {
            let mut cfg = Config::load()?;
            let renamed = Profile {
                name: to.clone(),
                ..cfg
                    .profile(&from)
                    .ok_or_else(|| anyhow!(t!("config.unknown_profile", name = from).to_string()))?
                    .clone()
            };
            cfg.remove(&from)?;
            cfg.upsert(renamed);
            cfg.save()?;
            println!("{}", t!("cli.profile_renamed", from = from, to = to));
            Ok(())
        }
    }
}

fn cmd_profile_list() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.profiles.is_empty() {
        let path = config_path().display().to_string();
        println!("{}", t!("cli.no_profiles", path = path));
        return Ok(());
    }

    // The list must stay readable even without Hyprland running.
    let monitors = session().and_then(|s| s.outputs()).unwrap_or_default();
    let active = cfg.best_match(&monitors).map(|p| p.name.clone());

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            t!("cli.table.profile").to_string(),
            t!("cli.table.outputs").to_string(),
            t!("cli.table.exact").to_string(),
            t!("cli.table.matches").to_string(),
        ]);
    for p in &cfg.profiles {
        table.add_row(vec![
            Cell::new(&p.name),
            Cell::new(
                p.outputs
                    .iter()
                    .map(|o| o.pattern.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Cell::new(if p.exact {
                t!("cli.yes").to_string()
            } else {
                t!("cli.no").to_string()
            }),
            Cell::new(if active.as_deref() == Some(p.name.as_str()) {
                t!("cli.active_marker").to_string()
            } else if p.matches(&monitors) {
                t!("cli.yes").to_string()
            } else {
                t!("cli.no").to_string()
            }),
        ]);
    }
    println!("{table}");
    Ok(())
}

/// Saves a layout identifying outputs by their fingerprint, so that the
/// profile survives a change of connector.
fn save_profile(name: &str, exact: bool, layout: &Layout, monitors: &[Monitor]) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.upsert(Profile {
        name: name.to_string(),
        exact,
        outputs: layout
            .outputs
            .iter()
            .map(|o| OutputRule::from_state(o, monitors.iter().find(|m| m.name == o.name)))
            .collect(),
    });
    cfg.save()?;
    Ok(())
}

fn cmd_apply_for_hardware(safety: SafetyArgs) -> Result<()> {
    let live = session()?;
    let monitors = live.outputs()?;
    let cfg = Config::load()?;

    let mut chosen: Option<String> = None;
    let primary = cfg.primary_output(&monitors);
    let mut layout = match cfg.best_match(&monitors) {
        Some(profile) => {
            println!("{}", t!("cli.matching_profile", name = &profile.name));
            chosen = Some(profile.name.clone());
            profile.resolve(&monitors)?.with_primary(primary)
        }
        None => {
            println!("{}", t!("cli.no_matching_profile"));
            let mut layout = Layout::from_monitors(&monitors).with_primary(primary);
            layout.auto_arrange();
            layout
        }
    };
    // A profile positions its outputs relative to one another; anchoring the set
    // on the main screen is what turns that into absolute coordinates.
    layout.normalize();
    apply_interactively(live.as_ref(), &layout, safety, chosen.as_deref())
}

// ----------------------------------------------------------------- persistence --

fn cmd_persist() -> Result<()> {
    let monitors = session()?.outputs()?;
    let cfg = Config::load()?;
    let compositor = cfg.compositor()?;
    println!("{}", describe_compositor(&cfg, compositor));
    emit::persist(
        compositor,
        &Layout::from_monitors(&monitors).with_primary(cfg.primary_output(&monitors)),
        &cfg.settings.monitors_lua,
    )?;
    let path = cfg.settings.monitors_lua.display().to_string();
    println!("{}", t!("cli.persisted", path = path));

    if !requires_generated_file(
        compositor,
        &compositor.main_config(),
        &cfg.settings.monitors_lua,
    ) {
        let path = compositor.main_config().display().to_string();
        println!("{}", t!("cli.not_required", path = path));
    }
    Ok(())
}

/// Is the compositor's own configuration already pulling in the file we generate?
fn requires_generated_file(
    compositor: &dyn Compositor,
    main: &std::path::Path,
    generated: &std::path::Path,
) -> bool {
    std::fs::read_to_string(main).is_ok_and(|c| {
        c.lines().any(|l| {
            !l.trim().starts_with(compositor.comment()) && compositor.includes(l, generated)
        })
    })
}

/// Names the plugin in force and where the choice came from.
///
/// Worth a line of output: which file gets written, and in which syntax, is the
/// one thing about `persist` and `init` a user cannot guess.
fn describe_compositor(cfg: &Config, compositor: &dyn Compositor) -> String {
    let origin = if cfg.settings.compositor.is_some() {
        t!("compositor.origin.configured")
    } else {
        t!("compositor.origin.detected")
    };
    t!(
        "compositor.detected",
        name = compositor.label(),
        origin = origin
    )
    .to_string()
}

fn cmd_init(dry_run: bool) -> Result<()> {
    let cfg = Config::load()?;
    let compositor = cfg.compositor()?;
    println!("{}", describe_compositor(&cfg, compositor));
    let hypr_conf = compositor.main_config();
    let report = emit::run_init_all(
        compositor,
        &hypr_conf,
        &[&cfg.settings.monitors_lua, &cfg.settings.input_lua],
        dry_run,
    )?;

    if report.already_wired {
        let conf = hypr_conf.display().to_string();
        let target = cfg.settings.monitors_lua.display().to_string();
        println!(
            "{}",
            t!("cli.init.already_wired", conf = conf, target = target)
        );
        return Ok(());
    }

    if dry_run {
        let path = hypr_conf.display().to_string();
        println!("{}", t!("cli.init.dry_run_header", path = path));
        print!("{}", report.new_conf);
        return Ok(());
    }

    if let Some(backup) = &report.backup {
        let path = backup.display().to_string();
        println!("{}", t!("cli.init.backup", path = path));
    }
    println!(
        "{}",
        t!(
            "cli.init.source_added",
            statement = compositor.include(&hypr_conf, &cfg.settings.monitors_lua)
        )
    );
    if !report.adopted.is_empty() {
        println!("{}", t!("cli.init.adopted", count = report.adopted.len()));
    }

    // The current state is written right away: on the next reload, the
    // display and the keyboard must stay exactly as they are now.
    let live = session()?;
    let monitors = live.outputs()?;
    emit::persist(
        compositor,
        &Layout::from_monitors(&monitors).with_primary(cfg.primary_output(&monitors)),
        &cfg.settings.monitors_lua,
    )?;
    let path = cfg.settings.monitors_lua.display().to_string();
    println!("{}", t!("cli.persisted", path = path));

    emit::persist_input(compositor, &live.read_input()?, &cfg.settings.input_lua)?;
    let input_path = cfg.settings.input_lua.display().to_string();
    println!("{}", t!("cli.persisted", path = input_path));
    println!("{}", t!("cli.init.reload_hint"));
    Ok(())
}

// ---------------------------------------------------------------------- daemon --

fn web_addr(cfg: &Config, args: &WebArgs) -> Result<SocketAddr> {
    let ip: IpAddr = args
        .bind
        .as_deref()
        .unwrap_or(&cfg.settings.bind)
        .parse()
        .context(t!("cli.invalid_bind").to_string())?;
    Ok(SocketAddr::new(
        ip,
        args.port.unwrap_or(cfg.settings.web_port),
    ))
}

/// Starts the web server and hands back the URL it is reachable at.
///
/// The listener is bound here rather than inside the server task so that the
/// browser is only pointed at the port once it is genuinely accepting
/// connections, and so that `--port 0` resolves to the real port.
async fn start_web(state: &Arc<AppState>, addr: SocketAddr) -> Result<String> {
    let listener = hyprdmc::web::bind(addr).await?;
    let url = browser::reachable_url(listener.local_addr()?);
    tracing::info!("web interface: {url}");

    let web_state = Arc::clone(state);
    tokio::spawn(async move {
        if let Err(err) = hyprdmc::web::serve_on(listener, web_state).await {
            tracing::error!("web interface stopped: {err:#}");
        }
    });
    Ok(url)
}

/// Opens the UI in a browser, reporting rather than failing if it cannot.
///
/// The server is up and the URL has been printed either way, so a missing
/// `xdg-open` or a headless session is an inconvenience, not an error.
fn offer_browser(url: &str) {
    match browser::open(url) {
        Ok(()) => println!("{}", t!("cli.web.opening", url = url)),
        Err(err) => println!(
            "{}",
            t!("cli.web.open_failed", url = url, error = err.to_string())
        ),
    }
}

async fn cmd_daemon(args: WebArgs, with_web: bool) -> Result<()> {
    let state = daemon::bootstrap()?;
    let addr = web_addr(&*state.config.read().await, &args)?;

    if with_web {
        let url = start_web(&state, addr).await?;
        println!("{}", t!("cli.web.listening", url = &url));
        // A daemon usually starts with the session: opening a browser then
        // would be intrusive, so it takes an explicit --open.
        if args.should_open(false) {
            offer_browser(&url);
        }
    }

    tracing::info!("hotplug monitoring active");
    daemon::run(state).await
}

async fn cmd_web(args: WebArgs) -> Result<()> {
    let state = daemon::bootstrap()?;
    let addr = web_addr(&*state.config.read().await, &args)?;

    let url = start_web(&state, addr).await?;
    println!("{}", t!("cli.web.listening", url = &url));
    // `web` exists to be used interactively: opening the page is the point.
    if args.should_open(true) {
        offer_browser(&url);
    }

    // Nothing else to do here: the server owns the process until Ctrl-C.
    tokio::signal::ctrl_c().await?;
    Ok(())
}
