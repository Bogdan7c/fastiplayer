//! Единый app-owned candidate → transport → demux composition path для YtDlp media.
//!
//! Queue/media-open владеют моментом commit-а, `service-ytdlp` — extraction и
//! request material, pure planner — выбором playable candidate, а этот модуль
//! только соединяет concrete runtime registries до существующего commit barrier.

pub(crate) mod catalog;
pub(crate) mod catalog_capabilities;
pub(crate) mod component_variants;
#[cfg(test)]
mod component_variants_tests;
mod content_probe;
mod content_probe_fallback;
#[cfg(test)]
pub(crate) mod content_probe_tests;
mod hds;
pub(crate) use hds::{NativeHdsCandidatePreparation, prepare_native_hds_candidate};
/// Fresh extraction/rematch и process-local generation allocators.
mod preparation;
pub(crate) use preparation::next_dynamic_timeline_port_generation;
/// Concrete transport/demux registries и immutable capability snapshots одного attempt-а.
mod runtime;
mod smooth;
pub(crate) use smooth::{NativeSmoothCandidatePreparation, prepare_native_smooth_candidate};
mod source_state;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use demux_api::{CompositeAvDemuxer, CompositeAvTrackSelection, CompositeComponentLeadPolicy};
use fastiplayer_config::{
    NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec, WebMediaConfig,
    WebMediaHdrSelection, YtDlpConfig,
};
use media_core::{
    Demuxer, DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration, TrackId, TrackKind,
};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpLiveIntent, YtDlpMediaLocator,
    YtDlpNormalizedCandidate,
};
use source_core::CancellationToken;
use web_media_core::{
    ContainerFamily, ExactSelectionIdentity, ExtractionGeneration, SelectionRequest, SourceIdentity,
};
use web_media_dash::DashEndpointRefreshPort;
use web_media_hls::HlsEndpointRefreshPort;
use web_media_playback_plan::{HdrSelectionPolicy, PlaybackSelectionPolicy, plan_playback};
use web_media_transport_api::MediaComponentRole;

pub(crate) use component_variants::{
    ComponentVariantFinalizationError, YtDlpComponentSelectionOpenIntent,
    YtDlpExactCandidateOpenIntent,
};
use component_variants::{
    PreparedComponentVariantCatalog, finalize_component_variant_configuration,
};
use runtime::WebOpenRuntime;
#[cfg(test)]
use runtime::progressive_transport_capabilities;
pub(crate) use source_state::ExtractorMediaSourceState;

/// Separate A/V не должен опережать companion более чем на этот интервал.
const COMPOSITE_MAX_TIMESTAMP_LEAD: Duration = Duration::from_millis(500);
/// Один retained packet на component bounded независимо от transport sniff/bootstrap chunk.
const COMPOSITE_MAX_PENDING_PACKET_BYTES: usize = 4 * 1024 * 1024;

/// Process-local source identity allocator не связывает neutral core с queue ID representation.
static NEXT_YT_DLP_SOURCE_IDENTITY: AtomicU64 = AtomicU64::new(1);
/// Process-local identity allocator для S31L timeline ports.
static NEXT_DYNAMIC_TIMELINE_PORT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Process-local allocator независимой generation component catalog-а.
static NEXT_COMPONENT_VARIANT_CATALOG_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Намерение selection: новый лучший playable candidate либо semantic rematch старого exact выбора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum YtDlpCandidateOpenIntent {
    /// Первичное открытие либо явная runtime override/reselection.
    BestPlayable,
    /// Restore/rebuild обязан сохранить semantic candidate identity.
    Exact(Box<YtDlpExactCandidateOpenIntent>),
    /// Service-owned video-only + audio-only composition из одного fresh snapshot-а.
    Composed(Box<component_variants::YtDlpComposedCandidateOpenIntent>),
}

/// Проверяет, что product reason соответствует media-open lifecycle, а не только selection shape.
fn validate_extractor_reason(
    intent: &YtDlpCandidateOpenIntent,
    reason: web_media_core::ExtractorInvocationReason,
) -> Result<()> {
    let compatible = match intent {
        YtDlpCandidateOpenIntent::BestPlayable => matches!(
            reason,
            web_media_core::ExtractorInvocationReason::PageMediaResolution
                | web_media_core::ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial
                | web_media_core::ExtractorInvocationReason::NativeProfileCompatibilityFallback
                | web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery
        ),
        YtDlpCandidateOpenIntent::Exact(_) | YtDlpCandidateOpenIntent::Composed(_) => matches!(
            reason,
            web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery
        ),
    };
    if compatible {
        Ok(())
    } else {
        Err(anyhow!(
            "extractor invocation reason {reason:?} несовместим с media-open intent"
        ))
    }
}

