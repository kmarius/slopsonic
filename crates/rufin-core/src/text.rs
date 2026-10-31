//! Small helpers for building user-facing English strings.

/// Replaces each `{name}` placeholder in `template` with its value.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// Picks the singular or plural template by `count`, then fills placeholders.
pub fn fill_plural(singular: &str, plural: &str, count: u64, args: &[(&str, &str)]) -> String {
    fill(if count == 1 { singular } else { plural }, args)
}

fn count_text(count: u64, singular: &str, plural: &str) -> String {
    let label = count.to_string();
    fill_plural(singular, plural, count, &[("count", &label)])
}

pub fn album_count_text(count: u64) -> String {
    count_text(count, "{count} album", "{count} albums")
}

pub fn result_count_text(count: u64) -> String {
    count_text(count, "{count} result", "{count} results")
}

pub fn track_count_text(count: u64) -> String {
    count_text(count, "{count} track", "{count} tracks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_keep_named_values() {
        assert_eq!(fill("Found {count} items", &[("count", "3")]), "Found 3 items");
    }

    #[test]
    fn counts_choose_singular_or_plural() {
        assert_eq!(track_count_text(1), "1 track");
        assert_eq!(track_count_text(2), "2 tracks");
    }
}
