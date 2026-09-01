//! Native HLS VOD catalog/open composition поверх общего HLS runtime.

use super::*;

/// Узкий native-VOD результат без extractor catalog/subtitle lifecycle attachment-ов.
pub(crate) struct PreparedNativeHlsVod {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    pub(crate) initial_position: PreparedInitialPosition,
}

/// Native open сохраняет typed HLS failure для строго ограниченного extractor fallback-а.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PrepareNativeHlsVodError {
    #[error("native HLS VOD runtime open failed: {0}")]
    Open(#[source] web_media_hls::HlsVodOpenError),
    #[error("native HLS VOD runtime потерял receipted seek handle")]
    MissingSeekHandle,
    #[error("native HLS VOD runtime не достиг install-ready topology: {0}")]
    InitialTopology(#[source] anyhow::Error),
    #[error("native HLS VOD initial-position proof нарушил requested start contract: {0:?}")]
    InitialPositionProof(HlsInitialPositionProofTakeOutcome),
    #[error("native HLS VOD initial-position capability не совпал с start intent")]
    InitialPositionCapabilityMismatch,
}

impl PrepareNativeHlsVodError {}

/// Открывает уже admitted native HLS VOD через те же policy/bootstrap constants, что YtDlp HLS.
pub(crate) fn prepare_native_hls_vod(
    request: HlsVodOpenRequest,
    start: HlsVodStartIntent,
) -> std::result::Result<PreparedNativeHlsVod, PrepareNativeHlsVodError> {
    let generation = request.generation;
    let request = request.with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap);
    let opened = prepare_hls_vod_receipted_at_start(request, hls_async_seek_limits(), start)
        .map_err(PrepareNativeHlsVodError::Open)?;
    finalize_native_hls_vod(opened, generation)
}

/// Открывает exact fresh-catalog selection с тем же native landing contract-ом.
pub(crate) fn prepare_native_hls_catalog_vod(
    request: HlsVodOpenRequest,
    selection: web_media_hls::HlsCatalogReopenSelection,
    start: HlsVodStartIntent,
) -> std::result::Result<PreparedNativeHlsVod, PrepareNativeHlsVodError> {
    let generation = request.generation;
    let request = request.with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap);
    let opened = prepare_hls_catalog_vod_receipted_at_start(
        request,
        selection,
        hls_async_seek_limits(),
        start,
    )
    .map_err(PrepareNativeHlsVodError::Open)?;
    finalize_native_hls_vod(opened, generation)
}

/// Финализирует общий topology/seek/initial-position boundary обоих native open modes.
fn finalize_native_hls_vod(
    opened: HlsVodOpenResult,
    generation: SourceGeneration,
) -> std::result::Result<PreparedNativeHlsVod, PrepareNativeHlsVodError> {
    let seek_handle = opened
        .async_seek_handle()
        .ok_or(PrepareNativeHlsVodError::MissingSeekHandle)?;
    let start_disposition = opened.start_disposition();
    let initial_position_proof = opened.initial_position_proof();
    let initial_readiness = opened.initial_readiness();
    let mut demuxer = opened.into_demuxer();
    wait_for_initial_hls_tracks(demuxer.as_mut(), &initial_readiness)
        .map_err(PrepareNativeHlsVodError::InitialTopology)?;
    let initial_position = match (start_disposition, initial_position_proof) {
        (
            HlsVodStartDisposition::BeginningRequested,
            HlsInitialPositionProofCapability::NotRequested,
        ) => PreparedInitialPosition::Beginning,
        (
            HlsVodStartDisposition::RestoreRequested { .. },
            HlsInitialPositionProofCapability::Deferred(port),
        ) => match port.take_for_generation(generation) {
            HlsInitialPositionProofTakeOutcome::Ready(proof) => {
                let target_position = proof.target_position();
                let result = proof.demux_seek_result();
                let landing_policy = if result.actual_position >= target_position {
                    PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget
                } else {
                    PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget
                };
                PreparedInitialPosition::PositionedAt {
                    target_position,
                    result,
                    landing_policy,
                }
            }
            outcome => return Err(PrepareNativeHlsVodError::InitialPositionProof(outcome)),
        },
        (
            HlsVodStartDisposition::RestoreRejectedToBeginning {
                reason: HlsVodRestoreFallbackReason::CheckpointOutsideVod,
                ..
            },
            HlsInitialPositionProofCapability::NotRequested,
        ) => PreparedInitialPosition::Beginning,
        _ => return Err(PrepareNativeHlsVodError::InitialPositionCapabilityMismatch),
    };
    Ok(PreparedNativeHlsVod {
        demuxer,
        seek_port: Arc::new(HlsPreparedDemuxSeekPort {
            handle: seek_handle,
        }),
        initial_position,
    })
}

/// Строит capability-filtered HLS catalog из уже переданного fetched root manifest-а.
pub(crate) fn discover_native_hls_catalog(
    request: &HlsVodOpenRequest,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    presentation: HlsCatalogPresentation,
    provider_default_variant_index: Option<usize>,
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
) -> Result<HlsCatalogDiscoveryOutcome> {
    let mut capability_probe =
        crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe::new(
            system_capabilities.clone(),
            audio_capabilities,
        );
    discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: request,
            catalog_identity,
            presentation,
            provider_default_variant_index,
            policy: hls_catalog_policy()?.with_provider_default_audio(
                web_media_hls::HlsProviderDefaultAudioPolicy::AllowUnsupportedOmission,
            ),
        },
        &mut capability_probe,
    )
    .context("native HLS catalog discovery завершился ошибкой")
}

/// Переносит уже доказанный native HLS runtime через единственный player preparation boundary.
#[cfg(test)]
pub(crate) fn prepare_native_hls_player_media(
    safe_label: &str,
    prepared: PreparedNativeHlsVod,
) -> std::result::Result<player_core::PreparedMedia, player_core::PreparedInitialPositionError> {
    let result = crate::media_open::compose_prepared_web_media(
        safe_label,
        prepared.demuxer,
        crate::media_open::PreparedWebMediaAttachments {
            demux_seek: Some(
                crate::media_open::PreparedWebMediaSeekAttachment::AuthoritativePostTarget(
                    prepared.seek_port,
                ),
            ),
            initial_position: Some(prepared.initial_position),
            ..crate::media_open::PreparedWebMediaAttachments::default()
        },
    );
    match result {
        Ok(prepared_media) => Ok(prepared_media),
        Err(crate::media_open::PreparedWebMediaCompositionError::InitialPosition(error)) => {
            Err(error)
        }
        Err(crate::media_open::PreparedWebMediaCompositionError::TimelineMode(_)) => {
            unreachable!("native HLS compatibility fixture has no dynamic timeline")
        }
    }
}
