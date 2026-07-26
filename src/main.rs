//! Point d'entrée : traduction des commandes en opérations sur l'agencement.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_BORDERS_ONLY};

use hyprmc::apply::{self, ApplyReport};
use hyprmc::cli::{Cli, Command, ProfileAction, SafetyArgs, SetArgs, WebArgs};
use hyprmc::config::{Config, OutputRule, Profile, config_path, hyprland_conf, parse_position};
use hyprmc::daemon::{self};
use hyprmc::emit;
use hyprmc::ipc::{HyprBackend, HyprSocket};
use hyprmc::layout::{Layout, Relation, Severity, format_scale};
use hyprmc::monitor::{Mode, Monitor, Rotation, Transform};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::List { json } => cmd_list(json),
        Command::Modes { output } => cmd_modes(&output),
        Command::Set(args) => cmd_set(args),
        Command::Arrange { spec, safety } => cmd_arrange(&spec, safety),
        Command::Auto { safety } => cmd_auto(safety),
        Command::Profile { action } => cmd_profile(action),
        Command::Apply { safety } => cmd_apply_for_hardware(safety),
        Command::Persist => cmd_persist(),
        Command::Init { dry_run } => cmd_init(dry_run),
        Command::Daemon { web, no_web } => cmd_daemon(web, !no_web).await,
        Command::Web { web } => cmd_web(web).await,
    }
}

fn init_tracing(verbose: bool) {
    let default = if verbose {
        "hyprmc=debug"
    } else {
        "hyprmc=info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("HYPRMC_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

fn backend() -> Result<HyprSocket> {
    HyprSocket::connect().context("Hyprland ne semble pas accessible")
}

// ---------------------------------------------------------------- lecture ---

fn cmd_list(json: bool) -> Result<()> {
    let monitors = backend()?.monitors()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&monitors)?);
        return Ok(());
    }
    if monitors.is_empty() {
        println!("Aucun écran détecté.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "Écran",
            "État",
            "Mode",
            "Position",
            "Échelle",
            "Orientation",
            "Identifiant",
        ]);

    for m in &monitors {
        let state = if m.disabled {
            "désactivé".to_string()
        } else if let Some(target) = m.mirror_target(&monitors) {
            format!("miroir de {target}")
        } else if m.focused {
            "actif (focus)".to_string()
        } else {
            "actif".to_string()
        };
        let dash = |s: String| if m.disabled { "—".to_string() } else { s };
        table.add_row(vec![
            Cell::new(&m.name),
            Cell::new(state),
            Cell::new(dash(m.mode().to_string())),
            Cell::new(dash(format!("{}x{}", m.x, m.y))),
            Cell::new(format_scale(m.scale)),
            Cell::new(m.transform().to_string()),
            Cell::new(m.fingerprint()),
        ]);
    }
    println!("{table}");

    print_issues(&Layout::from_monitors(&monitors));
    Ok(())
}

fn print_issues(layout: &Layout) {
    for issue in layout.validate() {
        eprintln!("{} : {}", severity_label(issue.severity), issue.message);
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "erreur",
        Severity::Warning => "attention",
    }
}

fn cmd_modes(output: &str) -> Result<()> {
    let monitors = backend()?.monitors()?;
    let m = find(&monitors, output)?;
    if m.available_modes.is_empty() {
        println!("Aucun mode rapporté pour « {output} ».");
        return Ok(());
    }
    let current = m.mode();
    for mode in m.parsed_modes() {
        let is_current = mode.width == current.width
            && mode.height == current.height
            && (mode.refresh - current.refresh).abs() < 0.5;
        println!("{mode}{}", if is_current { " (actuel)" } else { "" });
    }
    Ok(())
}

