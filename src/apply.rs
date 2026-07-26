//! Application d'un agencement avec filet de sécurité.
//!
//! Hyprland répond `ok` même quand il n'a pas fait ce qu'on lui demandait : un
//! mode inexistant est accepté sans broncher, une échelle invalide est arrondie
//! en silence. La seule façon fiable de savoir ce qui s'est passé est de relire
//! l'état ensuite et de le comparer à ce qu'on voulait — c'est le rôle de
//! [`diff`].
//!
//! Mais le changement n'est pas instantané : une rotation met une cinquantaine
//! de millisecondes à se refléter dans `j/monitors`. Relire une seule fois,
//! juste après le `ok`, ferait conclure à tort à un échec — d'où la phase de
//! stabilisation de [`observe`].

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::ipc::HyprBackend;
use crate::layout::{Issue, Layout, Severity, format_scale};
use crate::monitor::Monitor;

/// Écart toléré sur l'échelle avant de le signaler.
const SCALE_TOLERANCE: f64 = 0.005;
/// Écart toléré sur le taux de rafraîchissement (Hyprland arrondit : 60 → 60.06).
const REFRESH_TOLERANCE: f64 = 1.5;
/// Intervalle entre deux relectures pendant la stabilisation.
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);
/// Au-delà, on considère que Hyprland ne fera plus rien.
const SETTLE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Propriété sur laquelle porte un écart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    Presence,
    Enabled,
    Mode,
    Refresh,
    Position,
    Transform,
    Scale,
    Mirror,
}

impl Field {
    /// Cet écart peut-il se résorber tout seul si l'on patiente ?
    ///
    /// Oui pour tout ce que Hyprland applique au commit suivant. Non pour
    /// l'échelle et le rafraîchissement : ce sont des corrections délibérées du
    /// compositeur, attendre ne changerait rien et ferait perdre 1,5 s à chaque
    /// application.
    fn converges(self) -> bool {
        !matches!(self, Field::Scale | Field::Refresh)
    }
}

/// Un écart entre ce qui a été demandé et ce que Hyprland a réellement fait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drift {
    pub output: String,
    pub field: Field,
    pub severity: Severity,
    pub message: String,
}

impl Drift {
    fn error(output: &str, field: Field, message: String) -> Self {
        Self {
            output: output.to_string(),
            field,
            severity: Severity::Error,
            message,
        }
    }

    fn warning(output: &str, field: Field, message: String) -> Self {
        Self {
            output: output.to_string(),
            field,
            severity: Severity::Warning,
            message,
        }
    }
}

/// Résultat d'une application.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Directives envoyées à Hyprland.
    pub specs: Vec<String>,
    /// Problèmes détectés avant envoi.
    pub issues: Vec<Issue>,
    /// Écarts constatés après envoi.
    pub drifts: Vec<Drift>,
    /// L'état précédent a été restauré.
    pub rolled_back: bool,
}

