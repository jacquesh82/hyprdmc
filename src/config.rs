//! User profiles: reading/writing `~/.config/hyprdmc/config.toml` and
//! selecting the profile that matches the connected hardware.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::layout::{Layout, OutputState};
use crate::monitor::{Mode, Monitor, Rotation, Transform};

/// Global settings for the daemon and the web server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Listening port for the web interface.
    pub web_port: u16,
    /// Listening address. Local by default: the API drives the display, it
    /// has no business being on the network without an explicit decision.
    pub bind: String,
    /// Automatically apply the profile matching the current hardware.
    pub auto_apply: bool,
    /// Delay before automatically reverting if the user does not confirm.
    /// `0` disables the safety net.
    pub confirm_timeout_secs: u64,
    /// Generated file, to be sourced from `hyprland.conf`.
    pub monitors_conf: PathBuf,
    /// Interface language (`en`, `fr`). Unset means "follow the system
    /// locale"; see [`crate::i18n`] for the full resolution order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            web_port: 8787,
            bind: "127.0.0.1".to_string(),
            auto_apply: true,
            confirm_timeout_secs: 10,
            monitors_conf: default_monitors_conf(),
            language: None,
        }
    }
}

/// Rule describing how to configure a given output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputRule {
    /// Pattern designating the output: connector name (`DP-1`), fingerprint
    /// (`Dell Inc. U2723QE ABC123`), or a pattern with `*`.
    #[serde(rename = "match")]
    pub pattern: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `None` = the output's preferred mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `None` or `"auto"` = position computed automatically.
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
    /// Builds a rule from a concrete state, identifying the output by its
    /// fingerprint so the profile survives a connector change.
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

    /// Does this rule designate this output?
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

    /// Is the position left to automatic placement?
    fn auto_position(&self) -> bool {
        matches!(self.position.as_deref(), None | Some("") | Some("auto"))
    }
}

/// Parses `"1920x0"` or `"1920,0"`.
pub fn parse_position(s: &str) -> Result<(i32, i32)> {
    let s = s.trim();
    let (x, y) = s
        .split_once(['x', 'X', ','])
        .ok_or_else(|| anyhow!(t!("config.invalid_position", value = s).to_string()))?;
    Ok((
        x.trim()
            .parse()
            .map_err(|_| anyhow!(t!("config.invalid_x", value = s).to_string()))?,
        y.trim()
            .parse()
            .map_err(|_| anyhow!(t!("config.invalid_y", value = s).to_string()))?,
    ))
}

/// A named layout, associated with a set of outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// If true, the profile only applies when *all* connected outputs are
    /// covered by a rule.
    #[serde(default)]
    pub exact: bool,
    #[serde(default, rename = "output")]
    pub outputs: Vec<OutputRule>,
}

impl Profile {
    /// Assigns each rule to a connected output.
    ///
    /// Greedy assignment in declaration order: a rule cannot steal an output
    /// already claimed by an earlier rule. Returns `None` as soon as a rule
    /// finds no taker — the profile then does not match the hardware present.
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

    /// Translates the profile into a concrete layout for the connected
    /// hardware.
    ///
    /// Connected outputs that no rule covers are not lost: they are appended
    /// to the right of the layout with their preferred mode.
    pub fn resolve(&self, monitors: &[Monitor]) -> Result<Layout> {
        let pairs = self.assign(monitors).ok_or_else(|| {
            anyhow!(t!("config.profile_mismatch", name = self.name.clone()).to_string())
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

/// Places, to the right of the layout, the outputs whose position is free.
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

/// Full contents of `config.toml`.
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
            .with_context(|| t!("fs.read_failed", path = path.display().to_string()).to_string())?;
        toml::from_str(&raw)
            .with_context(|| t!("config.malformed", path = path.display().to_string()).to_string())
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path();
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| {
                t!("fs.create_dir_failed", path = dir.display().to_string()).to_string()
            })?;
        }
        let body = toml::to_string_pretty(self)?;
        crate::emit::write_atomic(path, &body)
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Replaces the profile with the same name, or appends it.
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
            bail!(t!("config.unknown_profile", name = name).to_string());
        }
        Ok(())
    }

    /// Best profile for the connected hardware.
    ///
    /// The profile covering the most outputs wins; ties go to the first one
    /// declared. An `exact` profile beats an otherwise equivalent one that
    /// is not.
    pub fn best_match(&self, monitors: &[Monitor]) -> Option<&Profile> {
        self.profiles
            .iter()
            .filter(|p| p.matches(monitors))
            .enumerate()
            .max_by_key(|(idx, p)| (p.outputs.len(), usize::from(p.exact), usize::MAX - idx))
            .map(|(_, p)| p)
    }
}

/// Matching with `*` as a wildcard, case-insensitive.
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
                // A pattern that doesn't start with `*` must be anchored at the start.
                if i == 0 && found != 0 {
                    return false;
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }
    // A pattern that doesn't end with `*` must be anchored at the end.
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
        .join("hyprdmc")
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
        assert!(glob_match("dp-1", "DP-1")); // case-insensitive
        assert!(!glob_match("DP-1", "DP-11"));
        assert!(glob_match("Dell*", "Dell Inc. U2723QE ABC"));
        assert!(!glob_match("Dell*", "Acer Dell"));
        assert!(glob_match("*U2723QE*", "Dell Inc. U2723QE ABC"));
        assert!(glob_match("*ABC", "Dell Inc. U2723QE ABC"));
        assert!(!glob_match("*ABC", "Dell Inc. U2723QE ABCD"));
        assert!(glob_match("*", "anything"));
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
        // Placed to the right of the already-positioned output, without overlap.
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
        assert!(err.to_string().contains("invalid rotation"));
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
        assert!(cfg.remove("unknown").is_err());
        assert!(cfg.remove("a").is_ok());
    }

    #[test]
    fn missing_config_file_yields_defaults() {
        let cfg = Config::load_from(Path::new("/nonexistent/hyprdmc/config.toml")).unwrap();
        assert!(cfg.profiles.is_empty());
        assert_eq!(cfg.settings.web_port, 8787);
    }
}
