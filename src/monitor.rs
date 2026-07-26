//! The model of a monitor as reported by Hyprland, plus the rotation and
//! mode types that revolve around it.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

/// Rotation applied to a monitor, in clockwise degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub fn degrees(self) -> u16 {
        match self {
            Rotation::R0 => 0,
            Rotation::R90 => 90,
            Rotation::R180 => 180,
            Rotation::R270 => 270,
        }
    }

    pub fn from_degrees(deg: u16) -> Result<Self> {
        Ok(match deg % 360 {
            0 => Rotation::R0,
            90 => Rotation::R90,
            180 => Rotation::R180,
            270 => Rotation::R270,
            other => bail!(t!("monitor.invalid_rotation", degrees = other.to_string()).to_string()),
        })
    }

    /// True when the rotation swaps width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::R90 | Rotation::R270)
    }

    fn index(self) -> u8 {
        match self {
            Rotation::R0 => 0,
            Rotation::R90 => 1,
            Rotation::R180 => 2,
            Rotation::R270 => 3,
        }
    }
}

impl fmt::Display for Rotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            t!("monitor.rotation", degrees = self.degrees().to_string())
        )
    }
}

/// Rotation + flip, encoded into Hyprland's single `transform` value.
///
/// | transform | 0 | 1  | 2   | 3   | 4 | 5  | 6   | 7   |
/// |-----------|---|----|-----|-----|---|----|-----|-----|
/// | rotation  | 0 | 90 | 180 | 270 | 0 | 90 | 180 | 270 |
/// | flipped   | n | n  | n   | n   | y | y  | y   | y   |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Transform {
    pub rotation: Rotation,
    pub flipped: bool,
}

impl Transform {
    pub fn new(rotation: Rotation, flipped: bool) -> Self {
        Self { rotation, flipped }
    }

    pub fn to_u8(self) -> u8 {
        self.rotation.index() + if self.flipped { 4 } else { 0 }
    }

    pub fn from_u8(v: u8) -> Result<Self> {
        if v > 7 {
            bail!(t!("monitor.invalid_transform", value = v.to_string()).to_string());
        }
        let rotation = match v % 4 {
            0 => Rotation::R0,
            1 => Rotation::R90,
            2 => Rotation::R180,
            _ => Rotation::R270,
        };
        Ok(Self {
            rotation,
            flipped: v >= 4,
        })
    }

    pub fn swaps_axes(self) -> bool {
        self.rotation.swaps_axes()
    }
}

impl fmt::Display for Transform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.flipped {
            write!(
                f,
                "{}",
                t!(
                    "monitor.orientation.flipped",
                    rotation = self.rotation.to_string()
                )
            )
        } else {
            write!(f, "{}", self.rotation)
        }
    }
}

/// A display mode: resolution + refresh rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    pub refresh: f64,
}

impl Mode {
    pub fn new(width: i32, height: i32, refresh: f64) -> Self {
        Self {
            width,
            height,
            refresh,
        }
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    /// Accepts `1920x1080`, `1920x1080@60`, `1920x1080@60.06Hz`.
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().trim_end_matches("Hz").trim_end_matches("hz");
        let (res, refresh) = match s.split_once('@') {
            Some((res, rate)) => (
                res,
                rate.trim()
                    .parse::<f64>()
                    .map_err(|_| anyhow!(t!("monitor.invalid_refresh", value = s).to_string()))?,
            ),
            None => (s, 0.0),
        };
        let (w, h) = res
            .split_once(['x', 'X'])
            .ok_or_else(|| anyhow!(t!("monitor.invalid_mode", value = s).to_string()))?;
        Ok(Mode {
            width: w
                .trim()
                .parse()
                .map_err(|_| anyhow!(t!("monitor.invalid_width", value = s).to_string()))?,
            height: h
                .trim()
                .parse()
                .map_err(|_| anyhow!(t!("monitor.invalid_height", value = s).to_string()))?,
            refresh,
        })
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.refresh > 0.0 {
            write!(f, "{}x{}@{:.2}", self.width, self.height, self.refresh)
        } else {
            write!(f, "{}x{}", self.width, self.height)
        }
    }
}

