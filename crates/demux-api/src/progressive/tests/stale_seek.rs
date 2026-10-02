//! Устаревшие seek/read не останавливают worker раньше актуальной команды.

use super::*;

#[test]
fn manifest_reanchored_preview_publishes_fresh_target_generation() {
    let controller = ProgressiveSeekController::manifest_reanchored(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            // Моделируем старый observed index, который существенно отстаёт от drag target-а.
            actual_position: MediaTime::from_duration(
                request.timestamp.saturating_sub(Duration::from_secs(5)),
            ),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        || {
            Ok(Box::new(CommandSeekableDemuxer {
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("manifest-reanchored seekable worker starts");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial packet"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial EOF"),
        DemuxReadEvent::EndOfStream
    ));

    let target = Duration::from_secs(8);
    let preview = progressive
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("nonblocking observed preview accepted");
    assert_eq!(
        preview.actual_position.as_duration(),
        Duration::from_secs(3)
    );

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("fresh manifest target packet")
    else {
        panic!("fresh manifest target packet expected");
    };
    assert_eq!(packet.pts, target);
}

#[test]
fn stale_failing_seek_does_not_stop_worker_before_latest_command() {
    assert_stale_controlled_seek_does_not_stop_latest_command(ControlledFirstSeekOutcome::Failure);
}

#[test]
fn stale_mismatched_seek_does_not_stop_worker_before_latest_command() {
    assert_stale_controlled_seek_does_not_stop_latest_command(
        ControlledFirstSeekOutcome::MismatchedAnchor,
    );
}

fn assert_stale_controlled_seek_does_not_stop_latest_command(
    first_seek_outcome: ControlledFirstSeekOutcome,
) {
    let (seek_started_sender, seek_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || {
            Ok(Box::new(SlowControlledSeekDemuxer {
                first_seek_started: seek_started_sender,
                release_first_seek: release_receiver,
                first_seek_outcome,
                seek_count: 0,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial EOF"),
        DemuxReadEvent::EndOfStream
    ));

    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(8)))
        .expect("first preview");
    seek_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered first seek");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(2)))
        .expect("latest preview");
    release_sender.send(()).expect("release first seek");

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("latest packet after stale failure")
    else {
        panic!("latest seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(2));
}

#[test]
fn stale_read_failure_does_not_stop_worker_before_pending_seek() {
    let (read_started_sender, read_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || {
            Ok(Box::new(SupersededReadFailureDemuxer {
                read_started: read_started_sender,
                release_read: release_receiver,
                first_read: true,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    read_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered blocking read");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("pending seek preview");
    release_sender.send(()).expect("release stale read");

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("packet after stale read error")
    else {
        panic!("post-seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(3));
}
