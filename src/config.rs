//! Profils utilisateur : lecture/écriture de `~/.config/hyprmc/config.toml` et
//! sélection du profil correspondant au matériel branché.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::layout::{Layout, OutputState};
use crate::monitor::{Mode, Monitor, Rotation, Transform};

/// Réglages globaux du démon et du serveur web.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Port d'écoute de l'interface web.
    pub web_port: u16,
    /// Adresse d'écoute. Locale par défaut : l'API pilote l'affichage, elle n'a
    /// rien à faire sur le réseau sans décision explicite.
    pub bind: String,
    /// Appliquer automatiquement le profil correspondant au branchement.
    pub auto_apply: bool,
    /// Délai avant retour arrière automatique si l'utilisateur ne confirme pas.
    /// `0` désactive le filet de sécurité.
    pub confirm_timeout_secs: u64,
    /// Fichier généré, à sourcer depuis `hyprland.conf`.
    pub monitors_conf: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            web_port: 8787,
            bind: "127.0.0.1".to_string(),
            auto_apply: true,
            confirm_timeout_secs: 10,
            monitors_conf: default_monitors_conf(),
        }
    }
}

/// Règle décrivant comment configurer un écran donné.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputRule {
    /// Motif désignant l'écran : nom de connecteur (`DP-1`), empreinte
    /// (`Dell Inc. U2723QE ABC123`) ou motif avec `*`.
    #[serde(rename = "match")]
    pub pattern: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `None` = mode préféré de l'écran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `None` ou `"auto"` = position calculée automatiquement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub flipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_of: Option<String>,
    #[serde(default)]
    pub vrr: bool,
}

fn yes() -> bool {
    true
}

fn one() -> f64 {
    1.0
}

impl OutputRule {
    /// Construit une règle à partir d'un état concret, en désignant l'écran par
    /// son empreinte pour que le profil survive à un changement de connecteur.
    pub fn from_state(state: &OutputState, monitor: Option<&Monitor>) -> Self {
        Self {
            pattern: monitor.map_or_else(|| state.name.clone(), Monitor::fingerprint),
            enabled: state.enabled,
            mode: state.mode.map(|m| m.to_string()),
            position: Some(format!("{}x{}", state.x, state.y)),
            scale: state.scale,
            rotation: state.transform.rotation.degrees(),
            flipped: state.transform.flipped,
            mirror_of: state.mirror_of.clone(),
            vrr: state.vrr,
        }
    }

    /// Cette règle désigne-t-elle cet écran ?
    pub fn matches(&self, m: &Monitor) -> bool {
        m.identifiers()
            .iter()
            .any(|id| glob_match(&self.pattern, id))
    }

    fn to_state(&self, connector: &str) -> Result<OutputState> {
        let mode = match self.mode.as_deref() {
            None | Some("") | Some("preferred") | Some("auto") => None,
            Some(s) => Some(s.parse::<Mode>()?),
        };
        let (x, y) = match self.position.as_deref() {
            None | Some("") | Some("auto") => (0, 0),
            Some(p) => parse_position(p)?,
        };
        Ok(OutputState {
            name: connector.to_string(),
            enabled: self.enabled,
            mode,
            x,
            y,
            scale: self.scale,
            transform: Transform::new(Rotation::from_degrees(self.rotation)?, self.flipped),
            mirror_of: self.mirror_of.clone(),
            vrr: self.vrr,
        })
    }

    /// Position laissée au calcul automatique ?
    fn auto_position(&self) -> bool {
        matches!(self.position.as_deref(), None | Some("") | Some("auto"))
    }
}

/// Analyse `"1920x0"` ou `"1920,0"`.
pub fn parse_position(s: &str) -> Result<(i32, i32)> {
    let s = s.trim();
    let (x, y) = s
        .split_once(['x', 'X', ','])
        .ok_or_else(|| anyhow!("position invalide « {s} » (attendu XxY, par exemple 1920x0)"))?;
    Ok((
        x.trim()
            .parse()
            .map_err(|_| anyhow!("abscisse invalide dans « {s} »"))?,
        y.trim()
            .parse()
            .map_err(|_| anyhow!("ordonnée invalide dans « {s} »"))?,
    ))
}

/// Un agencement nommé, associé à un ensemble d'écrans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Si vrai, le profil ne s'applique que si *tous* les écrans branchés sont
    /// couverts par une règle.
    #[serde(default)]
    pub exact: bool,
    #[serde(default, rename = "output")]
    pub outputs: Vec<OutputRule>,
}