fn default_scale() -> f64 {
    1.0
}

/// A monitor as reported by `j/monitors all`.
///
/// Fields not listed here are deliberately ignored: Hyprland adds new ones
/// regularly, and `serde` drops them without breaking deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitor {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(rename = "refreshRate", default)]
    pub refresh_rate: f64,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub transform: u8,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(rename = "mirrorOf", default)]
    pub mirror_of: String,
    #[serde(default)]
    pub vrr: bool,
    #[serde(rename = "availableModes", default)]
    pub available_modes: Vec<String>,
}

impl Monitor {
    pub fn mode(&self) -> Mode {
        Mode::new(self.width, self.height, self.refresh_rate)
    }

    pub fn transform(&self) -> Transform {
        Transform::from_u8(self.transform).unwrap_or_default()
    }

    /// The output mirrored by this one, resolved to a connector name.
    ///
    /// Hyprland publishes the source monitor's **numeric id** here
    /// (`"0"`), whereas the configuration directive expects its name
    /// (`eDP-1`): without this resolution, any comparison would fail.
    /// Versions that publish a name directly are still handled.
    pub fn mirror_target(&self, all: &[Monitor]) -> Option<String> {
        let raw = self.mirror_of.trim();
        if raw.is_empty() || raw == "none" {
            return None;
        }
        if let Ok(id) = raw.parse::<i64>()
            && let Some(source) = all.iter().find(|m| m.id == id)
        {
            return Some(source.name.clone());
        }
        Some(raw.to_string())
    }

    /// Stable identifier for a monitor, independent of the connector it is
    /// plugged into: `"make model serial"`.
    ///
    /// Some monitors (laptop panels in particular) don't publish a usable
    /// serial or make — we then fall back to the description, then to the
    /// connector name.
    pub fn fingerprint(&self) -> String {
        let parts: Vec<&str> = [
            self.make.as_str(),
            self.model.as_str(),
            self.serial.as_str(),
        ]
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
        if !parts.is_empty() {
            return parts.join(" ");
        }
        if !self.description.trim().is_empty() {
            return self.description.trim().to_string();
        }
        self.name.clone()
    }

    /// Every string by which a profile rule can designate this monitor, from
    /// the most specific to the most generic.
    pub fn identifiers(&self) -> Vec<String> {
        let mut ids = vec![self.name.clone(), self.fingerprint()];
        let desc = self.description.trim();
        if !desc.is_empty() {
            ids.push(desc.to_string());
            // Hyprland also accepts the `desc:<description>` form.
            ids.push(format!("desc:{desc}"));
        }
        ids.dedup();
        ids
    }

    /// Available modes, parsed and deduplicated.
    pub fn parsed_modes(&self) -> Vec<Mode> {
        let mut modes: Vec<Mode> = self
            .available_modes
            .iter()
            .filter_map(|m| m.parse::<Mode>().ok())
            .collect();
        modes.sort_by(|a, b| {
            (b.width * b.height)
                .cmp(&(a.width * a.height))
                .then(b.refresh.total_cmp(&a.refresh))
        });
        modes
    }

