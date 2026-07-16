use std::num::NonZeroU64;

use crossbeam_channel::bounded;
use media_core::{MediaDuration, MediaTime};

use super::test_support::install_fake_media;
use super::*;
use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, MediaInstanceId, TimelineSeekKind,
    TimelineSeekRequestId,
};

fn instance(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("non-zero instance"))
}

fn request(
    id: u64,
    media_instance_id: MediaInstanceId,
    target: MediaTime,
    kind: TimelineSeekKind,
) -> ExactTimelineSeekRequest {
    ExactTimelineSeekRequest {
        request_id: TimelineSeekRequestId::new(NonZeroU64::new(id).expect("non-zero request")),
        media_instance_id,
        target,
        kind,
    }
}

fn exact_session(media_instance_id: MediaInstanceId) -> PlayerSession {
    let mut session = PlayerSession::default();
    install_fake_media(&mut session, Vec::new());
    session.snapshot.media_instance_id = Some(media_instance_id);
    session.snapshot.timeline.duration = Some(MediaDuration::from_duration(
        std::time::Duration::from_secs(10),
    ));
    session
}

#[test]
fn stale_instance_is_terminal_without_starting_seek() {
    let mut session = exact_session(instance(1));
    let (tx, rx) = bounded(1);
    session.begin_exact_timeline_seek(
        request(
            1,
            instance(2),
            MediaTime::ZERO,
            TimelineSeekKind::SetPosition,
        ),
        tx,
    );
    assert!(matches!(
        rx.recv().expect("terminal outcome"),
        ExactTimelineSeekOutcome::StaleInstance { .. }
    ));
    assert!(!session.snapshot.timeline.seeking);
}

#[test]
fn strict_beyond_end_distinguishes_set_position_and_relative_seek() {
    let media_instance_id = instance(3);
    let mut session = exact_session(media_instance_id);
    for (id, kind, expected_relative) in [
        (2, TimelineSeekKind::SetPosition, false),
        (3, TimelineSeekKind::Relative, true),
    ] {
        let (tx, rx) = bounded(1);
        session.begin_exact_timeline_seek(
            request(id, media_instance_id, MediaTime::from_secs(11), kind),
            tx,
        );
        let outcome = rx.recv().expect("terminal range outcome");
        assert_eq!(
            matches!(outcome, ExactTimelineSeekOutcome::BeyondEnd { .. }),
            expected_relative
        );
        assert_eq!(
            matches!(outcome, ExactTimelineSeekOutcome::InvalidRange { .. }),
            !expected_relative
        );
    }
}

#[test]
fn equal_end_is_accepted_and_applied_only_after_matching_commit() {
    let media_instance_id = instance(4);
    let mut session = exact_session(media_instance_id);
    let target = MediaTime::from_secs(10);
    let (tx, rx) = bounded(1);
    session.begin_exact_timeline_seek(
        request(4, media_instance_id, target, TimelineSeekKind::SetPosition),
        tx,
    );
    assert!(rx.try_recv().is_err(), "enqueue must not publish Applied");
    session.finish_exact_timeline_seek(target);
    assert!(matches!(
        rx.recv().expect("applied outcome"),
        ExactTimelineSeekOutcome::Applied { position, .. } if position == target
    ));
}

#[test]
fn overlapping_seek_supersedes_old_receipt_without_false_applied() {
    let media_instance_id = instance(5);
    let mut session = exact_session(media_instance_id);
    let (old_tx, old_rx) = bounded(1);
    session.begin_exact_timeline_seek(
        request(
            5,
            media_instance_id,
            MediaTime::from_secs(2),
            TimelineSeekKind::Relative,
        ),
        old_tx,
    );
    let (new_tx, new_rx) = bounded(1);
    session.begin_exact_timeline_seek(
        request(
            6,
            media_instance_id,
            MediaTime::from_secs(3),
            TimelineSeekKind::Relative,
        ),
        new_tx,
    );
    assert!(matches!(
        old_rx.recv().expect("old terminal outcome"),
        ExactTimelineSeekOutcome::Failed { .. }
    ));
    session.finish_exact_timeline_seek(MediaTime::from_secs(3));
    assert!(matches!(
        new_rx.recv().expect("new applied outcome"),
        ExactTimelineSeekOutcome::Applied { request_id, .. } if request_id.get() == 6
    ));
}