/// Результат pre-barrier подготовки, который ещё не меняет player/queue state.
pub(crate) struct PreparedYtDlpWebMedia {
    /// Player-facing demuxer выбранного single либо compound candidate-а.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Service metadata из того же extraction snapshot-а, что и exact selection.
    pub(crate) playlist_metadata: service_ytdlp::YtDlpPlaylistMetadata,
    /// Reconstructible extractor state; provider DTO не выходит в lifecycle/UI.
    pub(crate) source_state: ExtractorMediaSourceState,
    /// Точный lifecycle kind того же extraction generation.
    pub(crate) presentation: web_media_core::WebMediaPresentationKind,
    /// Product reason фактически выполненной extractor invocation.
    pub(crate) extractor_reason: web_media_core::ExtractorInvocationReason,
    /// Neutral S31L port присутствует только у proven HLS live runtime.
    pub(crate) timeline_port: Option<DynamicMediaTimelinePort>,
    /// Worker-receipted demux seek port присутствует у static DASH/Smooth/HDS VOD.
    pub(crate) demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,
    /// Optional absolute source window для zero-based public presentation.
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,
    /// Candidate-level VOD expiry gate; live runtime сохраняет собственного refresh owner-а.
    pub(crate) vod_endpoint_recovery:
        Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

impl PreparedYtDlpWebMedia {
    /// Даёт extractor-focused tests только secret-safe installed projection.
    #[cfg(test)]
    pub(crate) const fn stream_configuration(
        &self,
    ) -> &crate::web_media_stream_model::WebMediaStreamConfiguration {
        self.source_state.stream_configuration()
    }

