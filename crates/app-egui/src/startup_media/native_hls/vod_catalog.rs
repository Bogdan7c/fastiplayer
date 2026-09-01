//! Native HLS root handoff, neutral catalog и semantic VOD/live reopen orchestration.

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
    // Первый context существует только для authoritative root fetch до content-based presentation.
    let admission_transport_request = native_transport_request(
        &snapshot_identity.parent,
        request.source,
        MediaPresentation::Vod,
        generation,
        request.cancellation.clone(),
    )?;
    let admission_http = native_adaptive_http_context(
        admission_transport_request,
        request.network_config,
        adaptive_limits,
    )?;

    // Root читается ровно один раз и затем передаётся parser/catalog/runtime как fetched manifest.
    let top_fetch = NativeTopManifestFetchIntent::new(request.source.target().clone());
    let fetched_top = match admission_http.fetch_resource_blocking(
        top_fetch.request(generation, adaptive_limits.maximum_manifest_bytes),
    ) {
        Ok(resource) => resource,
        Err(error) if matches!(error.http_status_code(), Some(401 | 403)) => {
            return Ok(NativeHlsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
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
    let provider_default = match admit_native_hls_catalog(
        fetched_top.bytes(),
        fetched_top.final_target(),
        HlsParserLimits::default(),
        &selection_policy,
    ) {
        Ok(selection) => selection,
        Err(NativeHlsAdmissionError::StrictlyNotHls) => {
            return Ok(NativeHlsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ProviderDocument,
            ));
        }
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired) => {
            return Ok(NativeHlsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
            ));
        }
        Err(error @ (NativeHlsAdmissionError::Parse(_) | NativeHlsAdmissionError::Profile(_))) => {
            return Err(error).context("native HLS admission rejected malformed/profile input");
        }
    };

    let presentation = match provider_default.presentation_evidence() {
        web_media_hls::NativeHlsPresentationEvidence::TopMedia(presentation) => presentation,
        web_media_hls::NativeHlsPresentationEvidence::SelectedMasterChild => {
            web_media_hls::detect_hls_catalog_presentation(
                &fetched_top,
                &admission_http,
                generation,
                &provider_default.runtime_intent(),
                provider_default.current_master_variant_index(),
                HlsParserLimits::default(),
            )
            .context("native HLS selected child presentation detection failed")?
        }
    };
    let vod_endpoint_recovery = crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new();
    let transport_request = native_transport_request(
        &snapshot_identity.parent,
        request.source,
        match presentation {
            web_media_hls::HlsCatalogPresentation::Vod => MediaPresentation::Vod,
            web_media_hls::HlsCatalogPresentation::Live => MediaPresentation::Live,
        },
        generation,
        request.cancellation.clone(),
    )?;
    let transport_request = match presentation {
        web_media_hls::HlsCatalogPresentation::Vod => {
            transport_request.with_endpoint_expiry_observer(vod_endpoint_recovery.observer())
        }
        web_media_hls::HlsCatalogPresentation::Live => transport_request,
    };
    let http =
        native_adaptive_http_context(transport_request, request.network_config, adaptive_limits)?;

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
    let discovered = crate::web_media_hls_open::discover_native_hls_catalog(
        &open_request,
        snapshot_identity.catalog,
        presentation,
        provider_default.current_master_variant_index(),
        request.system_capabilities,
        request.audio_capabilities,
    )?;
    let (prepared, neutral_selection, component_catalog) = match discovered {
        HlsCatalogDiscoveryOutcome::Unavailable => {
            let neutral_selection = match request.expected_selection {
                Some(expected) => expected
                    .rematch(
                        snapshot_identity.parent.clone(),
                        WebMediaSelectionRematchSource::Candidate,
                    )
                    .context("native HLS media selection semantic rematch failed")?,
                None => WebMediaSelection::candidate(snapshot_identity.parent.clone()),
            };
            let prepared = prepare_native_hls_runtime(NativeHlsRuntimeRequest {
                open: open_request,
                presentation,
                selection: NativeHlsRuntimeSelection::ProviderDefault,
                vod_start: request.start,
                parent: snapshot_identity.parent.clone(),
                source: request.source,
                network_config: request.network_config,
                cancellation: request.cancellation.clone(),
                vod_endpoint_recovery,
            })?;
            (prepared, neutral_selection, None)
        }
        HlsCatalogDiscoveryOutcome::Installed(snapshot) => {
            let neutral_selection = match request.expected_selection {
                Some(expected) => expected
                    .rematch(
                        snapshot_identity.parent.clone(),
                        WebMediaSelectionRematchSource::ComponentCatalog(snapshot.catalog()),
                    )
                    .context("native HLS master selection semantic rematch failed")?,
                None => WebMediaSelection::with_components(
                    snapshot_identity.parent.clone(),
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
            let prepared = prepare_native_hls_runtime(NativeHlsRuntimeRequest {
                open: open_request,
                presentation,
                selection: NativeHlsRuntimeSelection::Catalog(Box::new(reopen)),
                vod_start: request.start,
                parent: snapshot_identity.parent.clone(),
                source: request.source,
                network_config: request.network_config,
                cancellation: request.cancellation.clone(),
                vod_endpoint_recovery,
            })?;
            (prepared, neutral_selection, Some(component_catalog))
        }
    };

    let (demuxer, seek_port, lifecycle) = prepared.into_parts();
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
        source_state,
        lifecycle,
    }))
}

