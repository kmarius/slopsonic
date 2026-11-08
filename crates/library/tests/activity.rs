use library::{ListenDeliveryTarget, ListenWrite, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn scrobble_outbox_keeps_one_delivery_per_target() {
    let fixture = fixture().await;
    let listen = ListenWrite {
        play_id: "play-1".to_string(),
        title: "Stored Title".to_string(),
        artist: "Stored Artist".to_string(),
        album: "Stored Album".to_string(),
        duration_millis: 180_000,
        started_at: 1_700_000_000,
        listened_millis: 90_000,
    };
    let targets = (0..12)
        .map(|index| ListenDeliveryTarget {
            service: ["lastfm", "listenbrainz", "librefm"][index % 3].to_string(),
            account_id: format!("account-{index}"),
            next_attempt_at: Some(10),
        })
        .collect::<Vec<_>>();
    for _ in 0..2 {
        fixture
            .database
            .queue_scrobbles(&listen, &targets)
            .await
            .expect("queue scrobbles");
    }
    let cancel = ReadCancellation::new();
    let due = fixture
        .database
        .due_listen_deliveries(10, 100, &cancel)
        .await
        .expect("due deliveries");
    assert_eq!(due.len(), 12);
    assert_eq!(due[0].track_title, "Stored Title");
    assert_eq!(due[0].duration_millis, 180_000);
    assert_eq!(due[0].started_at, 1_700_000_000);
    assert!(
        fixture
            .database
            .complete_listen_delivery(due[0].outbox_key)
            .await
            .unwrap()
    );
    assert!(
        fixture
            .database
            .defer_listen_delivery(due[1].outbox_key, 20, Some("offline"))
            .await
            .unwrap()
    );
    assert_eq!(
        fixture
            .database
            .due_listen_deliveries(10, 100, &cancel)
            .await
            .unwrap()
            .len(),
        10
    );
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrobble_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        11
    );
}

#[tokio::test]
async fn plays_without_targets_store_nothing_and_bad_input_is_rejected() {
    let fixture = fixture().await;
    let listen = ListenWrite {
        play_id: "play".to_string(),
        title: "Title".to_string(),
        artist: "Artist".to_string(),
        album: String::new(),
        duration_millis: 1000,
        started_at: 1,
        listened_millis: 1000,
    };
    fixture
        .database
        .queue_scrobbles(&listen, &[])
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrobble_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    let anonymous = ListenWrite {
        play_id: String::new(),
        ..listen
    };
    assert!(
        fixture
            .database
            .queue_scrobbles(&anonymous, &[])
            .await
            .is_err()
    );
}
