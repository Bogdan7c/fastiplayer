//! Публикация snapshot-а только при изменении и общий `Arc` у читателя (cpu-parity, сессия 05).

use super::*;
use crate::TrackSummarySnapshot;
use crate::worker::runtime_publish::SnapshotPublication;

/// Publisher + handle поверх одного канала, как в production-связке `spawn`.
fn publisher_with_handle() -> (LatestSnapshotPublisher, PlayerWorker) {
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let publisher = LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx.clone());
    let (mut worker, ..) =
        worker_with_present_handoff_for_tests(Arc::new(OrderedPresentFrameHandoff::new()));
    worker.snapshot_rx = snapshot_rx;
    worker.snapshot_publication_lock = Arc::clone(&publisher.publication_lock);
    (publisher, worker)
}

/// Снимок с узнаваемым содержимым: отличается от `empty()`.
fn snapshot_at_position(seconds: u64) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.set_timeline_position(MediaTime::from_duration(Duration::from_secs(seconds)));
    snapshot
}

/// N одинаковых snapshot-ов → в канал ушёл один, читатель держит один и тот же `Arc`.
#[test]
fn identical_snapshots_are_published_once_and_reader_keeps_the_same_arc() {
    let (mut publisher, mut worker) = publisher_with_handle();

    let first = publisher.publish(snapshot_at_position(5));
    let first_seen = worker.latest_snapshot();
    let repeats: Vec<_> = (0..10)
        .map(|_| publisher.publish(snapshot_at_position(5)))
        .collect();
    let after_repeats = worker.latest_snapshot();

    assert_eq!(first, SnapshotPublication::Published);
    assert!(repeats.iter().all(|p| *p == SnapshotPublication::Unchanged));
    assert!(
        Arc::ptr_eq(&first_seen, &after_repeats),
        "duplicates must not replace the Arc the UI already holds"
    );
    assert_eq!(first_seen.current_position, Duration::from_secs(5));
}

/// Любое видимое изменение даёт ровно одну новую публикацию с новым содержимым.
#[test]
fn every_visible_field_change_publishes_exactly_one_new_snapshot() {
    type Mutation = (&'static str, fn(&mut PlayerSnapshot));
    let mutations: [Mutation; 7] = [
        ("position", |s| {
            s.set_timeline_position(MediaTime::from_duration(Duration::from_secs(9)));
        }),
        ("title", |s| s.media_title = Some("other".to_owned())),
        ("source label", |s| s.source_label = Some("src".to_owned())),
        ("playback state", |s| {
            s.playback_state = PlaybackState::Paused
        }),
        ("volume", |s| s.volume = 0.25),
        ("tracks", |s| {
            s.tracks.push(TrackSummarySnapshot {
                id: TrackId::new(1),
                kind: TrackKind::Audio,
                codec_id: "opus".to_owned(),
                sample_rate: None,
                channels: None,
                duration: None,
                video: None,
                video_color_summary: None,
                labels: Default::default(),
            });
        }),
        ("queues", |s| s.queues.decoded_video_frames = 3),
    ];
    for (name, mutate) in mutations {
        let (mut publisher, mut worker) = publisher_with_handle();
        let baseline = snapshot_at_position(1);
        assert_eq!(
            publisher.publish(baseline.clone()),
            SnapshotPublication::Published
        );
        let before = worker.latest_snapshot();

        let mut changed = baseline.clone();
        mutate(&mut changed);
        assert_eq!(
            publisher.publish(changed.clone()),
            SnapshotPublication::Published,
            "{name}"
        );
        assert_eq!(
            publisher.publish(changed.clone()),
            SnapshotPublication::Unchanged,
            "{name}: repeat of the changed snapshot"
        );
        let after = worker.latest_snapshot();

        assert!(
            !Arc::ptr_eq(&before, &after),
            "{name}: reader must see a new Arc"
        );
        assert_eq!(*after, changed, "{name}: reader must see the new content");
    }
}

/// Снимок, равный начальному пустому кэшу читателя, публиковать не нужно:
/// читатель и так видит равное состояние (UI не «застревает»).
#[test]
fn first_snapshot_equal_to_initial_empty_cache_is_not_a_missed_publication() {
    let (mut publisher, mut worker) = publisher_with_handle();

    let outcome = publisher.publish(PlayerSnapshot::empty());

    assert_eq!(outcome, SnapshotPublication::Unchanged);
    assert_eq!(*worker.latest_snapshot(), PlayerSnapshot::empty());
    // Возврат к «пустому» после непустого — это изменение и обязан дойти до UI.
    assert_eq!(
        publisher.publish(snapshot_at_position(2)),
        SnapshotPublication::Published
    );
    assert_eq!(
        publisher.publish(PlayerSnapshot::empty()),
        SnapshotPublication::Published
    );
    assert_eq!(*worker.latest_snapshot(), PlayerSnapshot::empty());
}

/// Два чтения без новой публикации — один `Arc`; новая публикация — новый.
#[test]
fn reader_returns_same_arc_until_a_new_snapshot_is_published() {
    let (mut publisher, mut worker) = publisher_with_handle();
    let _ = publisher.publish(snapshot_at_position(3));

    let first = worker.latest_snapshot();
    let second = worker.latest_snapshot();
    assert!(Arc::ptr_eq(&first, &second));

    let _ = publisher.publish(snapshot_at_position(4));
    let third = worker.latest_snapshot();
    assert!(!Arc::ptr_eq(&second, &third));
    assert_eq!(third.current_position, Duration::from_secs(4));
    assert_eq!(
        second.current_position,
        Duration::from_secs(3),
        "old Arc stays intact"
    );
}

/// Latest wins: если читатель не успел, он видит только последний из изменённых.
#[test]
fn reader_sees_only_the_latest_of_several_unread_publications() {
    let (mut publisher, mut worker) = publisher_with_handle();
    for seconds in 1..=4 {
        assert_eq!(
            publisher.publish(snapshot_at_position(seconds)),
            SnapshotPublication::Published
        );
    }

    assert_eq!(
        worker.latest_snapshot().current_position,
        Duration::from_secs(4)
    );
}

/// Вертикаль через публикацию worker-а: тики без изменений состояния не кладут
/// новых snapshot-ов в канал, а изменение громкости кладёт ровно один.
#[test]
fn runtime_publishes_snapshot_only_when_session_state_changed() {
    let mut runtime = runtime_for_tests(Instant::now());
    let drain_rx = runtime
        .snapshot_publisher
        .snapshot_rx_for_drain_latest
        .clone();
    runtime.publish_session_outputs();
    let _ = drain_rx.try_iter().count();

    for _ in 0..5 {
        runtime.publish_session_outputs();
    }
    assert_eq!(
        drain_rx.try_iter().count(),
        0,
        "unchanged session must publish nothing"
    );

    runtime
        .session
        .dispatch_command(PlayerCommand::SetVolume(0.3))
        .unwrap();
    for _ in 0..5 {
        runtime.publish_session_outputs();
    }
    let published: Vec<_> = drain_rx.try_iter().collect();
    assert_eq!(
        published.len(),
        1,
        "one change must produce exactly one publication"
    );
    assert!((published[0].volume - 0.3).abs() < f32::EPSILON);
}