    /// Extractor-focused tests могут проверить exact rematch token внутри adapter module.
    #[cfg(test)]
    pub(crate) const fn candidate_selection(&self) -> &YtDlpCandidateSelection {
        self.source_state.candidate_selection()
    }
}

/// Общий pre-barrier runtime result concrete transport branches.
struct OpenedWebCandidate {
    /// Player-facing demuxer.
    demuxer: Box<dyn Demuxer + Send>,
    /// Descriptor-only HLS subtitles.
    subtitles: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
    /// Dynamic timeline only для proven live provider-а.
    timeline_port: Option<DynamicMediaTimelinePort>,
    /// Async demux seek only для provider-а, который требует worker receipt.
    demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,
    /// Provider-owned absolute source window до player commit.
    playback_window: Option<player_core::MediaPlaybackWindow>,
    /// Fresh provider result финализируется до authorization barrier.
    component_variants: PreparedComponentVariantCatalog,
    /// Attachment создаётся отдельно для каждой physical candidate attempt.
    vod_endpoint_recovery: Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

/// Source-specific live refresh ports, собранные одним app composition boundary.
struct AdaptiveEndpointRefreshPorts {
    /// HLS endpoint owner присутствует только для выбранного HLS live candidate.
    hls: Option<Arc<dyn HlsEndpointRefreshPort>>,
    /// DASH endpoint owner присутствует только для выбранного DASH live candidate.
    dash: Option<Arc<dyn DashEndpointRefreshPort>>,
}

/// Named context одного concrete candidate open-а.
struct WebCandidateOpenContext {
    /// Extractor-declared live/VOD intent.
    live_intent: YtDlpLiveIntent,
    /// Provider-specific endpoint refresh owners.
    endpoint_refresh_ports: AdaptiveEndpointRefreshPorts,
    /// Reserved neutral timeline generation для live provider-а.
    timeline_port_generation: DynamicMediaTimelinePortGeneration,
    /// Fresh component catalog должен применить этот exact reopen intent.
    component_selection_intent: YtDlpComponentSelectionOpenIntent,
    /// App-owned quality preference для provider default selection.
    preferred_height: web_media_core::PreferredHeightPolicy,
    /// Exact parent и caller-owned catalog generation одного discovery pass-а.
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    /// Общая cancellation generation transport/demux runtime-а.
    cancellation: CancellationToken,
    /// VOD-only observer/gate не подменяет HLS/DASH live refresh ports.
    vod_endpoint_recovery: Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

/// Открывает YtDlp locator одним S19 → S21C → S22 production path-ом.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_yt_dlp_web_media(
    locator: &YtDlpMediaLocator,
    network_config: &NetworkConfig,
    web_media_config: &WebMediaConfig,
    yt_dlp_config: &YtDlpConfig,
    extractor_adapter: &service_ytdlp::YtDlpExtractorAdapter,
    demux_config: &PlayerDemuxConfig,
    preferred_video_codec_order: &[ConfigVideoCodec],
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    intent: YtDlpCandidateOpenIntent,
    extractor_reason: web_media_core::ExtractorInvocationReason,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedYtDlpWebMedia> {
    ensure_not_cancelled(&is_cancelled)?;
    validate_extractor_reason(&intent, extractor_reason)?;
    let component_selection_intent = intent.component_selection_intent();
    let selection_preference = match &intent {
        YtDlpCandidateOpenIntent::BestPlayable => {
            crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
                web_media_config,
            )
        }
        YtDlpCandidateOpenIntent::Exact(exact) => exact.preference,
        YtDlpCandidateOpenIntent::Composed(composed) => composed.preference,
    };
    let (candidate_snapshot, resolved_intent) = preparation::resolve_candidate_snapshot(
        extractor_adapter,
        locator,
        yt_dlp_config,
        intent,
        extractor_reason,
        &is_cancelled,
    )
    .context("Не удалось подготовить exact YtDlp candidate snapshot")?;
    // Новый узкий adapter projection локализует extractor DTO до neutral catalog boundary.
    let extractor_catalog_projection =
        crate::web_media_extractor_adapter::ExtractorCatalogProjection::from_snapshot(
            &candidate_snapshot,
        )?;
    // Сохраняем безопасный счётчик row-local planning rejections для diagnostics.
    let planning_rejection_count = extractor_catalog_projection.planning_rejection_count();
    // Downstream planner читает existing neutral catalog без второго inventory.
    let planning_snapshot = extractor_catalog_projection.catalog();
    let runtime = WebOpenRuntime::new(network_config, demux_config)
        .context("Не удалось собрать web-media runtime registries")?;
    let policy = selection_policy(web_media_config, preferred_video_codec_order)
        .context("Не удалось собрать YtDlp playback selection policy")?;
    let capabilities = runtime.playback_capabilities(system_capabilities, audio_capabilities);
    let planning_candidate_count = planning_snapshot.candidates().len();
    let normalization_rejection_count = candidate_snapshot
        .inventory()
        .iter()
        .filter(|entry| entry.rejected().is_some())
        .count()
        + usize::from(
            candidate_snapshot
                .selected()
                .is_some_and(|entry| entry.rejected().is_some()),
        );
    ensure_not_cancelled(&is_cancelled)?;
    let mut catalog_capability_probe = catalog_capabilities::AppCatalogCapabilityProbe::new(
        system_capabilities.clone(),
        audio_capabilities,
    );
    let mut attempt_context = content_probe_fallback::CandidateAttemptContext {
        locator,
        network_config,
        yt_dlp_config,
        extractor_adapter,
        candidate_snapshot: &candidate_snapshot,
        runtime: &runtime,
        component_selection_intent: &component_selection_intent,
        preferred_height: crate::web_media_quality::preferred_height_policy(
            web_media_config.preferred_video_height,
        ),
        cancellation: &cancellation,
        is_cancelled: &is_cancelled,
        playback_policy: &policy,
        catalog_capability_probe: &mut catalog_capability_probe,
    };
    let (candidate_selection, composed_selection, opened_candidate) = match resolved_intent {
        preparation::ResolvedCandidateIntent::Planner(SelectionRequest::BestPlayable) => {
            let ranked = content_probe_fallback::ranked_best_playable_candidates(
                &candidate_snapshot,
                planning_snapshot,
                capabilities,
                &policy,
            )
            .with_context(|| {
                format!(
                    "YtDlp planner не нашёл playable candidate (planning_candidates={planning_candidate_count}, normalization_rejections={normalization_rejection_count}, planning_rejections={planning_rejection_count})"
                )
            })?;
            let (_, opened_attempt) =
                content_probe_fallback::open_ranked_best(ranked, &is_cancelled, |candidate| {
                    let selection = candidate_snapshot
                        .selection_for(candidate)
                        .context("Planner-ranked YtDlp candidate не имеет exact selection")?;
                    attempt_context.open(candidate, selection)
                })
                .context("Не удалось открыть planner-ranked BestPlayable YtDlp candidate")?;
            let (selection, opened) = opened_attempt.into_parts();
            (selection, None, opened)
        }
        preparation::ResolvedCandidateIntent::Planner(
            selection_request @ SelectionRequest::Exact(_),
        ) => {
            let outcome = plan_playback(
                planning_snapshot,
                capabilities,
                &selection_request,
                &policy,
            )
            .map_err(|error| {
                let safe_summary = error.safe_summary();
                anyhow::Error::new(error).context(format!(
                    "YtDlp planner не нашёл exact playable candidate (planning_candidates={planning_candidate_count}, normalization_rejections={normalization_rejection_count}, planning_rejections={planning_rejection_count}, {safe_summary})"
                ))
            })?;
            let selected = candidate_snapshot
                .canonical_candidate_for_planning_identity(
                    outcome.selected().exact_identity(),
                    outcome.selected().semantic_identity(),
                )
                .ok_or_else(|| anyhow!("planner выбрал отсутствующий YtDlp candidate"))?;
            let selection = candidate_snapshot
                .selection_for(selected)
                .context("Exact YtDlp candidate не имеет exact selection")?;
            let opened_attempt = content_probe_fallback::open_single(selected, |candidate| {
                attempt_context.open(candidate, selection)
            })
            .context("Не удалось открыть exact YtDlp candidate")?;
            let (selection, opened) = opened_attempt.into_parts();
            (selection, None, opened)
        }
        preparation::ResolvedCandidateIntent::Composed {
            candidate,
            selection,
            parent_preference,
        } => {
            let opened_attempt =
                content_probe_fallback::open_single(candidate.as_ref(), |candidate| {
                    attempt_context.open(candidate, *parent_preference)
                })
                .context("Не удалось открыть composed YtDlp candidate")?;
            let (candidate_selection, opened) = opened_attempt.into_parts();
            (candidate_selection, Some(selection), opened)
        }
    };
    let extractor_projection =
        extractor_catalog_projection.with_active_selection(&candidate_selection)?;
    let planning_snapshot = extractor_projection.catalog();
    let stream_configuration =
        crate::web_media_stream_model::WebMediaStreamConfiguration::from_neutral_catalog(
            planning_snapshot,
            capabilities,
            &policy,
            extractor_projection.selection(),
            selection_preference,
        )
        .context("Не удалось построить secret-safe URL sidebar stream model")?;
    let stream_configuration =
        stream_configuration.with_hls_subtitle_renditions(opened_candidate.subtitles);
    ensure_not_cancelled(&is_cancelled)?;
    let stream_configuration = finalize_component_variant_configuration(
        stream_configuration,
        component_selection_intent.clone(),
        opened_candidate.component_variants,
    )
    .context("Не удалось финализировать fresh component variant configuration")?;
    ensure_not_cancelled(&is_cancelled)?;
    let catalog_projection = catalog::catalog_attachment(catalog::CatalogAttachmentRequest {
        candidate_snapshot: &candidate_snapshot,
        planning_snapshot,
        capabilities,
        policy: &policy,
        active_selection: &candidate_selection,
        active_composed: composed_selection.as_deref(),
    })?;
    let catalog_attachment = catalog_projection.attachment;
    let catalog_selection_routes = catalog_projection.routes.into();
    let extractor_projection = extractor_projection.with_neutral_selection(
        stream_configuration
            .neutral_selection()
            .context("Не удалось спроецировать canonical component selection")?,
    )?;
    let vod_endpoint_recovery = opened_candidate.vod_endpoint_recovery;
    if extractor_projection.presentation() == web_media_core::WebMediaPresentationKind::Live
        && vod_endpoint_recovery.is_some()
    {
        return Err(anyhow!(
            "Live extractor projection не может владеть VOD endpoint recovery"
        ));
    }
    if let Some(recovery) = vod_endpoint_recovery.as_ref() {
        recovery.arm_after_candidate_finalization();
    }
    let demuxer = match vod_endpoint_recovery.as_ref() {
        Some(recovery) => recovery.wrap_demuxer(opened_candidate.demuxer),
        None => opened_candidate.demuxer,
    };
    let demux_seek_port = match vod_endpoint_recovery.as_ref() {
        Some(recovery) => recovery.wrap_seek_port(opened_candidate.demux_seek_port),
        None => opened_candidate.demux_seek_port,
    };
    let neutral_selection = extractor_projection.selection().clone();
    let presentation = extractor_projection.presentation();
    let playlist_metadata = extractor_projection.into_playlist_metadata();
    Ok(PreparedYtDlpWebMedia {
        demuxer,
        playlist_metadata,
        source_state: ExtractorMediaSourceState {
            neutral_selection,
            candidate_selection,
            composed_selection,
            stream_configuration,
            catalog_attachment,
            catalog_selection_routes,
        },
        presentation,
        extractor_reason,
        timeline_port: opened_candidate.timeline_port,
        demux_seek_port,
        playback_window: opened_candidate.playback_window,
        vod_endpoint_recovery,
    })
}

/// Открытый physical component до layout composition.
struct OpenedCandidateComponent {
    /// Exact semantic role из selected candidate.
    role: MediaComponentRole,
    /// Уже проверенный concrete demuxer.
    demuxer: Box<dyn Demuxer + Send>,
}

/// Выдаёт новую non-zero process-local source lineage без URL/queue representation coupling.
fn next_source_identity() -> Result<SourceIdentity> {
    let source_value = NEXT_YT_DLP_SOURCE_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("YtDlp source identity space исчерпан"))?;
    Ok(SourceIdentity::new(source_value))
}

/// Строит pure selection policy из committed user config.
fn selection_policy(
    web_media_config: &WebMediaConfig,
    preferred_video_codec_order: &[ConfigVideoCodec],
) -> Result<PlaybackSelectionPolicy> {
    let hdr = match web_media_config.hdr_selection {
        WebMediaHdrSelection::SdrOnly => HdrSelectionPolicy::SdrOnly,
        WebMediaHdrSelection::PreferHdrWhenAvailable => HdrSelectionPolicy::PreferHdrWhenAvailable,
    };
    let codecs = preferred_video_codec_order
        .iter()
        .copied()
        .map(crate::startup_media::runtime_video_codec)
        .collect();
    let containers = vec![
        ContainerFamily::WebM,
        ContainerFamily::IsoBmff,
        ContainerFamily::FragmentedIsoBmff,
        ContainerFamily::Matroska,
        ContainerFamily::Ogg,
        ContainerFamily::Flac,
        ContainerFamily::MpegAudio,
        ContainerFamily::Wav,
        ContainerFamily::Aiff,
        ContainerFamily::Caf,
    ];
    PlaybackSelectionPolicy::new(
        hdr,
        codecs,
        crate::web_media_quality::preferred_height_policy(web_media_config.preferred_video_height),
        containers,
    )
    .map_err(Into::into)
}

/// Проверяет actual tracks, чтобы descriptor shape не подменял runtime evidence.
fn validate_component_tracks(role: MediaComponentRole, demuxer: &dyn Demuxer) -> Result<()> {
    let has_video = selected_track(demuxer, TrackKind::Video).is_some();
    let has_audio = selected_track(demuxer, TrackKind::Audio).is_some();
    let shape_matches = match role {
        MediaComponentRole::Muxed => has_video && has_audio,
        MediaComponentRole::ContentProbed => has_video || has_audio,
        MediaComponentRole::Video => has_video,
        MediaComponentRole::Audio => has_audio,
        MediaComponentRole::Subtitle | MediaComponentRole::PresentationManifest => false,
    };
    if !shape_matches {
        bail!("Opened YtDlp component tracks не совпадают с selected descriptor role");
    }
    Ok(())
}

/// Композирует single layout либо ровно одну separate video/audio пару.
fn compose_candidate_components(
    mut components: Vec<OpenedCandidateComponent>,
) -> Result<Box<dyn Demuxer + Send>> {
    if components.len() == 1 {
        return Ok(components.remove(0).demuxer);
    }
    if components.len() != 2 {
        bail!("YtDlp candidate содержит неподдерживаемое число physical components");
    }
    let video_index = components
        .iter()
        .position(|component| component.role == MediaComponentRole::Video)
        .ok_or_else(|| anyhow!("Separate YtDlp candidate не содержит video component"))?;
    let video = components.remove(video_index).demuxer;
    let audio_index = components
        .iter()
        .position(|component| component.role == MediaComponentRole::Audio)
        .ok_or_else(|| anyhow!("Separate YtDlp candidate не содержит audio component"))?;
    let audio = components.remove(audio_index).demuxer;
    let video_track = selected_track(video.as_ref(), TrackKind::Video)
        .ok_or_else(|| anyhow!("Separate video demuxer не содержит video track"))?;
    let audio_track = selected_track(audio.as_ref(), TrackKind::Audio)
        .ok_or_else(|| anyhow!("Separate audio demuxer не содержит audio track"))?;
    let lead_policy = progressive_composite_lead_policy()?;
    let composite = CompositeAvDemuxer::new(
        video,
        audio,
        CompositeAvTrackSelection::new(video_track, audio_track),
        lead_policy,
    )
    .context("Не удалось скомпоновать separate YtDlp A/V demuxers")?;
    Ok(Box::new(composite))
}

/// Держит encoded-packet bound независимым от transport sniff/bootstrap настройки.
fn progressive_composite_lead_policy() -> Result<CompositeComponentLeadPolicy> {
    CompositeComponentLeadPolicy::single_pending_packet(
        COMPOSITE_MAX_TIMESTAMP_LEAD,
        NonZeroUsize::new(COMPOSITE_MAX_PENDING_PACKET_BYTES)
            .expect("composite pending packet limit ненулевой"),
    )
    .context("Progressive composite A/V safety bounds invalid")
}

/// Возвращает первый track нужного kind-а как explicit composition selection.
fn selected_track(demuxer: &dyn Demuxer, kind: TrackKind) -> Option<TrackId> {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == kind)
        .map(|track| track.id)
}

