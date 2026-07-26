//! Keyboard and pointer settings.
//!
//! Deliberately kept out of the screen profiles: which layout you type in and
//! which way your touchpad scrolls have nothing to do with which monitors are
//! plugged in. Docking a laptop must not silently switch the keyboard, so this
//! lives in its own section of `config.toml` and its own generated file
//! (`input.lua`), wired into `hyprland.lua` alongside `monitors.lua`.
//!
//! Everything goes through `hl.config{…}`: since Hyprland 0.55 the
//! configuration is Lua and `keyword` is refused outright, exactly as for
//! monitors (see [`crate::ipc::HyprBackend::set_monitors`]).

use anyhow::{Context, Result};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::ipc::HyprBackend;
use crate::layout::lua_string;

/// Where the xkb catalogue lives on a standard system. `base.lst` ships with
/// `xkeyboard-config`; if it is missing we fall back to a short built-in list
/// rather than showing an empty dropdown.
const XKB_RULES: &[&str] = &[
    "/usr/share/X11/xkb/rules/base.lst",
    "/usr/share/X11/xkb/rules/evdev.lst",
];

/// Keyboard and pointer settings, as hyprdmc knows how to set them.
///
/// Empty strings mean "unset": `kb_variant = ""` is how xkb spells "the plain
/// layout", so there is no need for an `Option` here — and it is what
/// Hyprland itself reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct InputConfig {
    /// xkb layout code, e.g. `fr`. Several may be comma-separated.
    pub kb_layout: String,
    /// xkb variant, e.g. `oss`. Empty for the plain layout.
    pub kb_variant: String,
    /// xkb options, e.g. `compose:ralt`. Comma-separated.
    pub kb_options: String,
    /// Scroll direction for mice.
    pub natural_scroll: bool,
    /// Scroll direction for touchpads — a separate setting in Hyprland, and
    /// separate in most people's habits too: natural on the touchpad, normal
    /// on the wheel is a common pairing.
    pub touchpad_natural_scroll: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        // Hyprland's own defaults, so a config file that has never been
        // written describes the same thing the compositor already does.
        Self {
            kb_layout: "us".to_string(),
            kb_variant: String::new(),
            kb_options: String::new(),
            natural_scroll: false,
            touchpad_natural_scroll: false,
        }
    }
}

impl InputConfig {
    /// Reads what the compositor is currently using.
    ///
    /// The live state is the source of truth: the user may well have set
    /// `kb_layout` by hand in `hyprland.lua` long before hyprdmc existed, and
    /// the UI must show that rather than a default we invented.
    pub fn read(backend: &dyn HyprBackend) -> Result<Self> {
        Ok(Self {
            kb_layout: get_string(backend, "input:kb_layout")?,
            kb_variant: get_string(backend, "input:kb_variant")?,
            kb_options: get_string(backend, "input:kb_options")?,
            natural_scroll: get_bool(backend, "input:natural_scroll")?,
            touchpad_natural_scroll: get_bool(backend, "input:touchpad:natural_scroll")?,
        })
    }

    /// The single `hl.config{…}` call that carries every setting.
    ///
    /// One call rather than one per field: the compositor reconfigures the
    /// devices once, and a half-applied keyboard is not a state anyone wants
    /// to debug.
    pub fn to_lua(&self) -> String {
        format!(
            "hl.config({{ input = {{ kb_layout = {}, kb_variant = {}, kb_options = {}, \
             natural_scroll = {}, touchpad = {{ natural_scroll = {} }} }} }})",
            lua_string(&self.kb_layout),
            lua_string(&self.kb_variant),
            lua_string(&self.kb_options),
            self.natural_scroll,
            self.touchpad_natural_scroll,
        )
    }

    /// Applies the settings to the running compositor.
    pub fn apply(&self, backend: &dyn HyprBackend) -> Result<()> {
        backend.eval(&self.to_lua())
    }

