//! Description d'un agencement d'écrans, sa validation et son arrangement
//! automatique.
//!
//! Tout ce module est purement calculatoire : aucune I/O, aucune dépendance à
//! Hyprland. C'est ici que vivent les règles qui évitent d'envoyer au
//! compositeur une configuration qui laisserait l'utilisateur devant un écran
//! noir.

use std::collections::HashMap;
use std::fmt;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::monitor::{Mode, Monitor, Transform};

/// Hyprland raisonne en pas de 1/120 pour les échelles fractionnaires.
const SCALE_STEP: f64 = 1.0 / 120.0;
/// Tolérance d'arrondi pour juger qu'une taille logique est entière.
const EPSILON: f64 = 1e-3;

/// Configuration désirée pour un écran.
///
/// C'est le type pivot de l'application : la CLI, l'UI web, les profils et
/// l'état live se traduisent tous en `OutputState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    /// Nom du connecteur (`eDP-1`, `DP-3`…).
    pub name: String,
    pub enabled: bool,
    /// `None` = laisser Hyprland choisir le mode préféré.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_of: Option<String>,
    #[serde(default)]
    pub vrr: bool,
}

impl OutputState {
    /// Configuration désirée reflétant l'état actuel d'un écran.
    ///
    /// `all` sert à résoudre la cible de duplication, que Hyprland désigne par
    /// son identifiant numérique.
    pub fn from_monitor(m: &Monitor, all: &[Monitor]) -> Self {
        Self {
            name: m.name.clone(),
            enabled: !m.disabled,
            mode: if m.width > 0 && m.height > 0 {
                Some(m.mode())
            } else {
                None
            },
            x: m.x,
            y: m.y,
            scale: if m.scale > 0.0 { m.scale } else { 1.0 },
            transform: m.transform(),
            mirror_of: m.mirror_target(all),
            vrr: m.vrr,
        }
    }

    /// Taille occupée dans l'espace de travail, en pixels logiques.
    ///
    /// La rotation échange les axes ; l'échelle divise. Retourne `None` tant
    /// que le mode n'a pas été résolu.
    pub fn logical_size(&self) -> Option<(f64, f64)> {
        let m = self.mode?;
        let (w, h) = if self.transform.swaps_axes() {
            (m.height, m.width)
        } else {
            (m.width, m.height)
        };
        Some((f64::from(w) / self.scale, f64::from(h) / self.scale))
    }

    /// Taille logique arrondie, telle que Hyprland la réservera.
    pub fn logical_size_rounded(&self) -> (i32, i32) {
        match self.logical_size() {
            Some((w, h)) => (w.round() as i32, h.round() as i32),
            None => (0, 0),
        }
    }

    /// Rectangle occupé : `(x1, y1, x2, y2)`, bord droit/bas exclus.
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        let (w, h) = self.logical_size_rounded();
        (self.x, self.y, self.x + w, self.y + h)
    }

    /// Occupe-t-il de la place dans l'espace de travail ?
    ///
    /// Un écran en miroir se superpose volontairement à sa cible : il est exclu
    /// de la détection de chevauchement.
    pub fn occupies_space(&self) -> bool {
        self.enabled && self.mirror_of.is_none()
    }

    /// Rend la directive Hyprland correspondante (partie après `monitor = `).
    pub fn to_spec(&self) -> String {
        if !self.enabled {
            return format!("{},disable", self.name);
        }
        let mode = match self.mode {
            Some(m) => m.to_string(),
            None => "preferred".to_string(),
        };
        let mut spec = format!(
            "{},{},{}x{},{}",
            self.name,
            mode,
            self.x,
            self.y,
            format_scale(self.scale)
        );
        if self.transform.to_u8() != 0 {
            spec.push_str(&format!(",transform,{}", self.transform.to_u8()));
        }
        if let Some(target) = &self.mirror_of {
            spec.push_str(&format!(",mirror,{target}"));
        }
        if self.vrr {
            spec.push_str(",vrr,1");
        }
        spec
    }
}

/// Formate une échelle sans zéros parasites : `1`, `1.5`, `1.333333`.
pub fn format_scale(scale: f64) -> String {
    let s = format!("{scale:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "1".to_string()
    } else {
        s.to_string()
    }
}

