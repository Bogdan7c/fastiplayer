//! Обёртка печати HLS-демуксера на настоящей очереди ресурсов: ошибка демуксера —
//! конец потока ТОЛЬКО если печать оборвала чтение движка; иначе всё как есть.

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use media_core::{
    DemuxDataEnd, DemuxReadEvent, DemuxSeekResult, DemuxSeekability, Demuxer, TrackInfo,
};
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestIds, RangeRequestNotifier, ResourceExtent,
    ResourceFetchClient, ResourceFetchError, ResourceFetchPolicy, ResourceFetchRequests,
    ResourceFetchSpec, ResourceFetchWriter, resource_fetch_queue,
};
use source_core::CancellationToken;

use super::SealAwareHlsDemuxer;

/// Демуксер, который на каждом чтении и перемотке отвечает ошибкой (как HLS после
/// «источник закрыт»), с известной длительностью.
struct AlwaysFailing;

impl Demuxer for AlwaysFailing {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(4))
    }
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Err(anyhow::anyhow!("hls-resource-fetch"))
    }
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("seek failed"))
    }
}

struct Silent;
impl RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

fn queue() -> (
    ResourceFetchWriter,
    ResourceFetchClient,
    ResourceFetchRequests,
) {
    let policy = ResourceFetchPolicy::new(
        NonZeroUsize::new(8).expect("non-zero"),
        Duration::from_secs(10),
        NonZeroU64::new(1 << 20).expect("non-zero"),
    )
    .expect("policy");
    resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent))
}

fn segment_spec() -> ResourceFetchSpec {
    ResourceFetchSpec {
        handle: ExternalResourceHandle::new(3),
        extent: ResourceExtent::Whole,
        max_bytes: NonZeroU64::new(1024).expect("non-zero"),
    }
}

#[test]
fn errors_pass_through_unless_the_seal_cut_a_read_and_then_end_the_stream() {
    let (writer, client, requests) = queue();
    let mut demuxer = SealAwareHlsDemuxer::new(Box::new(AlwaysFailing), requests);

    // Без печати: ошибка — настоящая, конец данных — настоящий.
    assert!(demuxer.next_event().is_err());
    assert_eq!(demuxer.data_end(), DemuxDataEnd::Complete);

    // Печать сама по себе ничего не маскирует.
    writer.seal();
    assert!(
        demuxer.next_event().is_err(),
        "seal alone does not mask errors"
    );
    assert_eq!(demuxer.data_end(), DemuxDataEnd::Complete);

    // Движок хочет следующий сегмент после печати — полученного не хватило.
    let refused = client.start(segment_spec(), &CancellationToken::new());
    assert!(matches!(refused, Err(ResourceFetchError::SourceClosed)));
    assert!(matches!(
        demuxer.next_event(),
        Ok(DemuxReadEvent::EndOfStream)
    ));
    // Позиция на конце не прыгнет в длительность: данные обрезаны.
    assert_eq!(
        demuxer.data_end(),
        DemuxDataEnd::CutShort { playable_end: None }
    );
    // Перемотка — чистый проброс: её ошибка не маскируется.
    assert!(demuxer.seek(Duration::ZERO).is_err());
}

/// Обёртка не трогает того, чем не владеет: дорожки, длительность, перематываемость,
/// ожидание данных — ответы настоящего демуксера.
#[test]
fn everything_but_the_end_is_the_inner_demuxer_answer() {
    let (_writer, _client, requests) = queue();
    let demuxer = SealAwareHlsDemuxer::new(Box::new(AlwaysFailing), requests);
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(4)));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    assert!(demuxer.tracks().is_empty());
    assert_eq!(demuxer.data_wait(), AlwaysFailing.data_wait());
}
