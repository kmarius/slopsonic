use library::ReadCancellation;
use proptest::prelude::*;

use super::support::fixture;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]
    #[test]
    fn playlist_occurrence_projection_preserves_arbitrary_duplicates(
        selected in prop::collection::vec(0usize..4, 1..96),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("property runtime");
        let (expected, projection) = runtime.block_on(async {
            let fixture = fixture().await;
            let expected = selected
                .iter()
                .map(|index| fixture.track_uris[*index].clone())
                .collect::<Vec<_>>();
            let playlist = super::support::server_playlist(&fixture.database, "Property Playlist", &expected).await.0;
            let projection = fixture
                .database
                .selected_track_uris(&library::MemberRead::new(library::Collection::Playlist(playlist)).selection(),None, &ReadCancellation::new())
                .await
                .expect("property Playlist projection");
            (expected, projection)
        });

        prop_assert_eq!(&projection, &expected);
        prop_assert_eq!(projection.len(), selected.len());
    }
}
