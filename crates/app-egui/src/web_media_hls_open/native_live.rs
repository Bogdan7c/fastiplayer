//! Native HLS live composition поверх существующего S33 runtime.

use super::*;

/// Узкий native-live результат: timeline и seek остаются runtime-only attachments.
pub(crate) struct PreparedNativeHlsLive {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    pub(crate) timeline_port: DynamicMediaTimelinePort,
}

/// Открывает admitted top media/master live через общий receipted runtime.
pub(crate) fn prepare_native_hls_live(
    request: HlsLiveOpenRequest,
) -> Result<PreparedNativeHlsLive> {
    let opened = prepare_hls_live_receipted(request, hls_async_seek_limits())
        .context("native HLS live runtime open failed")?;
    finalize_native_hls_live(opened)
}

/// Открывает semantic-rematch-нутый fresh catalog selection без extractor material.
pub(crate) fn prepare_native_hls_catalog_live(
    request: HlsLiveOpenRequest,
    selection: web_media_hls::HlsCatalogReopenSelection,
) -> Result<PreparedNativeHlsLive> {
    let opened = prepare_hls_catalog_live_receipted(request, selection, hls_async_seek_limits())
        .context("native HLS live catalog runtime open failed")?;
    finalize_native_hls_live(opened)
}

/// Проверяет общие install-ready topology и receipted-seek invariants до strong barrier-а.
fn finalize_native_hls_live(
    opened: web_media_hls::HlsLiveOpenResult,
) -> Result<PreparedNativeHlsLive> {
    let seek_handle = opened
        .async_seek_handle()
        .ok_or_else(|| anyhow!("native HLS live runtime потерял receipted seek handle"))?;
    let initial_readiness = opened.initial_readiness();
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    wait_for_initial_hls_tracks(demuxer.as_mut(), &initial_readiness)
        .context("native HLS live не достиг install-ready track состояния")?;
    Ok(PreparedNativeHlsLive {
        demuxer,
        seek_port: Arc::new(HlsPreparedDemuxSeekPort {
            handle: seek_handle,
        }),
        timeline_port,
    })
}
