use super::*;

/// Sync preview без fence не создаёт receipt и сохраняет latest packet/capacity semantics.
#[test]
fn sync_preview_supersedes_pending_preview_without_staging_receipt() {
    let (read_started_sender, read_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let mut progressive = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || {
            Ok(Box::new(SupersededReadFailureDemuxer {
                read_started: read_started_sender,
                release_read: release_receiver,
                first_read: true,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        exact_seek_controller(),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(1).expect("test receipt bound ненулевой"),
        ),
    )
    .expect("receipted worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("receipt capability опубликована");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks опубликованы"),
        DemuxReadEvent::TracksChanged(_)
    ));
    read_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker удерживается внутри старого read");

    let first_preview = progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(8)))
        .expect("первый sync preview остаётся pending");
    assert_eq!(first_preview.actual_position, MediaTime::from_secs(8));
    let latest_preview = progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("latest sync preview supersede-ит pending preview");
    assert_eq!(latest_preview.actual_position, MediaTime::from_secs(3));
    assert_eq!(
        handle.poll_receipt(),
        None,
        "preview без fence не создаёт terminal receipt"
    );

    release_sender.send(()).expect("старый read освобождён");
    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("latest preview packet опубликован")
    else {
        panic!("latest preview обязан дойти до downstream packet boundary");
    };
    assert_eq!(packet.pts, Duration::from_secs(3));
    assert_eq!(
        handle.poll_receipt(),
        None,
        "скрытый receipt не опубликован"
    );

    let receipt_fence = receipt_fence(7, 1);
    handle
        .enqueue(
            receipt_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("preview supersede не занимает bounded receipt capacity");
    let receipt = poll_until_receipt(&handle);
    assert_eq!(receipt.fence, receipt_fence);
    assert_eq!(
        receipt.outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(5),
            actual_position: MediaTime::from_secs(5),
            actual_track_timestamp: None,
        })
    );
}

/// Sync preview обязан terminalize-ить ещё не начатый receipted seek и сохранить latest packet.
#[test]
fn sync_preview_supersedes_pending_receipt_and_releases_capacity() {
    let (read_started_sender, read_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let mut progressive = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || {
            Ok(Box::new(SupersededReadFailureDemuxer {
                read_started: read_started_sender,
                release_read: release_receiver,
                first_read: true,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        exact_seek_controller(),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(1).expect("test receipt bound ненулевой"),
        ),
    )
    .expect("receipted worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("receipt capability опубликована");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks опубликованы"),
        DemuxReadEvent::TracksChanged(_)
    ));
    read_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker удерживается внутри старого read");

    let superseded_fence = receipt_fence(7, 1);
    handle
        .enqueue(
            superseded_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(8)),
        )
        .expect("receipted seek остаётся pending, пока worker заблокирован");
    let reusable_fence = receipt_fence(7, 2);
    assert_eq!(
        handle
            .enqueue(
                reusable_fence,
                DemuxSeekRequest::accurate(Duration::from_secs(5)),
            )
            .expect_err("undrained pending receipt удерживает capacity"),
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull
    );

    let preview = progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("sync preview supersede-ит pending receipted seek");
    assert_eq!(preview.actual_position, MediaTime::from_secs(3));
    assert_eq!(
        handle
            .enqueue(
                reusable_fence,
                DemuxSeekRequest::accurate(Duration::from_secs(5)),
            )
            .expect_err("staged terminal receipt всё ещё удерживает capacity"),
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull
    );
    assert_eq!(
        handle.poll_receipt(),
        None,
        "terminal outcome публикует worker"
    );
    release_sender.send(()).expect("старый read освобождён");

    let superseded_receipt = poll_until_receipt(&handle);
    assert_eq!(superseded_receipt.fence, superseded_fence);
    assert_eq!(
        superseded_receipt.outcome,
        ProgressiveAsyncSeekOutcome::Superseded
    );
    assert_eq!(
        handle.poll_receipt(),
        None,
        "receipt публикуется ровно один раз"
    );

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("latest preview packet опубликован")
    else {
        panic!("latest preview обязан дойти до downstream packet boundary");
    };
    assert_eq!(packet.pts, Duration::from_secs(3));

    handle
        .enqueue(
            reusable_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("poll terminal receipt освобождает exact capacity");
    let reusable_receipt = poll_until_receipt(&handle);
    assert_eq!(reusable_receipt.fence, reusable_fence);
    assert_eq!(
        reusable_receipt.outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(5),
            actual_position: MediaTime::from_secs(5),
            actual_track_timestamp: None,
        })
    );
}