    /// Preferred mode: the largest, then the fastest.
    pub fn preferred_mode(&self) -> Option<Mode> {
        self.parsed_modes().into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_encode_decode_roundtrip() {
        for v in 0u8..=7 {
            let t = Transform::from_u8(v).unwrap();
            assert_eq!(t.to_u8(), v, "round trip broken for transform={v}");
        }
        assert!(Transform::from_u8(8).is_err());
    }

    #[test]
    fn transform_layout_matches_hyprland_table() {
        assert_eq!(Transform::new(Rotation::R0, false).to_u8(), 0);
        assert_eq!(Transform::new(Rotation::R90, false).to_u8(), 1);
        assert_eq!(Transform::new(Rotation::R180, false).to_u8(), 2);
        assert_eq!(Transform::new(Rotation::R270, false).to_u8(), 3);
        assert_eq!(Transform::new(Rotation::R0, true).to_u8(), 4);
        assert_eq!(Transform::new(Rotation::R270, true).to_u8(), 7);

        let t = Transform::from_u8(5).unwrap();
        assert_eq!(t.rotation, Rotation::R90);
        assert!(t.flipped);
    }

    #[test]
    fn rotation_axis_swap() {
        assert!(!Rotation::R0.swaps_axes());
        assert!(Rotation::R90.swaps_axes());
        assert!(!Rotation::R180.swaps_axes());
        assert!(Rotation::R270.swaps_axes());
    }

    #[test]
    fn rotation_from_degrees() {
        assert_eq!(Rotation::from_degrees(90).unwrap(), Rotation::R90);
        assert_eq!(Rotation::from_degrees(360).unwrap(), Rotation::R0);
        assert!(Rotation::from_degrees(45).is_err());
    }

    #[test]
    fn mode_parsing_accepts_hyprland_forms() {
        assert_eq!(
            "1920x1080".parse::<Mode>().unwrap(),
            Mode::new(1920, 1080, 0.0)
        );
        assert_eq!(
            "1920x1080@60".parse::<Mode>().unwrap(),
            Mode::new(1920, 1080, 60.0)
        );
        assert_eq!(
            "1920x1080@60.06Hz".parse::<Mode>().unwrap(),
            Mode::new(1920, 1080, 60.06)
        );
        assert!("nawak".parse::<Mode>().is_err());
        assert!("1920x@60".parse::<Mode>().is_err());
    }

    fn sample(make: &str, model: &str, serial: &str, desc: &str) -> Monitor {
        Monitor {
            id: 0,
            name: "eDP-1".into(),
            description: desc.into(),
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            width: 1920,
            height: 1080,
            refresh_rate: 60.056,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
            focused: true,
            disabled: false,
            mirror_of: "none".into(),
            vrr: false,
            available_modes: vec!["1920x1080@60.06Hz".into(), "1280x720@60.00Hz".into()],
        }
    }

    #[test]
    fn fingerprint_prefers_make_model_serial() {
        let m = sample("Dell Inc.", "U2723QE", "ABC123", "Dell Inc. U2723QE");
        assert_eq!(m.fingerprint(), "Dell Inc. U2723QE ABC123");
    }

    #[test]
    fn fingerprint_skips_empty_parts() {
        // Real-world case: laptop panel with an empty serial.
        let m = sample("AU Optronics", "0x5799", "", "AU Optronics 0x5799");
        assert_eq!(m.fingerprint(), "AU Optronics 0x5799");
    }

    #[test]
    fn fingerprint_falls_back_to_connector_name() {
        let m = sample("", "", "", "");
        assert_eq!(m.fingerprint(), "eDP-1");
    }

    #[test]
    fn preferred_mode_is_largest_then_fastest() {
        let m = sample("x", "y", "z", "");
        assert_eq!(m.preferred_mode().unwrap(), Mode::new(1920, 1080, 60.06));
    }

    #[test]
    fn mirror_target_normalises_none() {
        let mut m = sample("x", "y", "z", "");
        assert_eq!(m.mirror_target(&[]), None);
        m.mirror_of = String::new();
        assert_eq!(m.mirror_target(&[]), None);
    }

    #[test]
    fn mirror_target_resolves_the_numeric_id_hyprland_publishes() {
        // Hyprland reports mirrorOf: "0" to designate the monitor with id 0.
        let mut source = sample("x", "y", "z", "");
        source.id = 0;
        source.name = "eDP-1".into();

        let mut mirrored = sample("a", "b", "c", "");
        mirrored.id = 1;
        mirrored.name = "DP-1".into();
        mirrored.mirror_of = "0".into();

        let all = vec![source, mirrored.clone()];
        assert_eq!(mirrored.mirror_target(&all), Some("eDP-1".to_string()));
    }

    #[test]
    fn mirror_target_accepts_a_plain_name_too() {
        let mut m = sample("x", "y", "z", "");
        m.mirror_of = "DP-1".into();
        assert_eq!(m.mirror_target(&[]), Some("DP-1".to_string()));
    }
}
