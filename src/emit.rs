//! Persistance : génération de `monitors.conf` et branchement de ce fichier
//! dans la configuration Hyprland de l'utilisateur.
//!
//! Principe directeur : `hyprmc` est seul maître de `monitors.conf` et ne
//! touche à `hyprland.conf` qu'une seule fois, pour y ajouter un `source`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::config::home;
use crate::layout::Layout;

const HEADER: &str = "\
# Généré par hyprmc — NE PAS ÉDITER À LA MAIN.
# Ce fichier est réécrit à chaque `hyprmc persist` ou application de profil.
# Pour modifier l'agencement : `hyprmc set`, `hyprmc arrange`, ou l'interface web.
";

/// Écrit un fichier de façon atomique : fichier temporaire voisin puis
/// `rename`, pour qu'aucun lecteur ne voie jamais un contenu partiel — Hyprland
/// pourrait recharger la configuration au milieu de l'écriture.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("création de {} impossible", dir.display()))?;

    let tmp = dir.join(format!(
        ".{}.hyprmc.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("out"),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    {
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("création de {} impossible", tmp.display()))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("écriture dans {} impossible", tmp.display()))?;
        file.sync_all().ok();
    }

    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("remplacement de {} impossible", path.display())
    })
}

/// Rend le contenu de `monitors.conf` pour un agencement.
pub fn render(layout: &Layout) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for spec in layout.to_specs() {
        out.push_str("monitor = ");
        out.push_str(&spec);
        out.push('\n');
    }
    out
}

/// Écrit l'agencement dans `monitors.conf`.
pub fn persist(layout: &Layout, path: &Path) -> Result<()> {
    write_atomic(path, &render(layout))
}

