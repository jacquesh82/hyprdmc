//! Locale selection.
//!
//! Every message the user can read goes through `t!()` and lives in
//! `locales/app.yml`. Only developer-facing text — log lines, `--help`, error
//! context aimed at bug reports — stays hardcoded in English.
//!
//! Resolution order, most specific first:
//!
//! 1. `HYPRDMC_LANG` — explicit override, handy for scripts and bug reports.
//! 2. `language` in `config.toml` — the user's lasting preference.
//! 3. `LC_ALL`, `LC_MESSAGES`, `LANG` — the usual POSIX chain.
//! 4. English.

/// Languages shipped with the binary. Adding one means adding a section to
/// `locales/app.yml` and a variant here.
pub const AVAILABLE: &[&str] = &["en", "fr"];

/// Language used when nothing else matches.
pub const FALLBACK: &str = "en";

/// Applies a locale, falling back to English if it is not shipped.
pub fn set(locale: &str) {
    let chosen = normalize(locale).unwrap_or(FALLBACK);
    rust_i18n::set_locale(chosen);
}

/// Current locale.
pub fn current() -> String {
    rust_i18n::locale().to_string()
}

/// Picks a locale from the environment, honouring an explicit preference.
///
/// `preferred` comes from the configuration file; the environment overrides it
/// only through `HYPRDMC_LANG`, so that a saved preference is not silently
/// undone by a system-wide `LANG`.
pub fn detect(preferred: Option<&str>) -> &'static str {
    detect_with(preferred, |key| std::env::var(key).ok())
}

/// Same resolution, with the environment injected so it can be tested without
/// depending on the machine the tests run on.
fn detect_with<F>(preferred: Option<&str>, env: F) -> &'static str
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(locale) = env("HYPRDMC_LANG").as_deref().and_then(normalize) {
        return locale;
    }
    if let Some(locale) = preferred.and_then(normalize) {
        return locale;
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| env(key).as_deref().and_then(normalize))
        .unwrap_or(FALLBACK)
}

/// Reads the locale from the environment and applies it in one go.
pub fn init(preferred: Option<&str>) {
    rust_i18n::set_locale(detect(preferred));
}

/// Maps a POSIX locale string onto a shipped language.
///
/// Accepts the shapes found in the wild — `fr`, `fr_FR`, `fr_FR.UTF-8`,
/// `fr-CA` — and matches on the language subtag alone: a Québécois user gets
/// French rather than English.
fn normalize(raw: &str) -> Option<&'static str> {
    let tag = raw
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('-', "_");
    let language = tag.split('_').next().unwrap_or_default().to_lowercase();
    if language.is_empty() {
        return None;
    }
    AVAILABLE.iter().copied().find(|l| *l == language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_language_tags_are_recognised() {
        assert_eq!(normalize("fr"), Some("fr"));
        assert_eq!(normalize("en"), Some("en"));
    }

    #[test]
    fn posix_locales_are_reduced_to_their_language() {
        assert_eq!(normalize("fr_FR.UTF-8"), Some("fr"));
        assert_eq!(normalize("fr_BE@euro"), Some("fr"));
        assert_eq!(normalize("en_GB.utf8"), Some("en"));
        // A regional variant we do not ship still resolves to its language.
        assert_eq!(normalize("fr-CA"), Some("fr"));
    }

    #[test]
    fn unshipped_and_empty_locales_are_rejected() {
        assert_eq!(normalize("de_DE.UTF-8"), None);
        assert_eq!(normalize("C"), None);
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
    }

    /// Builds a fake environment from `(key, value)` pairs.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn posix_c_locale_falls_back_to_english() {
        // `LANG=C` is the scripting default: English is the right answer.
        assert_eq!(detect_with(None, env(&[("LANG", "C")])), "en");
        assert_eq!(detect_with(None, env(&[])), "en");
    }

    #[test]
    fn system_locale_is_honoured() {
        assert_eq!(detect_with(None, env(&[("LANG", "fr_FR.UTF-8")])), "fr");
        // LC_ALL outranks LANG, as POSIX prescribes.
        assert_eq!(
            detect_with(None, env(&[("LC_ALL", "en_US"), ("LANG", "fr_FR")])),
            "en"
        );
    }

    #[test]
    fn configured_preference_outranks_the_system_locale() {
        // A preference saved in config.toml must not be undone by a
        // system-wide LANG.
        assert_eq!(
            detect_with(Some("fr"), env(&[("LANG", "en_US.UTF-8")])),
            "fr"
        );
        // An unshipped preference does not shadow the rest of the chain.
        assert_eq!(detect_with(Some("de"), env(&[("LANG", "fr_FR")])), "fr");
    }

    #[test]
    fn explicit_override_beats_everything() {
        assert_eq!(
            detect_with(
                Some("fr"),
                env(&[("HYPRDMC_LANG", "en"), ("LANG", "fr_FR")])
            ),
            "en"
        );
    }
}