impl ApplyReport {
    pub fn succeeded(&self) -> bool {
        !self.rolled_back && !self.drifts.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Compare l'agencement demandé à l'état réellement obtenu.
pub fn diff(requested: &Layout, actual: &[Monitor]) -> Vec<Drift> {
    let mut drifts = Vec::new();

    for want in &requested.outputs {
        let Some(got) = actual.iter().find(|m| m.name == want.name) else {
            drifts.push(Drift::error(
                &want.name,
                Field::Presence,
                format!("« {} » a disparu de la liste des écrans", want.name),
            ));
            continue;
        };

        if want.enabled == got.disabled {
            drifts.push(Drift::error(
                &want.name,
                Field::Enabled,
                format!(
                    "« {} » devrait être {} mais est {}",
                    want.name,
                    if want.enabled {
                        "activé"
                    } else {
                        "désactivé"
                    },
                    if got.disabled {
                        "désactivé"
                    } else {
                        "activé"
                    }
                ),
            ));
            continue;
        }

        if !want.enabled {
            continue;
        }

        if let Some(mode) = want.mode {
            if mode.width != got.width || mode.height != got.height {
                drifts.push(Drift::error(
                    &want.name,
                    Field::Mode,
                    format!(
                        "mode refusé pour « {} » : {}x{} demandé, {}x{} obtenu",
                        want.name, mode.width, mode.height, got.width, got.height
                    ),
                ));
            } else if mode.refresh > 0.0
                && (mode.refresh - got.refresh_rate).abs() > REFRESH_TOLERANCE
            {
                drifts.push(Drift::warning(
                    &want.name,
                    Field::Refresh,
                    format!(
                        "taux de rafraîchissement ajusté sur « {} » : {:.2} Hz demandé, {:.2} Hz obtenu",
                        want.name, mode.refresh, got.refresh_rate
                    ),
                ));
            }
        }

        // Un écran dupliqué se cale sur la position de sa source : comparer sa
        // position à celle demandée n'aurait aucun sens.
        if want.mirror_of.is_none() && (want.x != got.x || want.y != got.y) {
            drifts.push(Drift::error(
                &want.name,
                Field::Position,
                format!(
                    "position refusée pour « {} » : {}x{} demandé, {}x{} obtenu",
                    want.name, want.x, want.y, got.x, got.y
                ),
            ));
        }

        if want.transform.to_u8() != got.transform {
            drifts.push(Drift::error(
                &want.name,
                Field::Transform,
                format!(
                    "orientation refusée pour « {} » : {} demandé, {} obtenu",
                    want.name,
                    want.transform,
                    got.transform()
                ),
            ));
        }

        // Hyprland arrondit l'échelle à une valeur qui donne une taille logique
        // entière : c'est une correction attendue, pas un échec.
        if (want.scale - got.scale).abs() > SCALE_TOLERANCE {
            drifts.push(Drift::warning(
                &want.name,
                Field::Scale,
                format!(
                    "échelle ajustée par Hyprland sur « {} » : {} demandé, {} appliqué",
                    want.name,
                    format_scale(want.scale),
                    format_scale(got.scale)
                ),
            ));
        }

        let got_mirror = got.mirror_target(actual);
        if want.mirror_of != got_mirror {
            drifts.push(Drift::warning(
                &want.name,
                Field::Mirror,
                format!(
                    "duplication non conforme sur « {} » : {} demandé, {} obtenu",
                    want.name,
                    want.mirror_of.as_deref().unwrap_or("aucune"),
                    got_mirror.as_deref().unwrap_or("aucune")
                ),
            ));
        }
    }

    drifts
}

/// Lit l'état courant sous forme d'agencement, pour pouvoir y revenir.
pub fn snapshot(backend: &dyn HyprBackend) -> Result<Layout> {
    Ok(Layout::from_monitors(&backend.monitors()?))
}

/// Relit l'état jusqu'à ce qu'il corresponde à la demande, ou jusqu'à
/// expiration.
///
/// Hyprland accuse réception immédiatement mais applique au commit suivant :
/// une rotation n'est visible dans `j/monitors` qu'une cinquantaine de
/// millisecondes plus tard. On sort dès que plus aucun écart bloquant ne
/// subsiste, donc sans attente inutile dans le cas courant.
pub fn observe(backend: &dyn HyprBackend, layout: &Layout) -> Result<Vec<Drift>> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let drifts = diff(layout, &backend.monitors()?);
        let settled = !drifts.iter().any(|d| d.field.converges());
        if settled || Instant::now() >= deadline {
            return Ok(drifts);
        }
        std::thread::sleep(SETTLE_INTERVAL);
    }
}