impl Profile {
    /// Associe chaque règle à un écran branché.
    ///
    /// Attribution gloutonne dans l'ordre de déclaration : une règle ne peut pas
    /// voler l'écran déjà pris par une règle précédente. Retourne `None` dès
    /// qu'une règle ne trouve pas preneur — le profil ne correspond alors pas au
    /// matériel présent.
    pub fn assign<'a>(&self, monitors: &'a [Monitor]) -> Option<Vec<(&OutputRule, &'a Monitor)>> {
        let mut taken = vec![false; monitors.len()];
        let mut pairs = Vec::with_capacity(self.outputs.len());
        for rule in &self.outputs {
            let idx = monitors
                .iter()
                .enumerate()
                .position(|(i, m)| !taken[i] && rule.matches(m))?;
            taken[idx] = true;
            pairs.push((rule, &monitors[idx]));
        }
        if self.exact && taken.iter().any(|t| !t) {
            return None;
        }
        Some(pairs)
    }

    pub fn matches(&self, monitors: &[Monitor]) -> bool {
        !self.outputs.is_empty() && self.assign(monitors).is_some()
    }

    /// Traduit le profil en agencement concret pour le matériel branché.
    ///
    /// Les écrans branchés qu'aucune règle ne couvre ne sont pas perdus : ils
    /// sont ajoutés à droite de l'agencement avec leur mode préféré.
    pub fn resolve(&self, monitors: &[Monitor]) -> Result<Layout> {
        let pairs = self.assign(monitors).ok_or_else(|| {
            anyhow!(
                "le profil « {} » ne correspond pas aux écrans branchés",
                self.name
            )
        })?;

        let mut outputs = Vec::new();
        let mut auto_placed = Vec::new();
        for (rule, monitor) in &pairs {
            let mut state = rule.to_state(&monitor.name)?;
            if state.mode.is_none() {
                state.mode = monitor.preferred_mode();
            }
            if rule.auto_position() {
                auto_placed.push(state.name.clone());
            }
            outputs.push(state);
        }

        let covered: Vec<&str> = pairs.iter().map(|(_, m)| m.name.as_str()).collect();
        for m in monitors
            .iter()
            .filter(|m| !covered.contains(&m.name.as_str()))
        {
            let mut state = OutputState::from_monitor(m, monitors);
            state.enabled = true;
            if state.mode.is_none() {
                state.mode = m.preferred_mode();
            }
            auto_placed.push(state.name.clone());
            outputs.push(state);
        }

        let mut layout = Layout::new(outputs);
        place_free_outputs(&mut layout, &auto_placed);
        layout.normalize();
        Ok(layout)
    }
}

/// Pose à droite de l'agencement les écrans dont la position est libre.
fn place_free_outputs(layout: &mut Layout, free: &[String]) {
    let mut cursor = layout
        .active()
        .filter(|o| !free.contains(&o.name))
        .map(|o| o.rect().2)
        .max()
        .unwrap_or(0);
    for name in free {
        let Some(o) = layout.get_mut(name) else {
            continue;
        };
        if !o.occupies_space() {
            continue;
        }
        o.x = cursor;
        o.y = 0;
        cursor += o.logical_size_rounded().0;
    }
}

/// Contenu complet de `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<Profile>,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("lecture de {} impossible", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("{} est mal formé", path.display()))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path();
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("création de {} impossible", dir.display()))?;
        }
        let body = toml::to_string_pretty(self)?;
        crate::emit::write_atomic(path, &body)
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Remplace le profil de même nom, ou l'ajoute.
    pub fn upsert(&mut self, profile: Profile) {
        match self.profiles.iter_mut().find(|p| p.name == profile.name) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() == before {
            bail!("profil « {name} » inconnu");
        }
        Ok(())
    }

    /// Meilleur profil pour le matériel branché.
    ///
    /// Le profil couvrant le plus d'écrans gagne ; à égalité, le premier
    /// déclaré. Un profil `exact` l'emporte sur un profil équivalent qui ne
    /// l'est pas.
    pub fn best_match(&self, monitors: &[Monitor]) -> Option<&Profile> {
        self.profiles
            .iter()
            .filter(|p| p.matches(monitors))
            .enumerate()
            .max_by_key(|(idx, p)| (p.outputs.len(), usize::from(p.exact), usize::MAX - idx))
            .map(|(_, p)| p)
    }
}

