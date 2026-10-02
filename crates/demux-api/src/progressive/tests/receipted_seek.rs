//! Receipted async seek: идентичность, supersede, прерывание чтения и отмена.

use super::*;

#[test]
fn legacy_runtime_rejects_async_seek_capability_without_changing_legacy_contract() {
    let (_sender, receiver) = sync_channel(1);
    let progressive = ProgressiveDemuxer::new(
        Box::new(BlockingChannelDemuxer { receiver }),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("legacy progressive runtime starts");

    let error = progressive
        .enqueue_async_seek(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect_err("legacy runtime не должен притворяться receipt-capable");
    assert_eq!(error, ProgressiveAsyncSeekEnqueueError::CapabilityAbsent);
}

#[test]
fn receipted_seek_publishes_authoritative_result_exactly_once() {
    let seek_count = Arc::new(AtomicUsize::new(0));
    let ordinary_seek_count = Arc::new(AtomicUsize::new(0));
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::clone(&seek_count),
            ordinary_seek_count: Arc::clone(&ordinary_seek_count),
        }),
        CancellationToken::new(),
        2,
    );
    let fence = receipt_fence(7, 1);
    handle
        .enqueue(fence, DemuxSeekRequest::accurate(Duration::from_secs(5)))
        .expect("valid request accepted");

    let receipt = poll_until_receipt(&handle);
    assert_eq!(receipt.fence, fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("authoritative success receipt expected");
    };
    assert_eq!(
        result.actual_position,
        MediaTime::from_duration(Duration::from_secs(4))
    );
    assert_eq!(seek_count.load(Ordering::SeqCst), 1);
    assert_eq!(ordinary_seek_count.load(Ordering::SeqCst), 0);
    assert_eq!(handle.poll_receipt(), None, "receipt is at-most-once");
}

#[test]
fn stale_fence_is_receipted_without_touching_inner_parser() {
    let seek_count = Arc::new(AtomicUsize::new(0));
    let ordinary_seek_count = Arc::new(AtomicUsize::new(0));
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::clone(&seek_count),
            ordinary_seek_count: Arc::clone(&ordinary_seek_count),
        }),
        CancellationToken::new(),
        2,
    );
    let stale_fence = receipt_fence(6, 1);
    handle
        .enqueue(
            stale_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("stale request получает terminal receipt");

    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence: stale_fence,
            outcome: ProgressiveAsyncSeekOutcome::Stale,
        }
    );
    assert_eq!(seek_count.load(Ordering::SeqCst), 0);
    assert_eq!(ordinary_seek_count.load(Ordering::SeqCst), 0);
}

#[test]
fn receipt_bound_and_monotonic_identity_are_enforced_until_drain() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::new(AtomicUsize::new(0)),
            ordinary_seek_count: Arc::new(AtomicUsize::new(0)),
        }),
        CancellationToken::new(),
        1,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        handle
            .enqueue(
                receipt_fence(7, 1),
                DemuxSeekRequest::accurate(Duration::from_secs(2)),
            )
            .expect_err("identity must increase"),
        ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity
    );
    assert_eq!(
        handle
            .enqueue(
                receipt_fence(7, 2),
                DemuxSeekRequest::accurate(Duration::from_secs(2)),
            )
            .expect_err("undrained receipt retains capacity"),
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull
    );

    let first_receipt = poll_until_receipt(&handle);
    assert_eq!(
        first_receipt.fence.request_id,
        ProgressiveSeekRequestId::new(1)
    );
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("drain releases exact capacity");
    assert_eq!(
        poll_until_receipt(&handle).fence.request_id,
        ProgressiveSeekRequestId::new(2)
    );
}

#[test]
fn rapid_seek_supersedes_in_flight_and_pending_requests() {
    let (started_sender, started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let (_progressive, handle) = receipted_runtime(
        Box::new(SlowReceiptSeekDemuxer {
            first_seek_started: started_sender,
            release_first_seek: release_receiver,
            seek_count: 0,
        }),
        CancellationToken::new(),
        3,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker owns first blocking seek");
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("second request accepted");
    handle
        .enqueue(
            receipt_fence(7, 3),
            DemuxSeekRequest::accurate(Duration::from_secs(3)),
        )
        .expect("third request supersedes pending second");
    release_sender.send(()).expect("release first seek");

    let mut outcomes = [None; 3];
    for _ in 0..3 {
        let receipt = poll_until_receipt(&handle);
        let index =
            usize::try_from(receipt.fence.request_id.value() - 1).expect("small test identity");
        outcomes[index] = Some(receipt.outcome);
    }
    assert_eq!(
        outcomes,
        [
            Some(ProgressiveAsyncSeekOutcome::Superseded),
            Some(ProgressiveAsyncSeekOutcome::Superseded),
            Some(ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
                requested_position: MediaTime::from_duration(Duration::from_secs(3)),
                actual_position: MediaTime::from_duration(Duration::from_secs(3)),
                actual_track_timestamp: None,
            })),
        ]
    );
    assert_eq!(handle.poll_receipt(), None);
}