fn find<'a>(monitors: &'a [Monitor], name: &str) -> Result<&'a Monitor> {
    monitors.iter().find(|m| m.name == name).ok_or_else(|| {
        anyhow!(
            "écran « {name} » introuvable (connus : {})",
            monitors
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

// ------------------------------------------------------------ modification ---

fn cmd_set(args: SetArgs) -> Result<()> {
    let hypr = backend()?;
    let monitors = hypr.monitors()?;
    let target = find(&monitors, &args.output)?;
    let preferred = target.preferred_mode();

    let mut layout = Layout::from_monitors(&monitors);
    {
        let o = layout
            .get_mut(&args.output)
            .expect("l'écran vient d'être vérifié");

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

        // La taille logique — donc la détection de chevauchement — dépend du
        // mode : il faut le résoudre avant de valider.
        if o.mode.is_none() {
            o.mode = preferred;
        }
    }

    apply_interactively(&hypr, &layout, args.safety)?;

    if let Some(name) = &args.save {
        save_profile(name, false, &layout, &monitors)?;
        println!("Profil « {name} » enregistré.");
    }
    Ok(())
}

fn cmd_arrange(spec: &[String], safety: SafetyArgs) -> Result<()> {
    if !spec.len().is_multiple_of(3) {
        bail!(
            "arrange attend des triplets « ÉCRAN RELATION RÉFÉRENCE » ({} argument(s) reçu(s))",
            spec.len()
        );
    }
    let hypr = backend()?;
    let mut layout = Layout::from_monitors(&hypr.monitors()?);

    for triple in spec.chunks(3) {
        let relation: Relation = triple[1].parse()?;
        layout.place(&triple[0], relation, &triple[2])?;
    }
    layout.normalize();

    apply_interactively(&hypr, &layout, safety)
}

fn cmd_auto(safety: SafetyArgs) -> Result<()> {
    let hypr = backend()?;
    let mut layout = Layout::from_monitors(&hypr.monitors()?);
    layout.auto_arrange();
    apply_interactively(&hypr, &layout, safety)
}

/// Applique un agencement puis, dans un terminal, propose de revenir en
/// arrière — c'est ce qui évite de rester bloqué devant un écran noir.
fn apply_interactively(hypr: &HyprSocket, layout: &Layout, safety: SafetyArgs) -> Result<()> {
    let previous = apply::snapshot(hypr)?;
    let report = apply::apply(hypr, layout, safety.force)?;
    print_report(&report);

    if report.rolled_back {
        bail!("configuration non appliquée : l'état précédent a été restauré");
    }
    if safety.no_confirm {
        return Ok(());
    }

    let timeout = Duration::from_secs(Config::load()?.settings.confirm_timeout_secs);
    if !apply::confirm_or_revert(hypr, &previous, timeout)? {
        println!("Retour à la configuration précédente.");
    }
    Ok(())
}

fn print_report(report: &ApplyReport) {
    for issue in report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
    {
        eprintln!("attention : {}", issue.message);
    }
    for drift in &report.drifts {
        eprintln!("{} : {}", severity_label(drift.severity), drift.message);
    }
    if report.succeeded() {
        for spec in &report.specs {
            println!("monitor = {spec}");
        }
    }
}

// ----------------------------------------------------------------- profils ---

fn cmd_profile(action: ProfileAction) -> Result<()> {
    match action {
        ProfileAction::List => cmd_profile_list(),

        ProfileAction::Show { name } => {
            let cfg = Config::load()?;
            let profile = cfg
                .profile(&name)
                .ok_or_else(|| anyhow!("profil « {name} » inconnu"))?;
            print!("{}", toml::to_string_pretty(profile)?);
            Ok(())
        }

        ProfileAction::Save { name, exact } => {
            let monitors = backend()?.monitors()?;
            save_profile(&name, exact, &Layout::from_monitors(&monitors), &monitors)?;
            println!(
                "Profil « {name} » enregistré dans {}.",
                config_path().display()
            );
            Ok(())
        }

        ProfileAction::Apply { name, safety } => {
            let hypr = backend()?;
            let monitors = hypr.monitors()?;
            let cfg = Config::load()?;
            let profile = cfg
                .profile(&name)
                .ok_or_else(|| anyhow!("profil « {name} » inconnu"))?;
            let layout = profile.resolve(&monitors)?;
            apply_interactively(&hypr, &layout, safety)
        }

        ProfileAction::Delete { name } => {
            let mut cfg = Config::load()?;
            cfg.remove(&name)?;
            cfg.save()?;
            println!("Profil « {name} » supprimé.");
            Ok(())
        }

        ProfileAction::Rename { from, to } => {
            let mut cfg = Config::load()?;
            let renamed = Profile {
                name: to.clone(),
                ..cfg
                    .profile(&from)
                    .ok_or_else(|| anyhow!("profil « {from} » inconnu"))?
                    .clone()
            };
            cfg.remove(&from)?;
            cfg.upsert(renamed);
            cfg.save()?;
            println!("Profil « {from} » renommé en « {to} ».");
            Ok(())
        }
    }
}

fn cmd_profile_list() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.profiles.is_empty() {
        println!(
            "Aucun profil. Créez-en un avec « hyprmc profile save <nom> ».\nFichier : {}",
            config_path().display()
        );
        return Ok(());
    }

    // La liste doit rester consultable même sans Hyprland en fonctionnement.
    let monitors = backend().and_then(|b| b.monitors()).unwrap_or_default();
    let active = cfg.best_match(&monitors).map(|p| p.name.clone());

    let mut table = Table::new();
    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Profil", "Écrans", "Exact", "Correspond"]);
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
            Cell::new(if p.exact { "oui" } else { "non" }),
            Cell::new(if active.as_deref() == Some(p.name.as_str()) {
                "← actif"
            } else if p.matches(&monitors) {
                "oui"
            } else {
                "non"
            }),
        ]);
    }
    println!("{table}");
    Ok(())
}

