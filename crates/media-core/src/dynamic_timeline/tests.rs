//! Тесты табло шкалы: ревизии, публикация, переходы вида и изоляция старой пары при замене.

use crossbeam_channel::TryRecvError;

use super::*;

fn generation(value: u64) -> DynamicMediaTimelinePortGeneration {
    DynamicMediaTimelinePortGeneration::new(
        NonZeroU64::new(value).expect("test generation must be non-zero"),
    )
}

fn no_dvr_pair() -> (DynamicMediaTimelinePort, DynamicMediaTimelinePublisher) {
    dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(1),
        source_epoch: DynamicMediaTimelineEpoch::new(4),
        state: DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(50)).into(),
    })
}

#[test]
fn join_position_defaults_to_edge_and_cannot_pass_it() {
    let edge = MediaTime::from_secs(50);
    let state = DynamicMediaTimelineState::without_dvr(edge);
    assert_eq!(state.join_position(), edge);

    let held_back = state
        .with_join_position(MediaTime::from_secs(44))
        .expect("join before edge is valid");
    assert_eq!(held_back.join_position(), MediaTime::from_secs(44));
    // Hold-back не меняет ни edge, ни диапазоны.
    assert_eq!(held_back.live_edge(), edge);
    assert_eq!(held_back.seekable_range(), None);

    assert_eq!(
        state.with_join_position(MediaTime::from_secs(51)),
        Err(
            DynamicMediaTimelineValidationError::JoinPositionAfterLiveEdge {
                join_position: MediaTime::from_secs(51),
                live_edge: edge,
            }
        )
    );
}

#[test]
fn join_position_survives_publication_to_consumer_snapshot() {
    let (port, publisher) = no_dvr_pair();
    let state = DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(60))
        .with_join_position(MediaTime::from_secs(54))
        .expect("valid join");
    publisher
        .publish(DynamicMediaTimelineEpoch::new(4), state)
        .expect("same epoch publication");
    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .join_position(),
        MediaTime::from_secs(54)
    );
}

#[test]
fn no_dvr_and_non_zero_dvr_states_are_explicit() {
    let (port, publisher) = no_dvr_pair();
    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .availability_range(),
        None
    );
    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .seekable_range(),
        None
    );

    let dvr_range = TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(60))
        .expect("ordered DVR range");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(5),
            DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(61), dvr_range)
                .expect("valid DVR state"),
        )
        .expect("publication");

    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .seekable_range(),
        Some(dvr_range)
    );
    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .availability_range(),
        Some(dvr_range)
    );
}

#[test]
fn availability_and_packet_proof_are_distinct_and_nested() {
    let availability = TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(60))
        .expect("ordered availability");
    let proven = TimelineRange::new(MediaTime::from_secs(30), MediaTime::from_secs(50))
        .expect("ordered proof");
    let state = DynamicMediaTimelineState::with_available_and_seekable_dvr(
        MediaTime::from_secs(60),
        availability,
        proven,
    )
    .expect("nested proof");
    assert_eq!(state.availability_range(), Some(availability));
    assert_eq!(state.seekable_range(), Some(proven));

    let outside = TimelineRange::new(MediaTime::from_secs(10), MediaTime::from_secs(50))
        .expect("ordered outside proof");
    assert!(matches!(
        DynamicMediaTimelineState::with_available_and_seekable_dvr(
            MediaTime::from_secs(60),
            availability,
            outside,
        ),
        Err(DynamicMediaTimelineValidationError::SeekableRangeOutsideAvailability { .. })
    ));
}

#[test]
fn invalid_empty_or_future_dvr_window_is_rejected() {
    let empty_range = TimelineRange::new(MediaTime::from_secs(5), MediaTime::from_secs(5))
        .expect("equal bounds are a valid generic timeline range");
    assert!(matches!(
        DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(5), empty_range),
        Err(DynamicMediaTimelineValidationError::EmptyDvrRange { .. })
    ));

    let range = TimelineRange::new(MediaTime::from_secs(5), MediaTime::from_secs(15))
        .expect("ordered range");
    assert!(matches!(
        DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(14), range),
        Err(DynamicMediaTimelineValidationError::LiveEdgeBeforeDvrEnd { .. })
    ));
}

#[test]
fn observe_arm_recheck_closes_lost_wakeup_window() {
    let (port, publisher) = no_dvr_pair();
    let observed = port.observe();
    let activity_rx = port.activity_receiver();
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(5),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(51)),
        )
        .expect("publication in observe-arm window");

    let rechecked = port
        .recheck_after_arm(observed.revision())
        .expect("new revision must be visible before wait");
    assert_eq!(
        rechecked.state.as_live().expect("live view").live_edge(),
        MediaTime::from_secs(51)
    );
    activity_rx
        .try_recv()
        .expect("wake edge remains observable");
}

#[test]
fn burst_is_coalesced_but_latest_snapshot_is_not_lost() {
    let (port, publisher) = no_dvr_pair();
    let activity_rx = port.activity_receiver();
    for second in 51..=80 {
        publisher
            .publish(
                DynamicMediaTimelineEpoch::new(5),
                DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(second)),
            )
            .expect("burst publication");
    }

    activity_rx.try_recv().expect("one coalesced activity edge");
    assert_eq!(activity_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(
        port.observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .live_edge(),
        MediaTime::from_secs(80)
    );
}

#[test]
fn stale_epoch_and_disconnected_consumer_are_typed() {
    let (port, publisher) = no_dvr_pair();
    assert!(matches!(
        publisher.publish(
            DynamicMediaTimelineEpoch::new(3),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(52)),
        ),
        Err(DynamicMediaTimelinePublishError::StaleSourceEpoch { .. })
    ));

    drop(port);
    assert_eq!(
        publisher.publish(
            DynamicMediaTimelineEpoch::new(5),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(52)),
        ),
        Err(DynamicMediaTimelinePublishError::ConsumerDisconnected)
    );
}

#[test]
fn old_port_and_publisher_are_isolated_from_reopened_pair() {
    let (old_port, old_publisher) = no_dvr_pair();
    let (new_port, _new_publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(2),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state: DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(500)).into(),
    });

    old_publisher
        .publish(
            DynamicMediaTimelineEpoch::new(5),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(99)),
        )
        .expect("old pair remains internally valid");

    assert_eq!(
        old_port
            .observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .live_edge(),
        MediaTime::from_secs(99)
    );
    assert_eq!(
        new_port
            .observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .live_edge(),
        MediaTime::from_secs(500)
    );
}
