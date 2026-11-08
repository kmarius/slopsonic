use crate::{RepeatMode, RunId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReportPhase {
    Started,
    Progress,
    QualifiedPlay,
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceReportFact {
    pub transcoded: bool,
    pub run: RunId,
    pub session_identifier: String,
    pub queue_id: Option<u64>,
    pub queue_item_id: Option<u64>,
    pub media_uri: String,
    pub phase: SourceReportPhase,
    pub started_at_unix_seconds: i64,
    pub position_millis: u64,
    pub duration_millis: Option<u64>,
    pub paused: bool,
    pub muted: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat_mode: RepeatMode,
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEndReason {
    Completed,
    ManualSkip,
    Stopped,
    Replaced,
    Failed,
}

pub fn qualified_play_threshold_millis(duration_millis: u64) -> u64 {
    if duration_millis <= 10_000 {
        duration_millis
    } else if duration_millis < 60_000 {
        (duration_millis / 2).max(5_000)
    } else {
        (duration_millis / 2).clamp(30_000, 240_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_play_threshold_preserves_short_half_and_clamped_law() {
        assert_eq!(qualified_play_threshold_millis(9_500), 9_500);
        assert_eq!(qualified_play_threshold_millis(31_000), 15_500);
        assert_eq!(qualified_play_threshold_millis(10_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(20_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(180_000), 90_000);
        assert_eq!(qualified_play_threshold_millis(900_000), 240_000);
    }
}
