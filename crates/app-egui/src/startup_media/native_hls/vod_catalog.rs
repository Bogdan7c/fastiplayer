//! Native HLS VOD root handoff, neutral catalog и semantic reopen orchestration.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// Fresh root/catalog generation не выводится из URL и монотонна в процессе.
static NEXT_NATIVE_HLS_SNAPSHOT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Готовит native media/master runtime без создания второго HLS data plane.
pub(super) fn prepare_native_hls_attempt(
    request: NativeHlsPreparationRequest<'_>,
) -> Result<NativeHlsAttempt<PreparedNativeHlsMedia>> {
    if request.cancellation.is_cancelled() {
        return Err(anyhow!("native HLS admission cancelled"));
    }

    // Fresh snapshot identity создаётся до I/O и остаётся общей для root, catalog и selection.
    let snapshot_identity = fresh_native_hls_snapshot_identity(request.source)?;
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(request.network_config)?;
    let source_config = SourceRuntimeConfig::from_network_config(request.network_config)
        .context("native HLS source config")?;
    let vod_endpoint_recovery = crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new();
    let transport_request = native_transport_request(
        &snapshot_identity.parent,
        request.source,
        generation,
        request.cancellation.clone(),
        vod_endpoint_recovery.observer(),
    )?;
    let http = AdaptiveHttpContext::new(
        transport_request,
        &source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("native HLS retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )?;

    // Root читается ровно один раз и затем передаётся parser/catalog/runtime как fetched manifest.
    let top_fetch = NativeTopManifestFetchIntent::new(request.source.target().clone());
    let fetched_top = match http.fetch_resource_blocking(
        top_fetch.request(generation, adaptive_limits.maximum_manifest_bytes),
    ) {
        Ok(resource) => resource,
        Err(error) if matches!(error.http_status_code(), Some(401 | 403)) => {
            return Ok(NativeHlsAttempt::RequiresYtDlpFallback(
                NativeHlsFallbackReason::AuthorizationRequired,
            ));
        }
        Err(AdaptiveTransportError::Cancelled) => {
            return Err(anyhow!("native HLS top manifest fetch cancelled"));
        }
        Err(error) => return Err(error).context("native HLS top manifest fetch"),
    };
    let selection_policy = NativeHlsSelectionPolicy::new(
        crate::web_media_quality::preferred_height_policy(
            request.web_media_config.preferred_video_height,
        ),
        request
            .preferred_video_codec_order
            .iter()
            .copied()
            .map(native_codec_family)
            .collect(),
    )
    .context("native HLS selection policy")?;
    let provider_default = match admit_native_hls_vod_catalog(
        fetched_top.bytes(),
        fetched_top.final_target(),
        HlsParserLimits::default(),
        &selection_policy,
    ) {
        Ok(selection) => selection,
        Err(NativeHlsAdmissionError::StrictlyNotHls) => {
            return Ok(NativeHlsAttempt::RequiresYtDlpFallback(
                NativeHlsFallbackReason::StrictlyNotHls,
            ));
        }
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired) => {
            return Ok(NativeHlsAttempt::RequiresYtDlpFallback(
                NativeHlsFallbackReason::ExtractorMaterialRequired,
            ));
        }
        Err(NativeHlsAdmissionError::LiveRequiresExtractor) => {
            return Ok(NativeHlsAttempt::RequiresYtDlpFallback(
                NativeHlsFallbackReason::LiveOrEventPlaylist,
            ));
        }
        Err(error @ (NativeHlsAdmissionError::Parse(_) | NativeHlsAdmissionError::Profile(_))) => {
            return Err(error).context("native HLS admission rejected malformed/profile input");
        }
    };

    let demux_registry =
        native_hls_demux_registry(request.demux_config, adaptive_limits.maximum_segment_bytes)?;
    let hls_policy = crate::web_media_hls_open::hls_policy(adaptive_limits)?;
    let manifest = top_fetch.into_manifest(fetched_top, &http);
    let open_request = HlsVodOpenRequest {
        http,
        generation,
        manifest,
        selection: provider_default.runtime_intent(),
        overrides: HlsRequestOverrides::new(None),
        containers: provider_default.container_intent(),
        demux_registry,
        policy: hls_policy,
    };

    // Media playlist даёт один neutral parent; master публикует полный proven component catalog.
    let discovered = crate::web_media_hls_open::discover_native_hls_vod_catalog(
        &open_request,
        snapshot_identity.catalog,
        provider_default.current_master_variant_index(),
        request.system_capabilities,
        request.audio_capabilities,
    )?;
    let (prepared, neutral_selection, component_catalog) = match discovered {
        HlsCatalogDiscoveryOutcome::Unavailable => {
            let neutral_selection = match request.expected_selection {
                Some(expected) => expected
                    .rematch(
                        snapshot_identity.parent,
                        WebMediaSelectionRematchSource::Candidate,
                    )
                    .context("native HLS media selection semantic rematch failed")?,
                None => WebMediaSelection::candidate(snapshot_identity.parent),
            };
            let prepared = match settle_native_hls_runtime(
                crate::web_media_hls_open::prepare_native_hls_vod(open_request, request.start),
            )? {
                NativeHlsAttempt::Prepared(prepared) => prepared,
                NativeHlsAttempt::RequiresYtDlpFallback(reason) => {
                    return Ok(NativeHlsAttempt::RequiresYtDlpFallback(reason));
                }
            };
            (prepared, neutral_selection, None)
        }
        HlsCatalogDiscoveryOutcome::Installed(snapshot) => {
            let neutral_selection = match request.expected_selection {
                Some(expected) => expected
                    .rematch(
                        snapshot_identity.parent,
                        WebMediaSelectionRematchSource::ComponentCatalog(snapshot.catalog()),
                    )
                    .context("native HLS master selection semantic rematch failed")?,
                None => WebMediaSelection::with_components(
                    snapshot_identity.parent,
                    snapshot.provider_default_selection().clone(),
                )
                .context("native HLS provider default нарушил catalog parent identity")?,
            };
            let WebMediaSelectionShape::Components(component_selection) = neutral_selection.shape()
            else {
                return Err(anyhow!(
                    "native HLS master selection потерял component catalog shape"
                ));
            };
            let reopen = snapshot
                .reopen_exact(component_selection)
                .context("native HLS exact catalog reopen projection failed")?;
            let component_catalog = Arc::new(snapshot.catalog().clone());
            let prepared = match settle_native_hls_runtime(
                crate::web_media_hls_open::prepare_native_hls_catalog_vod(
                    open_request,
                    reopen,
                    request.start,
                ),
            )? {
                NativeHlsAttempt::Prepared(prepared) => prepared,
                NativeHlsAttempt::RequiresYtDlpFallback(reason) => {
                    return Ok(NativeHlsAttempt::RequiresYtDlpFallback(reason));
                }
            };
            (prepared, neutral_selection, Some(component_catalog))
        }
    };

    // Только окончательно выбранный runtime может публиковать late endpoint expiry.
    vod_endpoint_recovery.arm_after_candidate_finalization();
    let crate::web_media_hls_open::PreparedNativeHlsVod {
        demuxer,
        seek_port,
        initial_position,
    } = prepared;
    let demuxer = vod_endpoint_recovery.wrap_demuxer(demuxer);
    let seek_port = vod_endpoint_recovery
        .wrap_seek_port(Some(seek_port))
        .expect("native HLS всегда публикует receipted seek port");
    let source_state = NativeHlsSourceState::new(
        neutral_selection,
        component_catalog,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            request.web_media_config,
        ),
    )
    .context("native HLS neutral catalog projection failed")?;

    Ok(NativeHlsAttempt::Prepared(PreparedNativeHlsMedia {
        demuxer,
        seek_port,
        initial_position,
        source_state,
        vod_endpoint_recovery,
    }))
}

