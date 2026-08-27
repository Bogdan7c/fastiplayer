// Focused contract tests для additive cancellable-preview demux boundary.

use super::*;

/// Раздельные журналы делают mutation и semantic routing наблюдаемыми без transport fake-а.
#[derive(Default)]
struct RecordingSeekDemuxer {
    preview_compatible_requests: Vec<DemuxSeekRequest>,
    receipted_requests: Vec<DemuxSeekRequest>,
}

impl Demuxer for RecordingSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.preview_compatible_requests.push(request);
        Ok(exact_result(request))
    }

    fn seek_with_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.receipted_requests.push(request);
        Ok(exact_result(request))
    }
}

/// Строит deterministic exact result, не добавляя контейнерную семантику в boundary test.
fn exact_result(request: DemuxSeekRequest) -> DemuxSeekResult {
    DemuxSeekResult {
        requested_position: MediaTime::from_duration(request.timestamp),
        actual_position: MediaTime::from_duration(request.timestamp),
        actual_track_timestamp: None,
    }
}

#[test]
fn default_cancellable_preview_seek_delegates_to_preview_semantics() {
    let mut demuxer = RecordingSeekDemuxer::default();
    let request = DemuxSeekRequest::accurate(Duration::from_secs(4));

    let result = demuxer
        .seek_with_cancellable_preview_request(request, DemuxSeekCancellationToken::new())
        .expect("активный token должен сохранить обычную preview-compatible семантику");

    assert_eq!(result.requested_position, MediaTime::from_secs(4));
    assert_eq!(demuxer.preview_compatible_requests, vec![request]);
    assert!(demuxer.receipted_requests.is_empty());
}

#[test]
fn default_cancellable_preview_seek_rejects_cancelled_token_without_mutation() {
    let mut demuxer = RecordingSeekDemuxer::default();
    let cancellation = DemuxSeekCancellationToken::new();
    cancellation.cancel();

    let error = demuxer
        .seek_with_cancellable_preview_request(
            DemuxSeekRequest::accurate(Duration::from_secs(4)),
            cancellation,
        )
        .expect_err("cancelled preview token не должен входить в legacy seek");
    let demux_error = error
        .downcast_ref::<MediaDemuxError>()
        .expect("отмена должна оставаться typed MediaDemuxError");

    assert!(matches!(demux_error, MediaDemuxError::SeekCancelled));
    assert!(demuxer.preview_compatible_requests.is_empty());
    assert!(demuxer.receipted_requests.is_empty());
}

#[test]
fn cancellable_preview_preserves_semantic_split_from_receipted_seek() {
    let mut demuxer = RecordingSeekDemuxer::default();
    let request = DemuxSeekRequest::preview(Duration::from_secs(4));

    demuxer
        .seek_with_cancellable_preview_request(request, DemuxSeekCancellationToken::new())
        .expect("preview boundary должен сохранить обычную request semantics");

    assert_eq!(demuxer.preview_compatible_requests, vec![request]);
    assert!(demuxer.receipted_requests.is_empty());
}
