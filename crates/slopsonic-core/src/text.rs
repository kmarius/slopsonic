//! Small helpers for building user-facing English strings.

pub fn count_text(count: u64, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

pub fn album_count_text(count: u64) -> String {
    count_text(count, "album", "albums")
}

pub fn track_count_text(count: u64) -> String {
    count_text(count, "track", "tracks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_choose_singular_or_plural() {
        assert_eq!(track_count_text(1), "1 track");
        assert_eq!(track_count_text(2), "2 tracks");
    }
}
