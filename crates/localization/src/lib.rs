use std::env;

pub const DOMAIN: &str = "rufin";
const ENGLISH_LANGUAGE_PREFERENCE: &str = "en";
pub const SYSTEM_LANGUAGE_PREFERENCE: &str = "system";
pub const TRANSLATOR_CREDITS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/translator_credits.txt"));

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageOption {
    pub id: String,
    pub title: String,
}

/// No-op initialization (gettext removed).
pub fn initialize() -> Result<(), String> {
    Ok(())
}

pub fn default_language_preference() -> String {
    SYSTEM_LANGUAGE_PREFERENCE.to_string()
}

pub fn sanitize_language_preference(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("default")
        || value.eq_ignore_ascii_case(SYSTEM_LANGUAGE_PREFERENCE)
    {
        return default_language_preference();
    }
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@'))
    {
        return default_language_preference();
    }
    value.to_string()
}

pub fn effective_language_preference(saved_language_preference: &str) -> String {
    selected_language_preference(
        saved_language_preference,
        env::var("RUFIN_LANGUAGE").ok().as_deref(),
    )
}

pub fn process_language(saved_language_preference: &str) -> Option<String> {
    let language = effective_language_preference(saved_language_preference);
    (language != SYSTEM_LANGUAGE_PREFERENCE).then_some(language)
}

fn selected_language_preference(
    saved_language_preference: &str,
    environment_override: Option<&str>,
) -> String {
    sanitize_language_preference(environment_override.unwrap_or(saved_language_preference))
}

pub fn language_options() -> Vec<LanguageOption> {
    vec![
        LanguageOption {
            id: default_language_preference(),
            title: tr("System default"),
        },
        LanguageOption {
            id: ENGLISH_LANGUAGE_PREFERENCE.to_string(),
            title: "English".to_string(),
        },
    ]
}

pub fn language_options_for(_ids: impl IntoIterator<Item = String>) -> Vec<LanguageOption> {
    language_options()
}

pub fn language_option_index(options: &[LanguageOption], language_preference: &str) -> u32 {
    let language_preference = sanitize_language_preference(language_preference);
    let language_preference = if is_english_language(&language_preference) {
        ENGLISH_LANGUAGE_PREFERENCE
    } else {
        &language_preference
    };
    options
        .iter()
        .position(|option| option.id == language_preference)
        .unwrap_or_default() as u32
}

/// Returns the English message directly (no translation).
pub fn tr(message: &str) -> String {
    message.to_string()
}

/// Returns singular or plural English message based on count.
pub fn trn(singular: &str, plural: &str, count: u64) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        plural.to_string()
    }
}

pub fn tr_with(message: &str, args: &[(&str, &str)]) -> String {
    replace_placeholders(tr(message), args)
}

pub fn trn_with(singular: &str, plural: &str, count: u64, args: &[(&str, &str)]) -> String {
    replace_placeholders(trn(singular, plural, count), args)
}

pub fn album_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} album",
        "{count} albums",
        count,
        &[("count", &label)],
    )
}

pub fn result_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} result",
        "{count} results",
        count,
        &[("count", &label)],
    )
}

pub fn track_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} track",
        "{count} tracks",
        count,
        &[("count", &label)],
    )
}

pub const fn msgid(message: &'static str) -> &'static str {
    message
}

fn replace_placeholders(mut text: String, args: &[(&str, &str)]) -> String {
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

fn is_english_language(language_preference: &str) -> bool {
    let language_preference = language_preference.replace('-', "_");
    matches!(language_preference.as_str(), "C" | "POSIX" | "en")
        || language_preference.starts_with("C.")
        || language_preference.starts_with("en_")
        || language_preference.starts_with("en.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_preferences_keep_system_and_explicit_values() {
        assert_eq!(sanitize_language_preference("default"), "system");
        assert_eq!(sanitize_language_preference("de-DE.UTF-8"), "de-DE.UTF-8");
        assert_eq!(sanitize_language_preference("invalid/value"), "system");
        assert_eq!(selected_language_preference("system", None), "system");
        assert_eq!(selected_language_preference("et", None), "et");
        assert_eq!(selected_language_preference("et", Some("de")), "de");
        assert_eq!(
            selected_language_preference("et", Some("default")),
            "system"
        );
    }

    #[test]
    fn language_options_select_english_aliases_and_saved_catalogs() {
        let options = vec![
            LanguageOption {
                id: "system".into(),
                title: "System default".into(),
            },
            LanguageOption {
                id: "en".into(),
                title: "English".into(),
            },
            LanguageOption {
                id: "de_DE".into(),
                title: "Deutsch".into(),
            },
        ];
        assert_eq!(language_option_index(&options, "C"), 1);
        assert_eq!(language_option_index(&options, "de_DE"), 2);
        assert_eq!(language_option_index(&options, "missing"), 0);
    }

    #[test]
    fn placeholder_replacement_keeps_named_values() {
        assert_eq!(
            replace_placeholders("Found {count} items".to_string(), &[("count", "3")]),
            "Found 3 items"
        );
    }
}