/// Correspondance avec `*` comme joker, insensible à la casse.
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    let value = value.trim().to_lowercase();
    if !pattern.contains('*') {
        return pattern == value;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match value[pos..].find(part) {
            Some(found) => {
                // Un motif ne commençant pas par `*` doit être ancré au début.
                if i == 0 && found != 0 {
                    return false;
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }
    // Un motif ne finissant pas par `*` doit être ancré à la fin.
    if let Some(last) = parts.last()
        && !last.is_empty()
        && pos != value.len()
    {
        return false;
    }
    true
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("hyprmc")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn default_monitors_conf() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("hypr")
        .join("monitors.conf")
}

pub fn hyprland_conf() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("hypr")
        .join("hyprland.conf")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, make: &str, model: &str, serial: &str) -> Monitor {
        Monitor {
            id: 0,
            name: name.into(),
            description: format!("{make} {model}").trim().into(),
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            width: 1920,
            height: 1080,
            refresh_rate: 60.0,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
            focused: false,
            disabled: false,
            mirror_of: "none".into(),
            vrr: false,
            available_modes: vec!["1920x1080@60.00Hz".into()],
        }
    }

    fn rule(pattern: &str) -> OutputRule {
        OutputRule {
            pattern: pattern.into(),
            enabled: true,
            mode: None,
            position: None,
            scale: 1.0,
            rotation: 0,
            flipped: false,
            mirror_of: None,
            vrr: false,
        }
    }

    #[test]
    fn glob_matching_handles_anchors_and_wildcards() {
        assert!(glob_match("DP-1", "DP-1"));
        assert!(glob_match("dp-1", "DP-1")); // insensible à la casse
        assert!(!glob_match("DP-1", "DP-11"));
        assert!(glob_match("Dell*", "Dell Inc. U2723QE ABC"));
        assert!(!glob_match("Dell*", "Acer Dell"));
        assert!(glob_match("*U2723QE*", "Dell Inc. U2723QE ABC"));
        assert!(glob_match("*ABC", "Dell Inc. U2723QE ABC"));
        assert!(!glob_match("*ABC", "Dell Inc. U2723QE ABCD"));
        assert!(glob_match("*", "n'importe quoi"));
    }

    #[test]
    fn rule_matches_by_connector_or_fingerprint() {
        let m = monitor("DP-1", "Dell Inc.", "U2723QE", "ABC123");
        assert!(rule("DP-1").matches(&m));
        assert!(rule("Dell Inc. U2723QE ABC123").matches(&m));
        assert!(rule("Dell*").matches(&m));
        assert!(!rule("eDP-1").matches(&m));
    }

    #[test]
    fn profile_does_not_match_when_a_screen_is_missing() {
        let p = Profile {
            name: "desk".into(),
            exact: false,
            outputs: vec![rule("eDP-1"), rule("Dell*")],
        };
        let laptop_only = vec![monitor("eDP-1", "AU Optronics", "0x5799", "")];
        assert!(!p.matches(&laptop_only));

        let docked = vec![
            monitor("eDP-1", "AU Optronics", "0x5799", ""),
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC123"),
        ];
        assert!(p.matches(&docked));
    }

    #[test]
    fn two_rules_cannot_claim_the_same_screen() {
        let p = Profile {
            name: "double".into(),
            exact: false,
            outputs: vec![rule("Dell*"), rule("Dell*")],
        };
        let one_dell = vec![monitor("DP-1", "Dell Inc.", "U2723QE", "ABC")];
        assert!(!p.matches(&one_dell));

        let two_dells = vec![
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC"),
            monitor("DP-2", "Dell Inc.", "U2723QE", "DEF"),
        ];
        assert!(p.matches(&two_dells));
    }

    #[test]
    fn exact_profile_rejects_extra_screens() {
        let p = Profile {
            name: "solo".into(),
            exact: true,
            outputs: vec![rule("eDP-1")],
        };
        assert!(p.matches(&[monitor("eDP-1", "AU", "X", "")]));
        assert!(!p.matches(&[
            monitor("eDP-1", "AU", "X", ""),
            monitor("DP-1", "Dell", "U", "")
        ]));
    }

    #[test]
    fn best_match_prefers_the_most_specific_profile() {
        let cfg = Config {
            settings: Settings::default(),
            profiles: vec![
                Profile {
                    name: "solo".into(),
                    exact: false,
                    outputs: vec![rule("eDP-1")],
                },
                Profile {
                    name: "desk".into(),
                    exact: false,
                    outputs: vec![rule("eDP-1"), rule("Dell*")],
                },
            ],
        };
        let laptop = vec![monitor("eDP-1", "AU", "X", "")];
        assert_eq!(cfg.best_match(&laptop).unwrap().name, "solo");

        let docked = vec![
            monitor("eDP-1", "AU", "X", ""),
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC"),
        ];
        assert_eq!(cfg.best_match(&docked).unwrap().name, "desk");
    }

    #[test]
    fn best_match_returns_nothing_without_a_candidate() {
        let cfg = Config::default();
        assert!(cfg.best_match(&[monitor("eDP-1", "AU", "X", "")]).is_none());
    }

    #[test]
    fn resolve_produces_connector_names_not_patterns() {
        let p = Profile {
            name: "desk".into(),
            exact: false,
            outputs: vec![OutputRule {
                position: Some("0x0".into()),
                ..rule("Dell*")
            }],
        };
        let monitors = vec![monitor("DP-3", "Dell Inc.", "U2723QE", "ABC")];
        let layout = p.resolve(&monitors).unwrap();
        assert_eq!(layout.outputs[0].name, "DP-3");
        assert_eq!(layout.outputs[0].mode.unwrap().width, 1920);
    }

    #[test]
    fn resolve_adopts_uncovered_screens_instead_of_dropping_them() {
        let p = Profile {
            name: "desk".into(),
            exact: false,
            outputs: vec![OutputRule {
                position: Some("0x0".into()),
                ..rule("eDP-1")
            }],
        };
        let monitors = vec![
            monitor("eDP-1", "AU", "X", ""),
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC"),
        ];
        let layout = p.resolve(&monitors).unwrap();
        assert_eq!(layout.outputs.len(), 2);
        let extra = layout.get("DP-1").unwrap();
        assert!(extra.enabled);
        // Posé à droite de l'écran déjà placé, sans chevauchement.
        assert_eq!(extra.x, 1920);
        assert!(!layout.has_errors());
    }

    #[test]
    fn resolve_places_auto_positioned_rules_side_by_side() {
        let p = Profile {
            name: "desk".into(),
            exact: false,
            outputs: vec![rule("eDP-1"), rule("Dell*")],
        };
        let monitors = vec![
            monitor("eDP-1", "AU", "X", ""),
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC"),
        ];
        let layout = p.resolve(&monitors).unwrap();
        assert!(!layout.has_errors());
        assert_eq!(layout.get("eDP-1").unwrap().x, 0);
        assert_eq!(layout.get("DP-1").unwrap().x, 1920);
    }

    #[test]
    fn disabled_rule_survives_resolution() {
        let p = Profile {
            name: "closed".into(),
            exact: false,
            outputs: vec![
                OutputRule {
                    enabled: false,
                    ..rule("eDP-1")
                },
                OutputRule {
                    position: Some("0x0".into()),
                    ..rule("Dell*")
                },
            ],
        };
        let monitors = vec![
            monitor("eDP-1", "AU", "X", ""),
            monitor("DP-1", "Dell Inc.", "U2723QE", "ABC"),
        ];
        let layout = p.resolve(&monitors).unwrap();
        assert!(!layout.get("eDP-1").unwrap().enabled);
        assert!(!layout.has_errors());
    }

    #[test]
    fn rotation_and_flip_survive_a_round_trip_through_toml() {
        let cfg = Config {
            settings: Settings::default(),
            profiles: vec![Profile {
                name: "portrait".into(),
                exact: false,
                outputs: vec![OutputRule {
                    rotation: 270,
                    flipped: true,
                    position: Some("0x0".into()),
                    ..rule("DP-1")
                }],
            }],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        let layout = back.profiles[0]
            .resolve(&[monitor("DP-1", "Dell", "U", "")])
            .unwrap();
        let t = layout.outputs[0].transform;
        assert_eq!(t.rotation, Rotation::R270);
        assert!(t.flipped);
        assert_eq!(t.to_u8(), 7);
    }

    #[test]
    fn invalid_rotation_in_config_is_reported() {
        let p = Profile {
            name: "x".into(),
            exact: false,
            outputs: vec![OutputRule {
                rotation: 45,
                ..rule("DP-1")
            }],
        };
        let err = p.resolve(&[monitor("DP-1", "Dell", "U", "")]).unwrap_err();
        assert!(err.to_string().contains("rotation invalide"));
    }

    #[test]
    fn position_parsing() {
        assert_eq!(parse_position("1920x0").unwrap(), (1920, 0));
        assert_eq!(parse_position("-1920,-100").unwrap(), (-1920, -100));
        assert!(parse_position("1920").is_err());
    }

    #[test]
    fn upsert_replaces_and_remove_reports_unknown() {
        let mut cfg = Config::default();
        cfg.upsert(Profile {
            name: "a".into(),
            exact: false,
            outputs: vec![rule("X")],
        });
        cfg.upsert(Profile {
            name: "a".into(),
            exact: true,
            outputs: vec![rule("Y")],
        });
        assert_eq!(cfg.profiles.len(), 1);
        assert!(cfg.profiles[0].exact);
        assert!(cfg.remove("inconnu").is_err());
        assert!(cfg.remove("a").is_ok());
    }

    #[test]
    fn missing_config_file_yields_defaults() {
        let cfg = Config::load_from(Path::new("/inexistant/hyprmc/config.toml")).unwrap();
        assert!(cfg.profiles.is_empty());
        assert_eq!(cfg.settings.web_port, 8787);
    }
}