#[test]
fn newer_receipted_seek_physically_cancels_in_flight_worker_operation() {
    let (started_sender, started_receiver) = sync_channel(2);
    let (_progressive, handle) = receipted_runtime(
        Box::new(CancellableReceiptSeekDemuxer {
            seek_started: started_sender,
            seek_count: 0,
        }),
        CancellationToken::new(),
        2,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(1),
        "worker должен войти в первый cancellable seek"
    );

    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("newer request accepted");
    assert_eq!(
        started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(2),
        "второй seek должен стартовать без ручного release первого"
    );

    let first = poll_until_receipt(&handle);
    let second = poll_until_receipt(&handle);
    let outcomes = [
        (first.fence.request_id.value(), first.outcome),
        (second.fence.request_id.value(), second.outcome),
    ];
    assert!(outcomes.contains(&(1, ProgressiveAsyncSeekOutcome::Superseded)));
    assert!(outcomes.contains(&(
        2,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(Duration::from_secs(2)),
            actual_position: MediaTime::from_duration(Duration::from_secs(2)),
            actual_track_timestamp: None,
        })
    )));
}

#[test]
fn deferred_receipted_seek_interrupts_stalled_old_body_before_replacement_receipt() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(false);
    let progressive = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || Ok(Box::new(inner)),
        exact_seek_controller(),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(2).expect("test receipt bound ненулевой"),
        ),
    )
    .expect("deferred receipt worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("deferred runtime публикует receipt capability");

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted),
        "worker должен войти в stalled old body read"
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("current-runtime request accepted");
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::BodyDropped),
        "physical old body owner должен быть dropped после interruption"
    );
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReplacementStarted),
        "replacement нельзя начинать до whole-parser unwind"
    );
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        1
    );
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            actual_position,
            ..
        }) if actual_position == MediaTime::from_secs(5)
    ));
}

#[test]
fn failed_replacement_after_active_read_interruption_has_no_fake_success_and_restarts_old_source() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(true);
    let (mut progressive, handle) = receipted_runtime(Box::new(inner), CancellationToken::new(), 2);

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted)
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("current-runtime request accepted");
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::BodyDropped)
    );
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReplacementStarted)
    );
    assert_eq!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Failed,
        "ошибка replacement не имеет права публиковать fake success"
    );
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        1
    );

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("old committed source должен restart-нуться")
    else {
        panic!("rollback обязан вернуть packet старого committed source-а");
    };
    assert_eq!(
        packet.pts,
        Duration::ZERO,
        "failed target нельзя подменять новой timeline position"
    );
}

#[test]
fn stale_receipted_seek_does_not_interrupt_current_active_read() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(false);
    let (_progressive, handle) = receipted_runtime(Box::new(inner), CancellationToken::new(), 2);

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted)
    );
    let stale_fence = receipt_fence(6, 1);
    handle
        .enqueue(
            stale_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("stale request получает terminal receipt");
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        0,
        "stale fence не должен трогать current physical read"
    );

    let _ = interruption_controller
        .request_active_read_interruption(DemuxActiveReadInterruptionReason::ReceiptedSeekEnqueued);
    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence: stale_fence,
            outcome: ProgressiveAsyncSeekOutcome::Stale,
        }
    );
}

#[test]
fn failed_receipted_seek_does_not_kill_transactional_worker() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(FirstReceiptSeekFailsDemuxer { seek_count: 0 }),
        CancellationToken::new(),
        2,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Failed
    );
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("worker remains available after transactional error");
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(_)
    ));
}

#[test]
fn completed_replacement_survives_later_failed_request_and_accepts_retry() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(CompletedTokenReplacementDemuxer {
            committed_source_token: None,
            seek_count: 0,
        }),
        CancellationToken::new(),
        1,
    );

    for (request_id, target_seconds, expected_success) in
        [(1, 1, true), (2, 2, false), (3, 3, true)]
    {
        handle
            .enqueue(
                receipt_fence(7, request_id),
                DemuxSeekRequest::accurate(Duration::from_secs(target_seconds)),
            )
            .expect("worker должен принимать retry после terminal receipt");
        let receipt = poll_until_receipt(&handle);
        assert_eq!(receipt.fence.request_id.value(), request_id);
        assert_eq!(
            matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Succeeded(_)),
            expected_success,
            "scripted request обязан сохранить свой terminal outcome"
        );
    }
}

#[test]
fn async_seek_after_visible_eof_reopens_front_generation() {
    let (mut progressive, handle) = receipted_runtime(
        Box::new(CommandSeekableDemuxer {
            position: Duration::ZERO,
            packet_emitted: false,
        }),
        CancellationToken::new(),
        1,
    );

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial packet"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("visible EOF"),
        DemuxReadEvent::EndOfStream
    ));

    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(6)),
        )
        .expect("new generation accepted after EOF");
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(_)
    ));

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("post-seek packet after old EOF")
    else {
        panic!("новая generation должна снять только старый EOS latch");
    };
    assert_eq!(packet.pts, Duration::from_secs(6));
}

#[test]
fn cancellation_terminalizes_in_flight_receipted_seek() {
    let (started_sender, started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let cancellation = CancellationToken::new();
    let (_progressive, handle) = receipted_runtime(
        Box::new(SlowReceiptSeekDemuxer {
            first_seek_started: started_sender,
            release_first_seek: release_receiver,
            seek_count: 0,
        }),
        cancellation.clone(),
        1,
    );
    let fence = receipt_fence(7, 1);
    handle
        .enqueue(fence, DemuxSeekRequest::accurate(Duration::from_secs(1)))
        .expect("request accepted");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker owns blocking seek");
    cancellation.cancel();
    release_sender.send(()).expect("release cancelled seek");

    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence,
            outcome: ProgressiveAsyncSeekOutcome::Cancelled,
        }
    );
    assert_eq!(
        handle.poll_receipt(),
        None,
        "cancellation emits one receipt"
    );
}
