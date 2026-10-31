//! Shared playback fixtures for integration tests.

use std::sync::Arc;

use playback::{
    ControlsView, CurrentMedia, CurrentMediaId, PlaybackView, Provenance, QueueRow,
    QueueSummaryView, RepeatMode, RunId, TrackReference, TransportStatus, TransportView,
};

pub(crate) fn test_view(
    run: u64,
    album: &str,
    state: TransportStatus,
    position_millis: u64,
) -> PlaybackView {
    let media_uri =
        library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
    let current = CurrentMedia {
        stream: None,
        id: CurrentMediaId {
            run: Some(RunId::new(run)),
            media_uri,
        },
        row: Arc::new(QueueRow {
            occurrence_id: None,
            position: 0,
            received_from_connect: false,
            provenance: Provenance::Manual,
            track: {
                let reference = TrackReference {
                    media_uri: library::source_entity_uri(
                        &library::SourceId::new("source"),
                        "track",
                        "track",
                    ),
                    artwork_binding: None,
                    metadata: library::TrackMetadata {
                        audio_properties: Default::default(),
                        title: "Track".to_string(),
                        artist: "Artist".to_string(),
                        album: album.to_string(),
                        album_display_artist: Some("Album Artist".to_string()),
                        duration_millis: 42_500,
                        disc_number: 1,
                        track_number: 1,
                        year: Some(2026),
                        release_date: None,
                        source_format: None,
                        musicbrainz_recording_id: Some("recording-id".to_string()),
                        musicbrainz_release_track_id: Some("track-id".to_string()),
                        musicbrainz_album_id: Some("album-id".to_string()),
                        musicbrainz_release_group_id: Some("release-group-id".to_string()),
                        primary_artist_musicbrainz_id: Some("artist-id".to_string()),
                        ..Default::default()
                    },
                };
                library::TrackView {
                    track_key: library::TrackKey::from_raw(1),
                    source_key: None,
                    source_id: None,
                    object_id: None,
                    media_uri: reference.media_uri,
                    metadata: reference.metadata,
                    artwork_binding: reference.artwork_binding,
                    state: Default::default(),
                    access: Default::default(),
                    links: Default::default(),
                }
            },
        }),
    };
    PlaybackView {
        queue_loading: false,
        queue_window: Vec::new(),
        queue: QueueSummaryView {
            revision: run,
            total: 1,
            current_index: Some(0),
            next_index: None,
            can_next: false,
        },
        transport: TransportView {
            current: Some(current),
            state,
            desired_playing: matches!(
                state,
                TransportStatus::Resolving
                    | TransportStatus::Buffering
                    | TransportStatus::Playing
            ),
            position_millis,
            position_observed_at_millis: 0,
            playback_rate: 1.0,
            duration_millis: 42_500,
            can_seek: true,
            buffering_percent: None,
            error: None,
        },
        controls: ControlsView {
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            auto_dj_enabled: false,
            volume: 1.0,
            muted: false,
            audio_output: None,
        },
    }
}
