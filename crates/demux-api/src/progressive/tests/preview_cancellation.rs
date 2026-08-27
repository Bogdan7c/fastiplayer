// Causal Previewed → Receipted cancellation через production progressive worker boundary.

use super::*;

/// Preview блокируется только на своём token-е, а следующий receipted seek стартует сразу.
struct CancellablePreviewThenReceiptDemuxer {
    /// Test наблюдает вход worker-а в obsolete preview boundary.
    preview_started: SyncSender<()>,
    /// Test наблюдает вход того же worker-а в final receipted boundary.
    receipt_started: SyncSender<()>,
}

impl CancellablePreviewThenReceiptDemuxer {
    /// Возвращает exact result без скрытого изменения preview/receipt semantics.
    fn seek_result(request: DemuxSeekRequest) -> DemuxSeekResult {
        DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        }
    }
}

impl Demuxer for CancellablePreviewThenReceiptDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(Self::seek_result(DemuxSeekRequest::accurate(timestamp)))
    }

    fn seek_with_cancellable_preview_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.preview_started
            .send(())
            .expect("test preview observer должен жить");
        cancellation.wait_cancelled();
        let _obsolete_result = Self::seek_result(request);
        Err(MediaDemuxError::SeekCancelled.into())
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        _cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.receipt_started
            .send(())
            .expect("test receipt observer должен жить");
        Ok(Self::seek_result(request))
    }
}

#[test]
fn final_receipted_seek_cancels_in_flight_preview_without_manual_release() {
    let (preview_started_sender, preview_started_receiver) = sync_channel(1);
    let (receipt_started_sender, receipt_started_receiver) = sync_channel(1);
    let cancellation = CancellationToken::new();
    let mut progressive = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || {
            Ok(Box::new(CancellablePreviewThenReceiptDemuxer {
                preview_started: preview_started_sender,
                receipt_started: receipt_started_sender,
            }))
        },
        exact_seek_controller(),
        cancellation,
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(2).expect("test receipt bound ненулевой"),
        ),
    )
    .expect("deferred receipted worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("receipt capability опубликована");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks опубликованы"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let preview = progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(2)))
        .expect("preview публикует exact anchor без ожидания worker-а");
    assert_eq!(preview.actual_position, MediaTime::from_secs(2));
    assert_eq!(
        preview_started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(()),
        "worker должен войти в cancellable preview"
    );

    let final_fence = receipt_fence(7, 1);
    handle
        .enqueue(
            final_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(6)),
        )
        .expect("final receipt accepted");
    assert_eq!(
        receipt_started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(()),
        "final seek обязан стартовать после token cancellation без ручного release preview"
    );

    let receipt = poll_until_receipt(&handle);
    assert_eq!(receipt.fence, final_fence);
    assert_eq!(
        receipt.outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(6),
            actual_position: MediaTime::from_secs(6),
            actual_track_timestamp: None,
        })
    );
    assert_eq!(
        poll_until_event(&mut progressive).expect("current generation EOF доступен"),
        DemuxReadEvent::EndOfStream,
        "stale preview cancellation не должна публиковаться как current failure"
    );
}