/// Проверяет caller-owned cancellation между потенциально дорогими boundaries.
fn ensure_not_cancelled(is_cancelled: &impl Fn() -> bool) -> Result<()> {
    if is_cancelled() {
        bail!("YtDlp web media preparation отменена");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTTP sniff chunk не должен становиться случайным лимитом encoded video packet-а.
    #[test]
    fn progressive_composite_packet_limit_is_independent_from_sniff_chunk() {
        let lead_policy =
            progressive_composite_lead_policy().expect("progressive composite policy валидна");

        assert_eq!(
            lead_policy.bootstrap_byte_limit(),
            COMPOSITE_MAX_PENDING_PACKET_BYTES
        );
        assert!(lead_policy.bootstrap_byte_limit() > 64 * 1024);
    }
    use demux_api::{
        DemuxContainerId, DemuxContainerRegistration, DemuxFactoryDescriptor, DemuxFactoryId,
        DemuxFixtureId, DemuxInputCapabilities, DemuxInputCapability,
    };
    use symphonia_demux::DemuxerOptions;
    use web_media_core::{FtpScheme, TransportFamily};

    #[test]
    fn media_open_reason_matrix_rejects_topology_and_preserves_page_recovery_reason() {
        for reason in [
            web_media_core::ExtractorInvocationReason::PageMediaResolution,
            web_media_core::ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial,
            web_media_core::ExtractorInvocationReason::NativeProfileCompatibilityFallback,
            web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
        ] {
            assert!(
                validate_extractor_reason(&YtDlpCandidateOpenIntent::BestPlayable, reason).is_ok()
            );
        }
        assert!(
            validate_extractor_reason(
                &YtDlpCandidateOpenIntent::BestPlayable,
                web_media_core::ExtractorInvocationReason::CollectionTopologyResolution,
            )
            .is_err(),
            "collection/topology reason не должен достигать media-open extractor path"
        );
    }

    /// Shutdown cancellation завершается до запуска extractor/network side effects.
    #[test]
    fn cancellation_is_a_pre_barrier_failure() {
        let locator =
            service_ytdlp::parse_yt_dlp_media_locator("https://media.example.test/watch?id=secret")
                .expect("valid test locator");
        let result = prepare_yt_dlp_web_media(
            &locator,
            &NetworkConfig::default(),
            &WebMediaConfig::default(),
            &YtDlpConfig::default(),
            &service_ytdlp::YtDlpExtractorAdapter::default(),
            &PlayerDemuxConfig::default(),
            &[ConfigVideoCodec::Vp9],
            &capability_core::SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
            YtDlpCandidateOpenIntent::BestPlayable,
            web_media_core::ExtractorInvocationReason::PageMediaResolution,
            CancellationToken::new(),
            || true,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("cancelled preparation не должна запускать yt-dlp"),
        };
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("отменена"));
        assert!(!diagnostic.contains("secret"));
    }

    /// Concrete Symphonia descriptor и planner snapshot не расходятся по S22 containers.
    #[test]
    fn demux_capability_snapshot_is_derived_from_registered_factory() {
        use demux_api::DemuxFactory;

        let factory = symphonia_demux::SymphoniaDemuxFactory::new(DemuxerOptions::default())
            .expect("Symphonia factory");
        let capabilities =
            crate::web_media_demux_registry::capabilities_for_descriptors([factory.descriptor()])
                .expect("capability snapshot");
        let expected_inputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        for family in [
            ContainerFamily::IsoBmff,
            ContainerFamily::FragmentedIsoBmff,
            ContainerFamily::Matroska,
            ContainerFamily::WebM,
            ContainerFamily::Ogg,
            ContainerFamily::Flac,
            ContainerFamily::Wav,
            ContainerFamily::Aiff,
            ContainerFamily::Caf,
            ContainerFamily::MpegAudio,
        ] {
            let expected_family_inputs = if matches!(
                family,
                ContainerFamily::IsoBmff
                    | ContainerFamily::FragmentedIsoBmff
                    | ContainerFamily::Matroska
                    | ContainerFamily::WebM
            ) {
                expected_inputs.with(DemuxInputCapability::OrderedSegments)
            } else {
                expected_inputs
            };
            assert_eq!(
                capabilities.input_capabilities_for(family),
                expected_family_inputs,
                "family {family:?} должна получить exact registered inputs"
            );
        }
    }

    /// Planner projection не переносит capability между соседними container rows.
    #[test]
    fn demux_capability_snapshot_preserves_per_container_input_sets() {
        let iso_inputs = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
        let webm_inputs = DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes);
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("synthetic-per-container").expect("factory ID"),
            vec![
                DemuxContainerRegistration::new(
                    DemuxContainerId::new("iso-bmff").expect("ISO BMFF container ID"),
                    iso_inputs,
                    vec![],
                    vec![],
                ),
                DemuxContainerRegistration::new(
                    DemuxContainerId::new("webm").expect("WebM container ID"),
                    webm_inputs,
                    vec![],
                    vec![],
                ),
            ],
            vec![DemuxFixtureId::new("synthetic/per-container").expect("fixture ID")],
        );

        let capabilities =
            crate::web_media_demux_registry::capabilities_for_descriptors([&descriptor])
                .expect("capability snapshot");
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::IsoBmff),
            iso_inputs
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::FragmentedIsoBmff),
            iso_inputs
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::WebM),
            webm_inputs
        );
        assert!(
            !capabilities
                .input_capabilities_for(ContainerFamily::WebM)
                .contains(DemuxInputCapability::OrderedSegments),
            "WebM не должен наследовать synthetic ISO ordered capability"
        );
    }

    /// Component catalog generation монотонна и fail-closed при исчерпании.
    #[test]
    fn component_variant_catalog_generation_is_monotonic_and_overflow_checked() {
        let allocator = AtomicU64::new(41);

        assert_eq!(
            preparation::allocate_component_variant_catalog_generation(&allocator)
                .expect("first catalog generation")
                .value(),
            41
        );
        assert_eq!(
            preparation::allocate_component_variant_catalog_generation(&allocator)
                .expect("second catalog generation")
                .value(),
            42
        );

        allocator.store(u64::MAX, Ordering::Relaxed);
        assert!(
            preparation::allocate_component_variant_catalog_generation(&allocator).is_err(),
            "исчерпанный allocator не должен оборачивать catalog generation"
        );
    }

    /// Planner видит exact adaptive input shapes только после concrete runtimes.
    #[test]
    fn transport_capability_snapshot_advertises_dash_ordered_and_range_inputs() {
        let capabilities =
            progressive_transport_capabilities().expect("transport capability snapshot builds");
        let dash_inputs = capabilities.output_inputs_for(TransportFamily::Dash);

        assert_eq!(
            dash_inputs,
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
                .with(DemuxInputCapability::SeekableBytes)
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::Hls),
            DemuxInputCapabilities::only(crate::web_media_hls_open::hls_transport_input()),
            "DASH registration не должна менять соседний HLS provider"
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::SmoothStreaming),
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
            "S36 Smooth runtime должен рекламировать только реально используемый ordered input"
        );
        let progressive_outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftp)),
            progressive_outputs,
            "S37 FTP runtime должен рекламировать seekable и streaming byte inputs"
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftps)),
            progressive_outputs,
            "S37 FTPS runtime должен рекламировать seekable и streaming byte inputs"
        );
    }
}