/// Runtime selection различает provider-default и exact fresh-catalog reopen без opaque Option.
enum NativeHlsRuntimeSelection {
    ProviderDefault,
    Catalog(Box<web_media_hls::HlsCatalogReopenSelection>),
}

/// Полный intent одного VOD/live runtime open после content-based admission.
struct NativeHlsRuntimeRequest<'a> {
    open: HlsVodOpenRequest,
    presentation: web_media_hls::HlsCatalogPresentation,
    selection: NativeHlsRuntimeSelection,
    vod_start: HlsVodStartIntent,
    parent: ExactSelectionIdentity,
    source: &'a NativeHlsUrl,
    network_config: &'a NetworkConfig,
    cancellation: CancellationToken,
    vod_endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
}

/// Prepared runtime сохраняет взаимоисключающие VOD recovery и live timeline attachments.
enum PreparedNativeHlsRuntime {
    Vod {
        prepared: crate::web_media_hls_open::PreparedNativeHlsVod,
        endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
    },
    Live(crate::web_media_hls_open::PreparedNativeHlsLive),
}

impl PreparedNativeHlsRuntime {
    fn into_parts(
        self,
    ) -> (
        Box<dyn Demuxer + Send>,
        Arc<dyn PreparedDemuxSeekPort>,
        PreparedNativeHlsLifecycle,
    ) {
        match self {
            Self::Vod {
                prepared,
                endpoint_recovery,
            } => {
                endpoint_recovery.arm_after_candidate_finalization();
                let crate::web_media_hls_open::PreparedNativeHlsVod {
                    demuxer,
                    seek_port,
                    initial_position,
                } = prepared;
                let demuxer = endpoint_recovery.wrap_demuxer(demuxer);
                let seek_port = endpoint_recovery
                    .wrap_seek_port(Some(seek_port))
                    .expect("native HLS VOD всегда публикует receipted seek port");
                (
                    demuxer,
                    seek_port,
                    PreparedNativeHlsLifecycle::Vod {
                        initial_position,
                        endpoint_recovery,
                    },
                )
            }
            Self::Live(prepared) => (
                prepared.demuxer,
                prepared.seek_port,
                PreparedNativeHlsLifecycle::Live {
                    timeline_port: prepared.timeline_port,
                },
            ),
        }
    }
}

/// Делегирует data plane существующим VOD/S33 owners и запрещает post-admission fallback.
fn prepare_native_hls_runtime(
    request: NativeHlsRuntimeRequest<'_>,
) -> Result<PreparedNativeHlsRuntime> {
    match request.presentation {
        web_media_hls::HlsCatalogPresentation::Vod => {
            let prepared = match request.selection {
                NativeHlsRuntimeSelection::ProviderDefault => {
                    crate::web_media_hls_open::prepare_native_hls_vod(
                        request.open,
                        request.vod_start,
                    )
                }
                NativeHlsRuntimeSelection::Catalog(selection) => {
                    crate::web_media_hls_open::prepare_native_hls_catalog_vod(
                        request.open,
                        *selection,
                        request.vod_start,
                    )
                }
            }
            .context("native HLS VOD runtime preparation")?;
            Ok(PreparedNativeHlsRuntime::Vod {
                prepared,
                endpoint_recovery: request.vod_endpoint_recovery,
            })
        }
        web_media_hls::HlsCatalogPresentation::Live => {
            let endpoint_refresh = Arc::new(live_refresh::NativeHlsEndpointRefreshPort::new(
                request.parent,
                request.source.clone(),
                request.network_config.clone(),
                request.cancellation,
            ));
            let live_request = web_media_hls::HlsLiveOpenRequest {
                common: request.open,
                endpoint_refresh,
                timeline_port_generation:
                    crate::web_media_open::next_dynamic_timeline_port_generation()?,
                initial_source_epoch: media_core::DynamicMediaTimelineEpoch::new(0),
            };
            let prepared = match request.selection {
                NativeHlsRuntimeSelection::ProviderDefault => {
                    crate::web_media_hls_open::prepare_native_hls_live(live_request)
                }
                NativeHlsRuntimeSelection::Catalog(selection) => {
                    crate::web_media_hls_open::prepare_native_hls_catalog_live(
                        live_request,
                        *selection,
                    )
                }
            }?;
            Ok(PreparedNativeHlsRuntime::Live(prepared))
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
