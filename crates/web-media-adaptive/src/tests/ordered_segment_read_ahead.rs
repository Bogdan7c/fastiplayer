//! Selected-only read-ahead regression для blocking ordered adapter-а.

use demux_api::OrderedSegmentSource;

use super::*;

/// Активация заполняет caller-owned successor FIFO до предела, не осушает весь
/// snapshot и продолжает выдавать sequence строго по порядку.
#[test]
fn activation_fills_bounded_successor_fifo_without_eagerly_draining_snapshot() {
    let server = LocalServer::start(|request_index, _request| {
        let response_body = match request_index {
            0 => b"first-segment".as_slice(),
            1 => b"second-segment".as_slice(),
            2 => b"third-segment".as_slice(),
            3 => b"fourth-segment".as_slice(),
            _ => panic!("read-ahead вышел за bounded four-segment snapshot"),
        };
        response("200 OK", &[], response_body)
    });
    let first_target = server.target("/segment-1");
    let second_target = server.target("/segment-2");
    let third_target = server.target("/segment-3");
    let fourth_target = server.target("/segment-4");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &first_target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    let descriptors = vec![
        segment_descriptor(1, first_target),
        segment_descriptor(2, second_target),
        segment_descriptor(3, third_target),
        segment_descriptor(4, fourth_target),
    ];
    source
        .install_snapshot(
            AdaptiveSegmentSnapshot::new(
                SourceGeneration::new(1),
                AdaptivePresentation::Vod { duration: None },
                clock(1_000, 0),
                descriptors,
                AdaptiveSegmentCompletion::EndAfterSnapshot,
            )
            .expect("valid four-segment snapshot"),
        )
        .expect("snapshot installation");

    let activatable = BlockingOrderedSegmentAdapter::new_activatable(
        source,
        NonZeroUsize::new(2).expect("two buffered segments"),
    );
    let read_ahead = activatable.read_ahead_handle();
    let mut adapter = activatable.into_adapter();
    let cancellation = CancellationToken::new();

    let first = adapter
        .next_segment(&cancellation)
        .expect("first read succeeds")
        .expect("first segment exists");
    assert_eq!(first.bytes.as_ref(), b"first-segment");
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        server.request_count(),
        1,
        "suspended catalog probe не должен читать второй segment"
    );

    read_ahead.activate().expect("read-ahead activation");
    wait_for_request_count(&server, 3);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        server.request_count(),
        3,
        "двухсегментный FIFO не должен заранее осушать весь snapshot"
    );

    read_ahead.suspend().expect("read-ahead suspension");
    let second = adapter
        .next_segment(&cancellation)
        .expect("second read succeeds")
        .expect("second segment exists");
    assert_eq!(second.bytes.as_ref(), b"second-segment");
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        server.request_count(),
        3,
        "suspended pump не должен восполнять освобождённый FIFO slot"
    );
    read_ahead.activate().expect("read-ahead resume");
    wait_for_request_count(&server, 4);

    let third = adapter
        .next_segment(&cancellation)
        .expect("third read succeeds")
        .expect("third segment exists");
    assert_eq!(third.bytes.as_ref(), b"third-segment");
    let fourth = adapter
        .next_segment(&cancellation)
        .expect("fourth read succeeds")
        .expect("fourth segment exists");
    assert_eq!(fourth.bytes.as_ref(), b"fourth-segment");
    assert!(
        adapter
            .next_segment(&cancellation)
            .expect("terminal read succeeds")
            .is_none(),
        "snapshot должен завершиться typed EOF после четвёртого segment-а"
    );
}

/// Cancellation до активации остаётся terminal и не создаёт network side effect.
#[test]
fn activation_observes_cancellation_before_first_network_request() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"unexpected"));
    let target = server.target("/cancelled-segment");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        cancellation.clone(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            segment_descriptor(1, target),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot installation");
    let activatable = BlockingOrderedSegmentAdapter::new_activatable(
        source,
        NonZeroUsize::new(2).expect("two buffered segments"),
    );
    let read_ahead = activatable.read_ahead_handle();

    cancellation.cancel();
    assert!(matches!(
        read_ahead.activate_and_wait_for_ready_segment(&cancellation),
        Err(demux_api::OrderedSegmentReadError::Cancelled)
    ));
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        server.request_count(),
        0,
        "cancelled activation не должна отправлять HTTP request"
    );
}

