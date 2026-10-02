//! Readiness-порт: неблокирующее чтение, пробуждение, отмена и panic worker-а.

use super::*;

#[test]
fn blocked_inner_read_returns_readiness_without_blocking_player_owner() {
    let (sender, inner) = blocking_demuxer();
    let cancellation = CancellationToken::new();
    let mut progressive =
        ProgressiveDemuxer::new(inner, cancellation, limits(2, 1024), retry_hint())
            .expect("progressive worker starts");

    let started_at = Instant::now();
    let first = progressive.next_event().expect("readiness event");
    assert!(matches!(first, DemuxReadEvent::TemporarilyUnavailable(_)));
    assert!(started_at.elapsed() < Duration::from_millis(100));

    sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker receiver lives");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("terminal event"),
        DemuxReadEvent::EndOfStream
    ));
}

#[test]
fn deferred_open_failure_is_nonblocking_and_preserves_typed_error() {
    let (release_sender, release_receiver) = sync_channel(0);
    let mut progressive = ProgressiveDemuxer::new_deferred(
        move || {
            release_receiver
                .recv()
                .expect("test owner releases deferred failure");
            Err(anyhow::anyhow!("deferred-open-test-failure"))
        },
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts");
    let readiness = progressive.readiness_port();

    let started_at = Instant::now();
    assert!(matches!(
        progressive.next_event().expect("readiness event"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    assert!(started_at.elapsed() < Duration::from_millis(20));

    release_sender
        .send(())
        .expect("deferred worker still waits for release");
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::EventAvailable,
        "queued typed failure обязан иметь приоритет над worker terminal state"
    );
    let error = progressive
        .next_event()
        .expect_err("deferred worker publishes exact failure");
    assert_eq!(error.to_string(), "deferred-open-test-failure");
}

#[test]
fn readiness_port_wakes_on_tracks_changed_without_consuming_event() {
    let (sender, inner) = blocking_demuxer();
    let mut progressive = ProgressiveDemuxer::new(
        inner,
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("progressive worker starts");
    let readiness = progressive.readiness_port();
    let (outcome_sender, outcome_receiver) = sync_channel(1);
    let waiter = thread::spawn(move || {
        outcome_sender
            .send(readiness.wait_until(Instant::now() + Duration::from_secs(1)))
            .expect("publish readiness outcome");
    });

    sender
        .send(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
            Vec::new(),
            None,
        )))
        .expect("worker receiver lives");
    assert_eq!(
        outcome_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("tracks publication wakes readiness waiter"),
        ProgressiveDemuxReadiness::EventAvailable
    );
    waiter.join().expect("join readiness waiter");
    assert!(matches!(
        progressive
            .next_event()
            .expect("tracks event remains queued"),
        DemuxReadEvent::TracksChanged(_)
    ));

    sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker receiver lives until terminal event");
}

#[test]
fn readiness_port_cancellation_wakes_without_waiting_for_deadline() {
    let (sender, inner) = blocking_demuxer();
    let cancellation = CancellationToken::new();
    let progressive =
        ProgressiveDemuxer::new(inner, cancellation.clone(), limits(2, 1024), retry_hint())
            .expect("progressive worker starts");
    let readiness = progressive.readiness_port();
    let (outcome_sender, outcome_receiver) = sync_channel(1);
    let waiter = thread::spawn(move || {
        outcome_sender
            .send(readiness.wait_until(Instant::now() + Duration::from_secs(30)))
            .expect("publish cancellation readiness outcome");
    });

    cancellation.cancel();
    assert_eq!(
        outcome_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation future wakes Condvar waiter"),
        ProgressiveDemuxReadiness::Cancelled
    );
    waiter.join().expect("join cancelled readiness waiter");
    // Если worker уже заметил cancellation до blocking read-а, receiver законно закрыт;
    // иначе drop sender-а освобождает scripted recv без ложного terminal event-а.
    drop(sender);
}

#[test]
fn readiness_port_cancellation_waiter_saturation_fails_closed_without_deadlock() {
    let shared = Arc::new(ProgressiveSharedState::new(limits(1, 1)));
    let cancellation = CancellationToken::new();
    let mut registered_futures = Vec::new();
    let mut registered_wakers = Vec::new();

    // source-core contract ограничивает один token восемью уникальными waker-ами;
    // девятый poll обязан отменить token fail-closed вместо unbounded registry.
    for _ in 0..8 {
        let waker = Waker::from(Arc::new(ReadinessCountingWake {
            wake_count: AtomicUsize::new(0),
        }));
        let mut cancellation_future = Box::pin(cancellation.cancelled());
        let mut context = Context::from_waker(&waker);
        assert!(cancellation_future.as_mut().poll(&mut context).is_pending());
        registered_wakers.push(waker);
        registered_futures.push(cancellation_future);
    }
    let readiness = ProgressiveDemuxReadinessPort::new(shared, cancellation.clone());
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::Cancelled
    );
    assert!(cancellation.is_cancelled());
    drop(registered_futures);
    drop(registered_wakers);
}

#[test]
fn readiness_port_observes_worker_panic_as_terminal_without_fake_event() {
    let mut progressive = ProgressiveDemuxer::new_deferred(
        || -> anyhow::Result<Box<dyn Demuxer + Send>> {
            panic!("readiness-test-worker-panic");
        },
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts before its scripted panic");
    let readiness = progressive.readiness_port();

    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::WorkerStopped
    );
    let error = progressive
        .next_event()
        .expect_err("panic не должен превращаться в synthetic event");
    assert!(
        error
            .downcast_ref::<super::super::ProgressiveDemuxWorkerStoppedError>()
            .is_some()
    );
}

#[test]
fn readiness_port_ignores_stale_generation_and_preserves_queue_accounting() {
    let shared = Arc::new(ProgressiveSharedState::new(limits(2, 16)));
    let cancellation = CancellationToken::new();
    let packet_bytes = Bytes::from_static(&[1, 2, 3, 4]);
    assert_eq!(
        push_progressive_message(
            &shared,
            &cancellation,
            0,
            ProgressiveMessage::Event(DemuxReadEvent::Packet(Packet::new_unbounded(
                TrackId::new(1),
                TrackKind::Video,
                Duration::ZERO,
                None,
                true,
                packet_bytes.clone(),
            ))),
        ),
        ProgressivePushOutcome::Published
    );
    let readiness = ProgressiveDemuxReadinessPort::new(Arc::clone(&shared), cancellation.clone());
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::EventAvailable
    );
    {
        let queue = shared.lock_queue();
        assert_eq!(queue.messages.len(), 1, "port не потребляет queued event");
        assert_eq!(
            queue.queued_encoded_bytes,
            packet_bytes.len(),
            "port не меняет byte accounting"
        );
    }

    shared.lock_queue().current_generation = 1;
    assert_eq!(
        readiness.wait_until(Instant::now()),
        ProgressiveDemuxReadiness::DeadlineReached,
        "stale-only queue не является readiness нового player intent-а"
    );
    let queue = shared.lock_queue();
    assert_eq!(queue.messages.len(), 1);
    assert_eq!(queue.queued_encoded_bytes, packet_bytes.len());
}

#[test]
fn deferred_open_rejects_seekable_inner_instead_of_hiding_seekability() {
    let mut progressive = ProgressiveDemuxer::new_deferred(
        || Ok(Box::new(SeekableDeferredDemuxer)),
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts");

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match progressive.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                assert!(Instant::now() < deadline, "deferred rejection timed out");
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            Err(error) => {
                assert!(
                    error
                        .downcast_ref::<ProgressiveDemuxStartupError>()
                        .is_some_and(|source| {
                            matches!(source, ProgressiveDemuxStartupError::SeekableInput)
                        })
                );
                break;
            }
            Ok(other) => panic!("unexpected deferred event: {other:?}"),
        }
    }
}