    /// Rejects what Hyprland would only complain about later.
    ///
    /// Only the layout is required: an empty variant or options string is the
    /// normal way to say "none".
    pub fn validate(&self) -> Result<()> {
        if self.kb_layout.trim().is_empty() {
            anyhow::bail!(t!("input.layout_required").to_string());
        }
        Ok(())
    }
}

/// Reads a string option through `getoption`.
fn get_string(backend: &dyn HyprBackend, option: &str) -> Result<String> {
    let value = get_option(backend, option)?;
    Ok(value
        .get("str")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        // Hyprland reports an unset string option as the literal
        // `[[EMPTY]]`, which is not something to show in a text field.
        .replace("[[EMPTY]]", "")
        .to_string())
}

/// Reads a boolean option through `getoption`.
///
/// Hyprland answers with `bool` for these, but older versions used `int`;
/// both are accepted so a version bump does not silently read as `false`.
fn get_bool(backend: &dyn HyprBackend, option: &str) -> Result<bool> {
    let value = get_option(backend, option)?;
    Ok(value
        .get("bool")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| value.get("int").and_then(|v| v.as_i64()).map(|i| i != 0))
        .unwrap_or(false))
}

fn get_option(backend: &dyn HyprBackend, option: &str) -> Result<serde_json::Value> {
    let raw = backend.query(&format!("j/getoption {option}"))?;
    serde_json::from_str(&raw)
        .with_context(|| t!("input.unreadable_option", option = option).to_string())
}

/// One selectable entry of the xkb catalogue: the code Hyprland wants, and
/// the description a human recognises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub code: String,
    pub label: String,
    /// Layout this variant belongs to. Always empty for layouts and options.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub layout: String,
}

/// Everything the UI needs to populate its dropdowns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub layouts: Vec<Entry>,
    pub variants: Vec<Entry>,
    pub options: Vec<Entry>,
}

/// Reads the xkb catalogue, falling back to a built-in shortlist.
///
/// The list is on disk on any system with `xkeyboard-config` installed, which
/// is anything running a Wayland compositor — but a container or a stripped
/// image may not have it, and a keyboard dropdown with nothing in it is worse
/// than a short one.
pub fn catalog() -> Catalog {
    for path in XKB_RULES {
        if let Ok(text) = std::fs::read_to_string(path) {
            let catalog = parse_rules(&text);
            if !catalog.layouts.is_empty() {
                return catalog;
            }
        }
    }
    fallback_catalog()
}