/// Gravité d'un problème détecté dans un agencement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Bloquant : Hyprland refuserait la configuration, ou elle rendrait un
    /// écran inutilisable.
    Error,
    /// Suspect mais applicable.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    /// Écrans concernés, pour que l'UI puisse les surligner.
    pub outputs: Vec<String>,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.severity {
            Severity::Error => "erreur",
            Severity::Warning => "avertissement",
        };
        write!(f, "[{tag}] {}", self.message)
    }
}

/// Un agencement complet, prêt à être validé puis appliqué.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub outputs: Vec<OutputState>,
}

impl Layout {
    pub fn new(outputs: Vec<OutputState>) -> Self {
        Self { outputs }
    }

    /// Agencement reflétant l'état live.
    pub fn from_monitors(monitors: &[Monitor]) -> Self {
        Self::new(
            monitors
                .iter()
                .map(|m| OutputState::from_monitor(m, monitors))
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&OutputState> {
        self.outputs.iter().find(|o| o.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut OutputState> {
        self.outputs.iter_mut().find(|o| o.name == name)
    }

    pub fn active(&self) -> impl Iterator<Item = &OutputState> {
        self.outputs.iter().filter(|o| o.occupies_space())
    }

    /// Directives Hyprland pour l'agencement entier.
    pub fn to_specs(&self) -> Vec<String> {
        self.outputs.iter().map(OutputState::to_spec).collect()
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|i| i.severity == Severity::Error)
    }

    /// Passe en revue tous les pièges connus.
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();

        // Doublons de connecteur : la dernière directive écraserait la première.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for o in &self.outputs {
            *seen.entry(o.name.as_str()).or_insert(0) += 1;
        }
        for (name, count) in seen.iter().filter(|(_, c)| **c > 1) {
            issues.push(Issue {
                severity: Severity::Error,
                outputs: vec![(*name).to_string()],
                message: format!("l'écran « {name} » est défini {count} fois"),
            });
        }

        for o in &self.outputs {
            if !o.enabled {
                continue;
            }

            if o.scale <= 0.0 {
                issues.push(Issue {
                    severity: Severity::Error,
                    outputs: vec![o.name.clone()],
                    message: format!("échelle invalide pour « {} » : {}", o.name, o.scale),
                });
                continue;
            }

            // Une échelle qui ne donne pas une taille logique entière n'est pas
            // rejetée par Hyprland : il l'arrondit en silence. Autant prévenir
            // de la valeur qui sera réellement utilisée.
            if let Some((w, h)) = o.logical_size()
                && (!is_integral(w) || !is_integral(h))
            {
                let suggestion = nearest_valid_scale(o.mode.unwrap(), o.transform, o.scale);
                issues.push(Issue {
                    severity: Severity::Warning,
                    outputs: vec![o.name.clone()],
                    message: format!(
                        "l'échelle {} donne une taille logique non entière ({w:.3}x{h:.3}) pour « {} » : \
                         Hyprland l'ajustera (autour de {})",
                        format_scale(o.scale),
                        o.name,
                        format_scale(suggestion)
                    ),
                });
            }

            // Une cible de miroir absente fait échouer la directive.
            if let Some(target) = &o.mirror_of {
                match self.get(target) {
                    None => issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![o.name.clone()],
                        message: format!("« {} » duplique « {target} », qui n'existe pas", o.name),
                    }),
                    Some(t) if !t.enabled => issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![o.name.clone(), target.clone()],
                        message: format!("« {} » duplique « {target} », qui est désactivé", o.name),
                    }),
                    Some(_) => {}
                }
            }
        }

        // Deux écrans qui se chevauchent : la zone commune devient inatteignable
        // à la souris sur l'un des deux.
        let active: Vec<&OutputState> = self.active().collect();
        for (i, a) in active.iter().enumerate() {
            for b in &active[i + 1..] {
                if rects_overlap(a.rect(), b.rect()) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        outputs: vec![a.name.clone(), b.name.clone()],
                        message: format!("« {} » et « {} » se chevauchent", a.name, b.name),
                    });
                }
            }
        }

        // Un écran isolé reste accessible au clavier mais la souris ne peut pas
        // l'atteindre : c'est un avertissement, pas une erreur.
        if active.len() > 1 {
            for a in &active {
                let touches = active
                    .iter()
                    .any(|b| b.name != a.name && rects_touch(a.rect(), b.rect()));
                if !touches {
                    issues.push(Issue {
                        severity: Severity::Warning,
                        outputs: vec![a.name.clone()],
                        message: format!(
                            "« {} » ne touche aucun autre écran : le curseur ne pourra pas y accéder",
                            a.name
                        ),
                    });
                }
            }
        }

        if !self.outputs.is_empty() && active.is_empty() {
            issues.push(Issue {
                severity: Severity::Error,
                outputs: Vec::new(),
                message: "tous les écrans seraient désactivés".to_string(),
            });
        }

        issues
    }

    /// Ramène le coin supérieur gauche de l'ensemble à (0, 0).
    pub fn normalize(&mut self) {
        let min_x = self.active().map(|o| o.x).min().unwrap_or(0);
        let min_y = self.active().map(|o| o.y).min().unwrap_or(0);
        if min_x == 0 && min_y == 0 {
            return;
        }
        for o in self.outputs.iter_mut().filter(|o| o.occupies_space()) {
            o.x -= min_x;
            o.y -= min_y;
        }
    }

    /// Range les écrans actifs côte à côte, de gauche à droite, alignés en haut.
    ///
    /// C'est le repli quand aucun profil ne correspond au matériel branché.
    pub fn auto_arrange(&mut self) {
        let mut cursor = 0;
        for o in self.outputs.iter_mut().filter(|o| o.occupies_space()) {
            o.x = cursor;
            o.y = 0;
            cursor += o.logical_size_rounded().0;
        }
    }

    /// Applique une relation de placement : `subject` est posé contre `anchor`.
    pub fn place(&mut self, subject: &str, relation: Relation, anchor: &str) -> Result<()> {
        let (ax, ay, aw, ah) = match self.get(anchor) {
            Some(a) => {
                let (w, h) = a.logical_size_rounded();
                (a.x, a.y, w, h)
            }
            None => bail!("écran de référence « {anchor} » inconnu"),
        };
        let (sw, sh) = match self.get(subject) {
            Some(s) => s.logical_size_rounded(),
            None => bail!("écran « {subject} » inconnu"),
        };
        if subject == anchor {
            bail!("« {subject} » ne peut pas être positionné par rapport à lui-même");
        }

        let (x, y) = match relation {
            Relation::LeftOf => (ax - sw, ay),
            Relation::RightOf => (ax + aw, ay),
            Relation::Above => (ax, ay - sh),
            Relation::Below => (ax, ay + ah),
            Relation::SameAs => (ax, ay),
        };

        let s = self.get_mut(subject).expect("vérifié plus haut");
        s.x = x;
        s.y = y;
        Ok(())
    }
}

