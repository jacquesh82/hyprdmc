//! Keyboard and pointer settings.
//!
//! Deliberately kept out of the screen profiles: which layout you type in and
//! which way your touchpad scrolls have nothing to do with which monitors are
//! plugged in. Docking a laptop must not silently switch the keyboard, so this
//! lives in its own section of `config.toml` and its own generated file, wired
//! into the compositor's configuration alongside the monitor one.
//!
//! Neither writing nor reading these settings is this module's business: the
//! syntax belongs to whichever compositor is in play
//! ([`crate::compositor::Compositor::input_directives`]) and so does reading them
//! back ([`crate::session::Session::read_input`]). What is here is the model, its
//! validation, and the xkb catalogue the UI needs to offer a choice at all —
//! none of which is compositor-specific.

use anyhow::Result;
use rust_i18n::t;
use serde::{Deserialize, Serialize};

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
    fn a_layout_is_the_one_thing_that_cannot_be_empty() {
        let mut cfg = InputConfig {
            kb_variant: String::new(),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
        cfg.kb_layout = "  ".into();
        assert!(cfg.validate().is_err());
    }
}