/// Parses the `! layout` / `! variant` / `! option` sections of an xkb rules
/// list.
///
/// Format, one entry per line: a code, whitespace, then a description.
/// Variants prefix their description with `layout: `, which is how each one is
/// attached to its layout. Option lines without a `:` in the code are section
/// headings ("Switching to another layout") rather than options, and are
/// skipped.
fn parse_rules(text: &str) -> Catalog {
    let mut catalog = Catalog::default();
    let mut section = "";

    for line in text.lines() {
        if let Some(name) = line.strip_prefix('!') {
            section = name.split_whitespace().next().unwrap_or("");
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((code, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim();

        match section {
            "layout" => catalog.layouts.push(Entry {
                code: code.to_string(),
                label: rest.to_string(),
                layout: String::new(),
            }),
            "variant" => {
                let (layout, label) = rest.split_once(": ").unwrap_or(("", rest));
                catalog.variants.push(Entry {
                    code: code.to_string(),
                    label: label.to_string(),
                    layout: layout.to_string(),
                });
            }
            "option" if code.contains(':') => catalog.options.push(Entry {
                code: code.to_string(),
                label: rest.to_string(),
                layout: String::new(),
            }),
            _ => {}
        }
    }
    catalog
}

/// Enough to configure the common cases when xkb's own list is unavailable.
fn fallback_catalog() -> Catalog {
    let layouts = [
        ("us", "English (US)"),
        ("fr", "French"),
        ("de", "German"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("gb", "English (UK)"),
        ("be", "Belgian"),
        ("ch", "Swiss"),
        ("ca", "French (Canada)"),
    ];
    Catalog {
        layouts: layouts
            .iter()
            .map(|(code, label)| Entry {
                code: (*code).to_string(),
                label: (*label).to_string(),
                layout: String::new(),
            })
            .collect(),
        variants: Vec::new(),
        options: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::fake::FakeBackend;

    const RULES: &str = "\
! model
  pc105           Generic 105-key PC

! layout
  us              English (US)
  fr              French

! variant
  oss             fr: French (alt.)
  dvorak          us: English (Dvorak)

! option
  grp             Switching to another layout
  grp:alt_shift_toggle  Alt+Shift
  compose:ralt    Right Alt
";

    #[test]
    fn the_catalogue_keeps_codes_and_descriptions_apart() {
        let catalog = parse_rules(RULES);
        assert_eq!(catalog.layouts.len(), 2);
        assert_eq!(catalog.layouts[1].code, "fr");
        assert_eq!(catalog.layouts[1].label, "French");
    }

    #[test]
    fn a_variant_remembers_which_layout_it_belongs_to() {
        let catalog = parse_rules(RULES);
        let oss = catalog.variants.iter().find(|v| v.code == "oss").unwrap();
        assert_eq!(oss.layout, "fr");
        assert_eq!(oss.label, "French (alt.)");
    }

    #[test]
    fn option_section_headings_are_not_options() {
        let catalog = parse_rules(RULES);
        assert!(
            !catalog.options.iter().any(|o| o.code == "grp"),
            "\"grp\" is a heading, not something to select"
        );
        assert_eq!(catalog.options.len(), 2);
    }

    #[test]
    fn a_missing_rules_file_still_offers_something_to_pick() {
        let catalog = fallback_catalog();
        assert!(catalog.layouts.iter().any(|l| l.code == "fr"));
    }

    #[test]
    fn settings_travel_as_a_single_nested_config_call() {
        let cfg = InputConfig {
            kb_layout: "fr".into(),
            kb_variant: "oss".into(),
            kb_options: "compose:ralt".into(),
            natural_scroll: false,
            touchpad_natural_scroll: true,
        };
        let lua = cfg.to_lua();
        assert!(lua.starts_with("hl.config({ input = {"));
        assert!(lua.contains(r#"kb_layout = "fr""#));
        assert!(lua.contains(r#"kb_variant = "oss""#));
        assert!(lua.contains("natural_scroll = false"));
        assert!(lua.contains("touchpad = { natural_scroll = true }"));
    }

    #[test]
    fn a_layout_is_the_one_thing_that_cannot_be_empty() {
        let mut cfg = InputConfig {
            kb_variant: String::new(),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
        cfg.kb_layout = "  ".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn live_values_are_read_from_the_compositor() {
        let backend = FakeBackend::with_options(&[
            (
                "input:kb_layout",
                r#"{"option":"input:kb_layout","str":"fr","set":true}"#,
            ),
            (
                "input:kb_variant",
                r#"{"option":"input:kb_variant","str":"[[EMPTY]]","set":false}"#,
            ),
            (
                "input:kb_options",
                r#"{"option":"input:kb_options","str":"compose:ralt","set":true}"#,
            ),
            (
                "input:natural_scroll",
                r#"{"option":"input:natural_scroll","bool":false,"set":false}"#,
            ),
            (
                "input:touchpad:natural_scroll",
                r#"{"option":"input:touchpad:natural_scroll","bool":true,"set":true}"#,
            ),
        ]);
        let cfg = InputConfig::read(&backend).unwrap();
        assert_eq!(cfg.kb_layout, "fr");
        assert_eq!(cfg.kb_variant, "", "[[EMPTY]] is not a variant name");
        assert_eq!(cfg.kb_options, "compose:ralt");
        assert!(!cfg.natural_scroll);
        assert!(cfg.touchpad_natural_scroll);
    }

    #[test]
    fn applying_sends_one_eval() {
        let backend = FakeBackend::default();
        InputConfig::default().apply(&backend).unwrap();
        let sent = backend.sent_commands();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("/eval hl.config({ input = {"));
    }
}