/// Envoie l'agencement à Hyprland, vérifie le résultat, et revient en arrière
/// si le résultat est inutilisable.
///
/// `force` passe outre les erreurs de validation *et* les écarts constatés :
/// c'est la sortie de secours quand l'utilisateur sait ce qu'il fait.
pub fn apply(backend: &dyn HyprBackend, layout: &Layout, force: bool) -> Result<ApplyReport> {
    let issues = layout.validate();
    let blocking: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    if !blocking.is_empty() && !force {
        bail!(
            "agencement refusé :\n{}\n(utilisez --force pour passer outre)",
            blocking
                .iter()
                .map(|i| format!("  • {}", i.message))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let previous = snapshot(backend)?;
    let specs = layout.to_specs();

    let mut report = ApplyReport {
        specs: specs.clone(),
        issues,
        ..Default::default()
    };

    if let Err(err) = backend.set_monitors(&specs) {
        // Un batch peut avoir été partiellement appliqué : on remet l'état connu.
        restore(backend, &previous).ok();
        return Err(err);
    }

    report.drifts = observe(backend, layout)?;

    let fatal = report.drifts.iter().any(|d| d.severity == Severity::Error);
    if fatal && !force {
        restore(backend, &previous)?;
        report.rolled_back = true;
    }

    Ok(report)
}

/// Réapplique un agencement connu, sans validation ni vérification : on revient
/// à un état qui fonctionnait, il ne faut surtout pas que ça échoue.
pub fn restore(backend: &dyn HyprBackend, layout: &Layout) -> Result<()> {
    backend.set_monitors(&layout.to_specs())
}

/// Demande confirmation à l'utilisateur et restaure l'état précédent en
/// l'absence de réponse.
///
/// C'est le garde-fou classique des réglages d'affichage : si la nouvelle
/// configuration rend l'écran illisible, ne rien faire suffit à revenir en
/// arrière.
pub fn confirm_or_revert(
    backend: &dyn HyprBackend,
    previous: &Layout,
    timeout: Duration,
) -> Result<bool> {
    use std::io::{BufRead, Write};
    use std::sync::mpsc;

    if timeout.is_zero() {
        return Ok(true);
    }
    if !stdin_is_tty() {
        // Sans terminal (script, hook), personne ne peut confirmer : on garde.
        return Ok(true);
    }

    print!(
        "Conserver cette configuration ? [o/N] (retour arrière automatique dans {} s) ",
        timeout.as_secs()
    );
    std::io::stdout().flush().ok();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });

    let keep = match rx.recv_timeout(timeout) {
        Ok(line) => {
            let a = line.trim().to_lowercase();
            a.starts_with('o') || a.starts_with('y')
        }
        Err(_) => {
            println!();
            false
        }
    };

    if !keep {
        restore(backend, previous)?;
    }
    Ok(keep)
}