/// Enregistre un agencement en désignant les écrans par leur empreinte, pour
/// que le profil résiste à un changement de connecteur.
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
    let hypr = backend()?;
    let monitors = hypr.monitors()?;
    let cfg = Config::load()?;

    let layout = match cfg.best_match(&monitors) {
        Some(profile) => {
            println!("Profil correspondant : « {} ».", profile.name);
            profile.resolve(&monitors)?
        }
        None => {
            println!("Aucun profil ne correspond : rangement automatique.");
            let mut layout = Layout::from_monitors(&monitors);
            layout.auto_arrange();
            layout
        }
    };
    apply_interactively(&hypr, &layout, safety)
}

// ------------------------------------------------------------- persistance ---

fn cmd_persist() -> Result<()> {
    let monitors = backend()?.monitors()?;
    let cfg = Config::load()?;
    emit::persist(
        &Layout::from_monitors(&monitors),
        &cfg.settings.monitors_conf,
    )?;
    println!(
        "Agencement écrit dans {}.",
        cfg.settings.monitors_conf.display()
    );

    if !sources_monitors_conf(&hyprland_conf()) {
        println!(
            "Attention : {} ne source pas encore ce fichier. Lancez « hyprmc init ».",
            hyprland_conf().display()
        );
    }
    Ok(())
}

fn sources_monitors_conf(hypr_conf: &std::path::Path) -> bool {
    std::fs::read_to_string(hypr_conf).is_ok_and(|c| {
        c.lines().any(|l| {
            let l = l.trim();
            !l.starts_with('#') && l.starts_with("source") && l.contains("monitors.conf")
        })
    })
}

fn cmd_init(dry_run: bool) -> Result<()> {
    let cfg = Config::load()?;
    let hypr_conf = hyprland_conf();
    let report = emit::run_init(&hypr_conf, &cfg.settings.monitors_conf, dry_run)?;

    if report.already_wired {
        println!(
            "{} source déjà {}. Rien à faire.",
            hypr_conf.display(),
            cfg.settings.monitors_conf.display()
        );
        return Ok(());
    }

    if dry_run {
        println!("--- {} (simulation) ---", hypr_conf.display());
        print!("{}", report.new_conf);
        return Ok(());
    }

    if let Some(backup) = &report.backup {
        println!("Sauvegarde : {}", backup.display());
    }
    println!(
        "Ajout de « source = {} ».",
        emit::tildify(&cfg.settings.monitors_conf)
    );
    if !report.adopted.is_empty() {
        println!(
            "{} directive(s) monitor reprise(s) et commentée(s) dans hyprland.conf.",
            report.adopted.len()
        );
    }

    // L'agencement courant est écrit tout de suite : au prochain rechargement,
    // l'affichage doit rester exactement tel qu'il est.
    let monitors = backend()?.monitors()?;
    emit::persist(
        &Layout::from_monitors(&monitors),
        &cfg.settings.monitors_conf,
    )?;
    println!(
        "Agencement courant écrit dans {}.",
        cfg.settings.monitors_conf.display()
    );
    println!("Rechargez avec « hyprctl reload ».");
    Ok(())
}

// ------------------------------------------------------------------ démon ---

fn web_addr(cfg: &Config, args: &WebArgs) -> Result<SocketAddr> {
    let ip: IpAddr = args
        .bind
        .as_deref()
        .unwrap_or(&cfg.settings.bind)
        .parse()
        .context("adresse d'écoute invalide")?;
    Ok(SocketAddr::new(
        ip,
        args.port.unwrap_or(cfg.settings.web_port),
    ))
}

async fn cmd_daemon(args: WebArgs, with_web: bool) -> Result<()> {
    let (state, hypr) = daemon::bootstrap()?;
    let addr = web_addr(&*state.config.read().await, &args)?;

    if with_web {
        let web_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(err) = hyprmc::web::serve(web_state, addr).await {
                tracing::error!("interface web arrêtée : {err:#}");
            }
        });
    }

    tracing::info!("surveillance du branchement à chaud active");
    daemon::run(state, &hypr).await
}

async fn cmd_web(args: WebArgs) -> Result<()> {
    let (state, _) = daemon::bootstrap()?;
    let addr = web_addr(&*state.config.read().await, &args)?;
    hyprmc::web::serve(state, addr).await
}