/// Ошибка successor-а не отменяет уже доставленный segment и возвращается
/// вызывающему как прежний typed ordered-source failure на следующем read-е.
#[test]
fn successor_failure_is_reported_after_already_delivered_segment() {
    let server = LocalServer::start(|request_index, _| match request_index {
        0 => response("200 OK", &[], b"delivered-segment"),
        1 => response("404 Not Found", &[], b"missing-successor"),
        _ => panic!("failure regression должен выполнить ровно два request-а"),
    });
    let first_target = server.target("/available");
    let missing_target = server.target("/missing");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &first_target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(
            AdaptiveSegmentSnapshot::new(
                SourceGeneration::new(1),
                AdaptivePresentation::Vod { duration: None },
                clock(1_000, 0),
                vec![
                    segment_descriptor(1, first_target),
                    segment_descriptor(2, missing_target),
                ],
                AdaptiveSegmentCompletion::EndAfterSnapshot,
            )
            .expect("valid failure snapshot"),
        )
        .expect("snapshot installation");
    let activatable = BlockingOrderedSegmentAdapter::new_activatable(
        source,
        NonZeroUsize::new(2).expect("two buffered segments"),
    );
    let read_ahead = activatable.read_ahead_handle();
    let mut adapter = activatable.into_adapter();
    let cancellation = CancellationToken::new();

    let delivered = adapter
        .next_segment(&cancellation)
        .expect("first read succeeds")
        .expect("first segment exists");
    assert_eq!(delivered.bytes.as_ref(), b"delivered-segment");
    read_ahead
        .activate()
        .expect("read-ahead scheduling succeeds");
    let successor_error = adapter
        .next_segment(&cancellation)
        .expect_err("missing successor must remain a typed read error");
    assert!(matches!(
        successor_error,
        demux_api::OrderedSegmentReadError::Failed { .. }
    ));
    assert_eq!(server.request_count(), 2);
}

/// Fetch failure не является delivery receipt: новая manifest generation имеет
/// право повторить тот же sequence и действительно доставить восстановленный segment.
#[test]
fn failed_fetch_does_not_skip_same_sequence_from_new_generation() {
    let server = LocalServer::start(|request_index, _| match request_index {
        0 => response("404 Not Found", &[], b"temporarily-missing"),
        1 => response("200 OK", &[], b"recovered-segment"),
        _ => panic!("recovery regression должен выполнить ровно два request-а"),
    });
    let target = server.target("/recoverable");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            segment_descriptor(1, target.clone()),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("initial snapshot installation");

    assert!(matches!(poll_segment(&mut source), SegmentPoll::Failed(_)));
    source
        .install_snapshot(snapshot(
            2,
            segment_descriptor(1, target),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("replacement snapshot retains failed sequence");

    let SegmentPoll::Segment(recovered) = poll_segment(&mut source) else {
        panic!("replacement generation должна повторно доставить failed sequence");
    };
    assert_eq!(recovered.sequence, OrderedSegmentSequence::new(1));
    assert_eq!(recovered.bytes.as_ref(), b"recovered-segment");
    assert_eq!(server.request_count(), 2);
}

/// Строит один media descriptor с очевидной sequence identity.
fn segment_descriptor(sequence: u64, target: HttpRequestTarget) -> AdaptiveSegmentDescriptor {
    AdaptiveSegmentDescriptor::full(
        OrderedSegmentSequence::new(sequence),
        OrderedSegmentKind::Media,
        OrderedSegmentDiscontinuity::Continuous,
        target,
    )
}

/// Ждёт только появления network request-а, не вызывая consumer poll.
fn wait_for_request_count(server: &LocalServer, expected_count: usize) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while server.request_count() < expected_count {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected_count} ordered segment requests"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