fn stdin_is_tty() -> bool {
    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(0) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::fake::FakeBackend;
    use crate::layout::OutputState;
    use crate::monitor::{Mode, Rotation, Transform};

    /// `(nom, largeur, hauteur, x, y, échelle, transform, désactivé)`
    type Row<'a> = (&'a str, i32, i32, i32, i32, f64, u8, bool);

    fn monitors_json(entries: &[Row<'_>]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(name, w, h, x, y, scale, transform, disabled)| {
                format!(
                    r#"{{"id":0,"name":"{name}","description":"d","make":"m","model":"mo","serial":"s",
                    "width":{w},"height":{h},"refreshRate":60.0,"x":{x},"y":{y},"scale":{scale},
                    "transform":{transform},"focused":false,"disabled":{disabled},"mirrorOf":"none",
                    "vrr":false,"availableModes":["1920x1080@60.00Hz"]}}"#
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    fn want(name: &str, w: i32, h: i32, x: i32, y: i32) -> OutputState {
        OutputState {
            name: name.into(),
            enabled: true,
            mode: Some(Mode::new(w, h, 60.0)),
            x,
            y,
            scale: 1.0,
            transform: Transform::default(),
            mirror_of: None,
            vrr: false,
        }
    }

    #[test]
    fn diff_is_silent_when_hyprland_obeyed() {
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        assert_eq!(diff(&layout, &actual), Vec::new());
    }

    #[test]
    fn diff_catches_a_silently_refused_mode() {
        // Hyprland répond « ok » pour un mode inexistant : seul le relevé le révèle.
        let layout = Layout::new(vec![want("DP-1", 9999, 9999, 0, 0)]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("mode refusé"));
    }

    #[test]
    fn diff_catches_a_refused_position_and_orientation() {
        let mut w = want("DP-1", 1920, 1080, 3000, 0);
        w.transform = Transform::new(Rotation::R90, false);
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 2);
        assert!(drifts.iter().all(|d| d.severity == Severity::Error));
    }

    #[test]
    fn adjusted_scale_is_only_a_warning() {
        // Cas réel : 1.37 demandé, 1.33 appliqué.
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.scale = 1.37;
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> = serde_json::from_str(&monitors_json(&[(
            "DP-1", 1920, 1080, 0, 0, 1.33, 0, false,
        )]))
        .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].severity, Severity::Warning);
    }

    #[test]
    fn refresh_rounding_is_tolerated() {
        // 60 demandé, 60.056 appliqué : c'est le même mode.
        let layout = Layout::new(vec![want("eDP-1", 1920, 1080, 0, 0)]);
        let json = monitors_json(&[("eDP-1", 1920, 1080, 0, 0, 1.0, 0, false)])
            .replace("\"refreshRate\":60.0", "\"refreshRate\":60.056");
        let actual: Vec<Monitor> = serde_json::from_str(&json).unwrap();
        assert_eq!(diff(&layout, &actual), Vec::new());
    }

    #[test]
    fn diff_catches_a_vanished_output() {
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 0, 0)]);
        let drifts = diff(&layout, &[]);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("disparu"));
    }

    #[test]
    fn diff_catches_enable_disable_mismatch() {
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.enabled = false;
        let layout = Layout::new(vec![w]);
        let actual: Vec<Monitor> =
            serde_json::from_str(&monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]))
                .unwrap();
        let drifts = diff(&layout, &actual);
        assert_eq!(drifts[0].severity, Severity::Error);
        assert!(drifts[0].message.contains("désactivé"));
    }

    #[test]
    fn apply_sends_a_single_batch_and_reports_success() {
        let json = monitors_json(&[
            ("eDP-1", 1920, 1080, 0, 0, 1.0, 0, false),
            ("DP-1", 1920, 1080, 1920, 0, 1.0, 0, false),
        ]);
        let backend = FakeBackend::with_monitors(&json);
        let layout = Layout::new(vec![
            want("eDP-1", 1920, 1080, 0, 0),
            want("DP-1", 1920, 1080, 1920, 0),
        ]);

        let report = apply(&backend, &layout, false).unwrap();
        assert!(report.succeeded());
        assert!(!report.rolled_back);

        let batches: Vec<String> = backend
            .sent_commands()
            .into_iter()
            .filter(|c| c.starts_with("[[BATCH]]"))
            .collect();
        assert_eq!(batches.len(), 1, "un seul aller-retour attendu");
        assert!(batches[0].contains("keyword monitor eDP-1,1920x1080@60.00,0x0,1"));
        assert!(batches[0].contains("keyword monitor DP-1,1920x1080@60.00,1920x0,1"));
    }

    #[test]
    fn apply_refuses_an_invalid_layout_without_touching_hyprland() {
        let json = monitors_json(&[("A", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeBackend::with_monitors(&json);
        // Deux écrans superposés : erreur de validation.
        let layout = Layout::new(vec![
            want("A", 1920, 1080, 0, 0),
            want("B", 1920, 1080, 100, 0),
        ]);
        let err = apply(&backend, &layout, false).unwrap_err();
        assert!(err.to_string().contains("chevauchent"));
        assert!(
            !backend
                .sent_commands()
                .iter()
                .any(|c| c.contains("keyword monitor")),
            "aucune commande ne doit partir"
        );
    }

    #[test]
    fn apply_rolls_back_when_the_result_is_wrong() {
        // Le backend rapporte toujours 1920x1080 en 0x0 : la demande de 3000x0
        // ne sera pas honorée, donc retour arrière.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeBackend::with_monitors(&json);
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 3000, 0)]);

        let report = apply(&backend, &layout, false).unwrap();
        assert!(report.rolled_back);
        assert!(!report.succeeded());

        let batches: Vec<String> = backend
            .sent_commands()
            .into_iter()
            .filter(|c| c.starts_with("[[BATCH]]"))
            .collect();
        assert_eq!(batches.len(), 2, "application puis restauration");
        assert!(
            batches[1].contains("DP-1,1920x1080@60.00,0x0,1"),
            "la restauration doit réécrire l'état d'origine : {}",
            batches[1]
        );
    }

    #[test]
    fn apply_waits_for_hyprland_to_catch_up() {
        // Cas réel : une rotation met ~50 ms à se refléter dans j/monitors.
        // Relire une seule fois ferait conclure à tort à un échec.
        let rotated = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 1, false)]);
        let not_yet = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeBackend::settling_after(2, &not_yet, &rotated);

        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.transform = Transform::new(Rotation::R90, false);
        let report = apply(&backend, &Layout::new(vec![w]), false).unwrap();

        assert!(report.succeeded(), "drifts : {:?}", report.drifts);
        assert!(!report.rolled_back);
        assert!(
            backend.monitor_reads() >= 3,
            "l'état doit être relu jusqu'à convergence"
        );
    }

    #[test]
    fn settling_also_waits_for_the_mirror_to_take_effect() {
        // La duplication n'est qu'un avertissement, mais elle finit par
        // s'appliquer : il faut l'attendre comme le reste.
        let mirrored = r#"[
          {"id":0,"name":"eDP-1","width":1920,"height":1080,"refreshRate":60.0,"x":0,"y":0,
           "scale":1.0,"transform":0,"disabled":false,"mirrorOf":"none","availableModes":[]},
          {"id":1,"name":"DP-1","width":1920,"height":1080,"refreshRate":60.0,"x":0,"y":0,
           "scale":1.0,"transform":0,"disabled":false,"mirrorOf":"0","availableModes":[]}
        ]"#;
        let not_yet = mirrored.replace(r#""mirrorOf":"0""#, r#""mirrorOf":"none""#);
        let backend = FakeBackend::settling_after(2, &not_yet, mirrored);

        let mut b = want("DP-1", 1920, 1080, 0, 0);
        b.mirror_of = Some("eDP-1".into());
        let layout = Layout::new(vec![want("eDP-1", 1920, 1080, 0, 0), b]);

        let report = apply(&backend, &layout, false).unwrap();
        assert_eq!(
            report.drifts,
            Vec::new(),
            "la duplication doit être attendue"
        );
    }

    #[test]
    fn an_adjusted_scale_does_not_stall_the_settle_loop() {
        // Hyprland ne reviendra jamais sur son arrondi : inutile d'attendre.
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.scale = 1.37;
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.33, 0, false)]);
        let backend = FakeBackend::with_monitors(&json);

        let started = Instant::now();
        let report = apply(&backend, &Layout::new(vec![w]), false).unwrap();
        assert!(report.succeeded());
        assert_eq!(report.drifts.len(), 1);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "l'application ne doit pas attendre la fin du délai de stabilisation"
        );
    }

    #[test]
    fn settling_gives_up_and_reports_a_genuine_failure() {
        // Rien ne bouge : au bout du délai, l'écart est bien signalé.
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeBackend::with_monitors(&json);
        let mut w = want("DP-1", 1920, 1080, 0, 0);
        w.transform = Transform::new(Rotation::R90, false);

        let report = apply(&backend, &Layout::new(vec![w]), false).unwrap();
        assert!(report.rolled_back);
    }

    #[test]
    fn force_keeps_the_result_despite_drift() {
        let json = monitors_json(&[("DP-1", 1920, 1080, 0, 0, 1.0, 0, false)]);
        let backend = FakeBackend::with_monitors(&json);
        let layout = Layout::new(vec![want("DP-1", 1920, 1080, 3000, 0)]);

        let report = apply(&backend, &layout, true).unwrap();
        assert!(!report.rolled_back);
        assert!(!report.drifts.is_empty());
    }

    #[test]
    fn force_bypasses_validation_errors() {
        let json = monitors_json(&[
            ("A", 1920, 1080, 0, 0, 1.0, 0, false),
            ("B", 1920, 1080, 100, 0, 1.0, 0, false),
        ]);
        let backend = FakeBackend::with_monitors(&json);
        let layout = Layout::new(vec![
            want("A", 1920, 1080, 0, 0),
            want("B", 1920, 1080, 100, 0),
        ]);
        let report = apply(&backend, &layout, true).unwrap();
        assert!(!report.rolled_back);
    }
}