/// Relations de placement acceptées par `hyprmc arrange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    LeftOf,
    RightOf,
    Above,
    Below,
    SameAs,
}

impl std::str::FromStr for Relation {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "left-of" | "gauche-de" | "left" => Relation::LeftOf,
            "right-of" | "droite-de" | "right" => Relation::RightOf,
            "above" | "au-dessus-de" | "up" => Relation::Above,
            "below" | "en-dessous-de" | "down" => Relation::Below,
            "same-as" | "mirror-position" => Relation::SameAs,
            other => bail!(
                "relation inconnue « {other} » (attendu left-of, right-of, above, below, same-as)"
            ),
        })
    }
}

fn is_integral(v: f64) -> bool {
    (v - v.round()).abs() < EPSILON
}

fn rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// Les rectangles partagent-ils un bord sur une longueur non nulle ?
fn rects_touch(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    let x_overlap = a.0 < b.2 && b.0 < a.2;
    let y_overlap = a.1 < b.3 && b.1 < a.3;
    let x_adjacent = a.2 == b.0 || b.2 == a.0;
    let y_adjacent = a.3 == b.1 || b.3 == a.1;
    (x_adjacent && y_overlap) || (y_adjacent && x_overlap)
}

/// Cherche l'échelle valide la plus proche de celle demandée.
///
/// Hyprland exige que `taille / échelle` tombe sur un entier ; il travaille par
/// pas de 1/120. On balaie ces pas autour de la valeur souhaitée et on retient
/// le premier candidat acceptable.
pub fn nearest_valid_scale(mode: Mode, transform: Transform, wanted: f64) -> f64 {
    let (w, h) = if transform.swaps_axes() {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    };
    let valid = |s: f64| s > 0.0 && is_integral(f64::from(w) / s) && is_integral(f64::from(h) / s);
    if valid(wanted) {
        return wanted;
    }

    let base = (wanted / SCALE_STEP).round();
    // ±0.5 autour de la valeur demandée, soit 60 pas de chaque côté.
    for step in 1..=60 {
        for candidate in [(base + f64::from(step)), (base - f64::from(step))] {
            let s = candidate * SCALE_STEP;
            if valid(s) {
                return (s * 120.0).round() / 120.0;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Rotation;

    fn out(name: &str, w: i32, h: i32, x: i32, y: i32) -> OutputState {
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
    fn logical_size_swaps_axes_when_rotated() {
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
        o.transform = Transform::new(Rotation::R90, false);
        assert_eq!(o.logical_size_rounded(), (1080, 1920));
        o.transform = Transform::new(Rotation::R180, false);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
    }

    #[test]
    fn logical_size_divides_by_scale() {
        let mut o = out("DP-1", 3840, 2160, 0, 0);
        o.scale = 1.5;
        assert_eq!(o.logical_size_rounded(), (2560, 1440));
    }

    #[test]
    fn flipping_does_not_change_logical_size() {
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        o.transform = Transform::new(Rotation::R0, true);
        assert_eq!(o.logical_size_rounded(), (1920, 1080));
    }

    #[test]
    fn spec_omits_default_extras() {
        let o = out("eDP-1", 1920, 1080, 0, 0);
        assert_eq!(o.to_spec(), "eDP-1,1920x1080@60.00,0x0,1");
    }

    #[test]
    fn spec_includes_transform_mirror_and_vrr() {
        let mut o = out("DP-1", 1920, 1080, 1920, 0);
        o.transform = Transform::new(Rotation::R90, true);
        o.mirror_of = Some("eDP-1".into());
        o.vrr = true;
        o.scale = 1.25;
        assert_eq!(
            o.to_spec(),
            "DP-1,1920x1080@60.00,1920x0,1.25,transform,5,mirror,eDP-1,vrr,1"
        );
    }

    #[test]
    fn disabled_output_spec_is_just_disable() {
        let mut o = out("eDP-1", 1920, 1080, 0, 0);
        o.enabled = false;
        assert_eq!(o.to_spec(), "eDP-1,disable");
    }

    #[test]
    fn scale_formatting_drops_noise() {
        assert_eq!(format_scale(1.0), "1");
        assert_eq!(format_scale(1.5), "1.5");
        assert_eq!(format_scale(1.6666666666), "1.666667");
    }

    #[test]
    fn overlap_is_an_error() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1000, 0),
        ]);
        let issues = layout.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("chevauchent"))
        );
    }

    #[test]
    fn adjacent_screens_are_clean() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1920, 0),
        ]);
        assert_eq!(layout.validate(), Vec::new());
    }

    #[test]
    fn isolated_screen_warns_but_does_not_block() {
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 5000, 0),
        ]);
        let issues = layout.validate();
        assert!(!layout.has_errors());
        assert!(issues.iter().any(|i| i.severity == Severity::Warning));
    }

    #[test]
    fn corner_contact_does_not_count_as_touching() {
        // Coin contre coin : aucun bord partagé, le curseur ne passe pas.
        let layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1920, 1080, 1920, 1080),
        ]);
        assert!(
            layout
                .validate()
                .iter()
                .any(|i| i.severity == Severity::Warning)
        );
    }

    #[test]
    fn mirrored_output_may_overlap_its_target() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("A".into());
        let layout = Layout::new(vec![out("A", 1920, 1080, 0, 0), b]);
        assert!(!layout.has_errors());
    }

    #[test]
    fn mirroring_a_missing_or_disabled_output_is_an_error() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("ABSENT".into());
        assert!(Layout::new(vec![out("A", 1920, 1080, 0, 0), b]).has_errors());

        let mut a = out("A", 1920, 1080, 0, 0);
        a.enabled = false;
        let mut b = out("B", 1920, 1080, 0, 0);
        b.mirror_of = Some("A".into());
        assert!(Layout::new(vec![a, b]).has_errors());
    }

    #[test]
    fn duplicate_connector_is_an_error() {
        let layout = Layout::new(vec![out("A", 1920, 1080, 0, 0), out("A", 1280, 720, 0, 0)]);
        assert!(
            layout
                .validate()
                .iter()
                .any(|i| i.message.contains("défini 2 fois"))
        );
    }

    #[test]
    fn disabling_everything_is_an_error() {
        let mut a = out("A", 1920, 1080, 0, 0);
        a.enabled = false;
        assert!(Layout::new(vec![a]).has_errors());
    }

    #[test]
    fn non_integral_scale_warns_without_blocking() {
        // Hyprland accepte 1.37 puis l'arrondit à 1.33 en silence : c'est un
        // avertissement, pas un refus.
        let mut o = out("DP-1", 1920, 1080, 0, 0);
        o.scale = 1.37;
        let layout = Layout::new(vec![o]);
        let issues = layout.validate();
        let issue = issues
            .iter()
            .find(|i| i.message.contains("non entière"))
            .expect("l'échelle bancale doit être signalée");
        assert_eq!(issue.severity, Severity::Warning);
        assert!(!layout.has_errors());
    }

    #[test]
    fn nearest_valid_scale_finds_a_usable_value() {
        let mode = Mode::new(1920, 1080, 60.0);
        let s = nearest_valid_scale(mode, Transform::default(), 1.37);
        assert!((1920.0 / s - (1920.0 / s).round()).abs() < 1e-3);
        assert!((1080.0 / s - (1080.0 / s).round()).abs() < 1e-3);
        assert!((s - 1.37).abs() < 0.5, "échelle trop éloignée : {s}");
    }

    #[test]
    fn nearest_valid_scale_keeps_already_valid_values() {
        let mode = Mode::new(3840, 2160, 60.0);
        assert_eq!(nearest_valid_scale(mode, Transform::default(), 1.5), 1.5);
    }

    #[test]
    fn auto_arrange_lines_screens_up() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 500, 500),
            out("B", 2560, 1440, 0, 0),
        ]);
        layout.auto_arrange();
        assert_eq!(
            (layout.get("A").unwrap().x, layout.get("A").unwrap().y),
            (0, 0)
        );
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (1920, 0)
        );
        assert!(!layout.has_errors());
    }

    #[test]
    fn auto_arrange_accounts_for_rotation() {
        let mut a = out("A", 1920, 1080, 0, 0);
        a.transform = Transform::new(Rotation::R90, false);
        let mut layout = Layout::new(vec![a, out("B", 1920, 1080, 0, 0)]);
        layout.auto_arrange();
        // A tourné occupe 1080 de large.
        assert_eq!(layout.get("B").unwrap().x, 1080);
        assert!(!layout.has_errors());
    }

    #[test]
    fn auto_arrange_skips_disabled_screens() {
        let mut b = out("B", 1920, 1080, 0, 0);
        b.enabled = false;
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            b,
            out("C", 1280, 720, 0, 0),
        ]);
        layout.auto_arrange();
        assert_eq!(layout.get("C").unwrap().x, 1920);
    }

    #[test]
    fn place_positions_relative_to_anchor() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, 0, 0),
            out("B", 1280, 720, 9999, 9999),
        ]);
        layout.place("B", Relation::LeftOf, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (-1280, 0)
        );

        layout.place("B", Relation::Below, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, 1080)
        );

        layout.place("B", Relation::Above, "A").unwrap();
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (0, -720)
        );
    }

    #[test]
    fn place_rejects_unknown_and_self_reference() {
        let mut layout = Layout::new(vec![out("A", 1920, 1080, 0, 0)]);
        assert!(layout.place("A", Relation::LeftOf, "ABSENT").is_err());
        assert!(layout.place("A", Relation::LeftOf, "A").is_err());
    }

    #[test]
    fn normalize_moves_layout_back_to_origin() {
        let mut layout = Layout::new(vec![
            out("A", 1920, 1080, -1920, -100),
            out("B", 1920, 1080, 0, -100),
        ]);
        layout.normalize();
        assert_eq!(
            (layout.get("A").unwrap().x, layout.get("A").unwrap().y),
            (0, 0)
        );
        assert_eq!(
            (layout.get("B").unwrap().x, layout.get("B").unwrap().y),
            (1920, 0)
        );
    }

    #[test]
    fn relation_parsing_is_forgiving() {
        assert_eq!("left-of".parse::<Relation>().unwrap(), Relation::LeftOf);
        assert_eq!("RIGHT_OF".parse::<Relation>().unwrap(), Relation::RightOf);
        assert_eq!("droite-de".parse::<Relation>().unwrap(), Relation::RightOf);
        assert!("diagonale".parse::<Relation>().is_err());
    }
}