/// Сохраняет единственный typed live/event fallback и не маскирует runtime failures.
fn settle_native_hls_runtime(
    result: std::result::Result<
        crate::web_media_hls_open::PreparedNativeHlsVod,
        crate::web_media_hls_open::PrepareNativeHlsVodError,
    >,
) -> Result<NativeHlsAttempt<crate::web_media_hls_open::PreparedNativeHlsVod>> {
    match result {
        Ok(prepared) => Ok(NativeHlsAttempt::Prepared(prepared)),
        Err(error) if error.fallback_reason().is_some() => Ok(
            NativeHlsAttempt::RequiresYtDlpFallback(NativeHlsFallbackReason::LiveOrEventPlaylist),
        ),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "Native HLS VOD runtime preparation failed"
            );
            Err(error).context("native HLS VOD runtime preparation")
        }
    }
}

/// Fresh parent и component catalog получают одну generation и stable source lineage.
struct NativeHlsSnapshotIdentity {
    parent: ExactSelectionIdentity,
    catalog: ComponentVariantCatalogIdentity,
}

/// Создаёт exact snapshot identity без locator/hash material.
fn fresh_native_hls_snapshot_identity(source: &NativeHlsUrl) -> Result<NativeHlsSnapshotIdentity> {
    let generation = NEXT_NATIVE_HLS_SNAPSHOT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .max(1);
    let source_identity = source.source_identity();
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source_identity,
            ExtractionGeneration::new(generation),
            CandidateFormatIdentity::new("native-hls-vod")?,
        ),
        SemanticIdentity::new(source_identity, "native-hls-vod")?,
    )?;
    let catalog = ComponentVariantCatalogIdentity::new(
        parent.clone(),
        ComponentVariantCatalogGeneration::new(generation),
    );
    Ok(NativeHlsSnapshotIdentity { parent, catalog })
}