/// Remplace le préfixe du répertoire personnel par `~`, comme on l'écrirait à
/// la main dans `hyprland.conf`.
pub fn tildify(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Ce que `hyprmc init` a fait (ou ferait, en simulation).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InitReport {
    /// `hyprland.conf` sourçait déjà le fichier généré.
    pub already_wired: bool,
    /// Copie de sauvegarde créée.
    pub backup: Option<PathBuf>,
    /// Directives `monitor =` reprises depuis `hyprland.conf` puis commentées.
    pub adopted: Vec<String>,
    /// Nouveau contenu de `hyprland.conf`.
    pub new_conf: String,
}

/// Calcule la transformation à appliquer à `hyprland.conf`.
///
/// Séparé de l'écriture pour pouvoir présenter le résultat à l'utilisateur
/// avant de toucher à son fichier.
pub fn plan_init(conf: &str, monitors_conf: &Path) -> InitReport {
    let source_line = format!("source = {}", tildify(monitors_conf));
    let target = monitors_conf
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("monitors.conf");

    let already = conf.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.starts_with("source") && l.contains(target)
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
    let mut last_source = None;

    for line in conf.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && is_directive(trimmed, "monitor") {
            if let Some(spec) = trimmed.split_once('=').map(|(_, v)| v.trim()) {
                adopted.push(spec.to_string());
            }
            lines.push(format!("# repris par hyprmc -> {target}\n#{line}"));
            continue;
        }
        if !trimmed.starts_with('#') && is_directive(trimmed, "source") {
            last_source = Some(lines.len());
        }
        lines.push(line.to_string());
    }

    // Le `source` est inséré juste après les autres pour rester lisible ; à
    // défaut, en tête de fichier.
    let insert_at = last_source.map_or(0, |i| i + 1);
    lines.insert(insert_at, source_line);

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

/// `mot = valeur` ou `mot=valeur`, en ignorant les espaces.
fn is_directive(line: &str, keyword: &str) -> bool {
    line.strip_prefix(keyword)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Branche `monitors.conf` dans `hyprland.conf`, avec sauvegarde préalable.
pub fn run_init(hyprland_conf: &Path, monitors_conf: &Path, dry_run: bool) -> Result<InitReport> {
    let conf = std::fs::read_to_string(hyprland_conf)
        .with_context(|| format!("lecture de {} impossible", hyprland_conf.display()))?;
    let mut report = plan_init(&conf, monitors_conf);

    if report.already_wired || dry_run {
        return Ok(report);
    }

    let backup = hyprland_conf.with_extension("conf.hyprmc.bak");
    std::fs::copy(hyprland_conf, &backup)
        .with_context(|| format!("sauvegarde vers {} impossible", backup.display()))?;
    report.backup = Some(backup);

    write_atomic(hyprland_conf, &report.new_conf)?;
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

    #[test]
    fn render_emits_one_directive_per_output() {
        let mut b = out("DP-1", 1920, 0);
        b.transform = Transform::new(Rotation::R90, false);
        let layout = Layout::new(vec![out("eDP-1", 0, 0), b]);
        let text = render(&layout);
        assert!(text.starts_with("# Généré par hyprmc"));
        assert!(text.contains("monitor = eDP-1,1920x1080@60.00,0x0,1\n"));
        assert!(text.contains("monitor = DP-1,1920x1080@60.00,1920x0,1,transform,1\n"));
        assert_eq!(text.lines().filter(|l| l.starts_with("monitor")).count(), 2);
    }

    #[test]
    fn render_emits_disable_for_switched_off_screens() {
        let mut a = out("eDP-1", 0, 0);
        a.enabled = false;
        let text = render(&Layout::new(vec![a, out("DP-1", 0, 0)]));
        assert!(text.contains("monitor = eDP-1,disable\n"));
    }

    const SAMPLE: &str = "\
# ma config
source = ~/.config/hypr/startup.conf
source = ~/.config/hypr/env.conf

monitor = eDP-1,1920x1080@60,0x0,1

general {
    gaps_in = 5
}
";

    #[test]
    fn init_inserts_source_after_existing_sources() {
        let report = plan_init(SAMPLE, Path::new("/home/u/.config/hypr/monitors.conf"));
        let lines: Vec<&str> = report.new_conf.lines().collect();
        let src = lines
            .iter()
            .position(|l| l.contains("monitors.conf"))
            .unwrap();
        let env = lines.iter().position(|l| l.contains("env.conf")).unwrap();
        assert_eq!(src, env + 1);
    }

    #[test]
    fn init_adopts_and_comments_existing_monitor_lines() {
        let report = plan_init(SAMPLE, Path::new("/tmp/monitors.conf"));
        assert_eq!(report.adopted, vec!["eDP-1,1920x1080@60,0x0,1"]);
        assert!(
            report
                .new_conf
                .contains("#monitor = eDP-1,1920x1080@60,0x0,1")
        );
        // Plus aucune directive monitor active.
        assert!(
            !report
                .new_conf
                .lines()
                .any(|l| is_directive(l.trim(), "monitor"))
        );
    }

    #[test]
    fn init_is_idempotent() {
        let first = plan_init(SAMPLE, Path::new("/tmp/monitors.conf"));
        let second = plan_init(&first.new_conf, Path::new("/tmp/monitors.conf"));
        assert!(second.already_wired);
        assert_eq!(second.new_conf, first.new_conf);
    }

    #[test]
    fn init_ignores_commented_out_directives() {
        let conf = "# monitor = eDP-1,preferred,auto,1\n# source = ~/.config/hypr/monitors.conf\n";
        let report = plan_init(conf, Path::new("/home/u/.config/hypr/monitors.conf"));
        assert!(!report.already_wired);
        assert!(report.adopted.is_empty());
        assert!(
            report
                .new_conf
                .lines()
                .any(|l| is_directive(l.trim(), "source"))
        );
    }

    #[test]
    fn init_handles_a_config_without_any_source() {
        let report = plan_init(
            "monitor = eDP-1,preferred,auto,1\n",
            Path::new("/tmp/m.conf"),
        );
        assert!(report.new_conf.starts_with("source = /tmp/m.conf"));
        assert_eq!(report.adopted.len(), 1);
    }

    #[test]
    fn directive_detection_tolerates_spacing() {
        assert!(is_directive("monitor=eDP-1", "monitor"));
        assert!(is_directive("monitor   =  eDP-1", "monitor"));
        assert!(!is_directive("monitorv2 = x", "monitor"));
        assert!(!is_directive("monitor", "monitor"));
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("hyprmc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("monitors.conf");
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
