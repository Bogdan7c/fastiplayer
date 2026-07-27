//! App-owned composition static DASH VOD runtime-а.

use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashContainer, DashMediaKind, DashMpdLimits, DashUtcTimestamp};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxRegistry, DemuxSniffBudget,
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use media_core::{
    DemuxRetryHint, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration,
};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};
use rustiplayer_config::NetworkConfig;
use service_ytdlp::{
    YtDlpDashFragmentLocatorKind, YtDlpDashFragmentRole, YtDlpDashInputKind,
    YtDlpDashRequestMaterial, YtDlpDashTransportComponent, YtDlpLiveIntent,
    YtDlpNormalizedCandidate, YtDlpTransportRequestContext,
};
use source_core::{CancellationToken, HttpRequestTarget, SourceRuntimeConfig};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceQueryApplication, AdaptiveRetryPolicy,
    AdaptiveTransportLimits,
};
use web_media_core::{
    AudioTrackDescriptor, ContainerFamily, MuxedComponentDescriptor, StreamLayout, TransportFamily,
    VideoTrackDescriptor,
};
use web_media_dash::{
    DashEndpointRefreshPort, DashLiveCatalogDiscoveryRequest, DashLiveOpenRequest,
    DashManifestInput, DashPresentationSelection, DashRepresentationEvidence,
    DashResourceReference, DashSerializedComponent, DashSerializedFragment,
    DashSerializedFragmentKind, DashSerializedPresentation, DashVideoDimensions,
    DashVodCatalogDiscoveryRequest, DashVodHttpContext, DashVodInput, DashVodOpenPolicy,
    DashVodOpenRequest, DashWallClock, discover_dash_live_catalog, discover_dash_vod_catalog,
    prepare_dash_live, prepare_dash_vod, prepare_discovered_dash_live_semantic,
    prepare_discovered_dash_vod_semantic,
};
use web_media_transport_api::{MediaComponentRole, TransportProviderId};

/// Результат pre-barrier DASH preparation.
pub(crate) struct PreparedDashCandidate {
    /// Ready nonblocking demuxer.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Provider-neutral seek port exact этого runtime-а.
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Dynamic S31L port; static VOD сохраняет `None`.
    pub(crate) timeline_port: Option<DynamicMediaTimelinePort>,
    pub(crate) component_variants:
        crate::web_media_open::component_variants::PreparedComponentVariantCatalog,
}

/// Production local wall clock; direct UTCTiming offset применяется provider-ом.
struct SystemDashWallClock;

impl DashWallClock for SystemDashWallClock {
    fn now_utc(&self) -> DashUtcTimestamp {
        let unix_nanoseconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX))
            .unwrap_or_default();
        DashUtcTimestamp::from_unix_nanoseconds(unix_nanoseconds)
    }
}

/// Adapter не переносит DASH vocabulary в player-core.
struct DashPreparedDemuxSeekPort {
    /// Cloneable S34B control handle.
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for DashPreparedDemuxSeekPort {
    /// Строит exact runtime fence из player-owned request identity.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: media_core::DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.handle
            .enqueue(
                ProgressiveSeekFence {
                    runtime_generation: self.handle.runtime_generation(),
                    request_id: ProgressiveSeekRequestId::new(request_id.value()),
                },
                request,
            )
            .map_err(map_enqueue_error)
    }

    /// Переводит provider receipt в neutral player vocabulary.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.handle
            .poll_receipt()
            .map(|receipt| PreparedDemuxSeekReceipt {
                request_id: PreparedDemuxSeekRequestId::new(receipt.fence.request_id.value()),
                outcome: match receipt.outcome {
                    ProgressiveAsyncSeekOutcome::Succeeded(result) => {
                        PreparedDemuxSeekOutcome::Succeeded(result)
                    }
                    ProgressiveAsyncSeekOutcome::Failed => PreparedDemuxSeekOutcome::Failed,
                    ProgressiveAsyncSeekOutcome::Cancelled => PreparedDemuxSeekOutcome::Cancelled,
                    ProgressiveAsyncSeekOutcome::Superseded => PreparedDemuxSeekOutcome::Superseded,
                    ProgressiveAsyncSeekOutcome::Stale => PreparedDemuxSeekOutcome::Stale,
                },
            })
    }
}

/// Borrowed service material рядом с app-owned concrete HTTP context.
struct ProjectedDashComponent<'candidate> {
    /// Explicit role.
    role: MediaComponentRole,
    /// Proven normalized container.
    container: ContainerFamily,
    /// Service-owned authoritative input.
    material: YtDlpDashRequestMaterial<'candidate>,
    /// Concrete bounded HTTP runtime.
    http: AdaptiveHttpContext,
}

/// Проверяет transport family без открытия provider-а.
pub(crate) fn candidate_is_dash(candidate: &YtDlpNormalizedCandidate) -> bool {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.transport().family() == TransportFamily::Dash,
        StreamLayout::VideoOnly(component) => {
            component.transport().family() == TransportFamily::Dash
        }
        StreamLayout::AudioOnly(component) => {
            component.transport().family() == TransportFamily::Dash
        }
        StreamLayout::Separate { video, audio } => {
            video.transport().family() == TransportFamily::Dash
                && audio.transport().family() == TransportFamily::Dash
        }
    }
}

/// Выполняет static либо strict dynamic DASH preparation до player/queue commit barrier-а.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_dash_candidate(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
    live_intent: YtDlpLiveIntent,
    endpoint_refresh: Option<Arc<dyn DashEndpointRefreshPort>>,
    timeline_port_generation: DynamicMediaTimelinePortGeneration,
    component_selection_intent:
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    capability_probe: &crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<PreparedDashCandidate> {
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let request_context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let service_components = candidate
        .dash_transport_components(&request_context)
        .context("Не удалось спроецировать yt-dlp DASH request material")?;
    let limits = crate::web_media_adaptive_config::adaptive_transport_limits(network_config)?;
    let projected_components = service_components
        .into_iter()
        .map(|component| project_component(component, source_config, limits))
        .collect::<Result<Vec<_>>>()?;
    let selection = presentation_selection(candidate.descriptor().layout())?;
    let (http, input) = presentation_input(projected_components)?;
    if live_intent == YtDlpLiveIntent::Live {
        let DashVodHttpContext::Manifest(http) = http else {
            bail!("serialized dynamic DASH fragments исключены S35 profile");
        };
        let DashVodInput::Manifest(manifest) = input else {
            bail!("serialized dynamic DASH fragments исключены S35 profile");
        };
        let endpoint_refresh = endpoint_refresh
            .ok_or_else(|| anyhow!("DASH live candidate потерял app endpoint refresh port"))?;
        let request = DashLiveOpenRequest {
            http,
            generation,
            manifest,
            selection,
            demux_registry,
            policy: dash_policy(limits)?,
            wall_clock: Arc::new(SystemDashWallClock),
            timeline_port_generation,
            initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
            endpoint_refresh,
        };
        let (opened, component_variants) = match component_selection_intent {
            crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::ProviderDefault => (
                prepare_dash_live(request).context("DASH live preflight завершился ошибкой")?,
                crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Unavailable,
            ),
            crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
                let discovered = discover_dash_live_catalog(DashLiveCatalogDiscoveryRequest {
                    open: request,
                    catalog_identity,
                    catalog_limit: web_media_core::ComponentVariantCatalogLimit::new(256)?,
                    compatibility_edge_limit: web_media_core::ComponentVariantEdgeLimit::new(4_096)?,
                    capability_probe,
                })?;
                let catalog = Arc::new(discovered.catalog().clone());
                let selected = catalog.rematch_semantic(semantic.clone())?;
                (
                    prepare_discovered_dash_live_semantic(discovered, semantic)?,
                    crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Installed {
                        catalog,
                        provider_selection: selected,
                    },
                )
            }
        };
        let (demuxer, seek_handle, timeline_port) = opened.into_parts();
        let seek_port: Arc<dyn PreparedDemuxSeekPort> = Arc::new(DashPreparedDemuxSeekPort {
            handle: seek_handle
                .ok_or_else(|| anyhow!("DASH live runtime не опубликовал receipted seek handle"))?,
        });
        return Ok(PreparedDashCandidate {
            demuxer: Box::new(demuxer),
            seek_port,
            timeline_port: Some(timeline_port),
            component_variants,
        });
    }
    ensure_static_dash_intent(live_intent)?;
    let request = DashVodOpenRequest {
        http,
        generation,
        input,
        selection,
        demux_registry,
        policy: dash_policy(limits)?,
    };
    let (opened, component_variants) = match component_selection_intent {
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::ProviderDefault => (
            prepare_dash_vod(request).context("DASH VOD preflight завершился ошибкой")?,
            crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Unavailable,
        ),
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
            let discovered = discover_dash_vod_catalog(DashVodCatalogDiscoveryRequest {
                open: request,
                catalog_identity,
                catalog_limit: web_media_core::ComponentVariantCatalogLimit::new(256)?,
                compatibility_edge_limit: web_media_core::ComponentVariantEdgeLimit::new(4_096)?,
                capability_probe,
            })?;
            let catalog = Arc::new(discovered.catalog().clone());
            let selected = catalog.rematch_semantic(semantic.clone())?;
            (
                prepare_discovered_dash_vod_semantic(discovered, semantic)?,
                crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Installed {
                    catalog,
                    provider_selection: selected,
                },
            )
        }
    };
    let seek_port: Arc<dyn PreparedDemuxSeekPort> = Arc::new(DashPreparedDemuxSeekPort {
        handle: opened.async_seek_handle(),
    });
    Ok(PreparedDashCandidate {
        demuxer: Box::new(opened.into_demuxer()),
        seek_port,
        timeline_port: None,
        component_variants,
    })
}

/// Создаёт concrete S31 context без потери service-owned secret scopes.
fn project_component<'candidate>(
    component: YtDlpDashTransportComponent<'candidate>,
    source_config: &SourceRuntimeConfig,
    limits: AdaptiveTransportLimits,
) -> Result<ProjectedDashComponent<'candidate>> {
    let (role, container, material, request) = component.into_parts();
    let http = AdaptiveHttpContext::new(
        request,
        source_config,
        limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("non-zero DASH retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .context("DASH retry policy invalid")?,
    )
    .context("Не удалось создать DASH adaptive HTTP context")?;
    Ok(ProjectedDashComponent {
        role,
        container,
        material,
        http,
    })
}

/// Fresh endpoint refresh переиспользует тот же exact projection path.
pub(crate) fn project_dash_live_runtime_material(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    generation: web_media_transport_api::SourceGeneration,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    cancellation: CancellationToken,
) -> Result<(Box<AdaptiveHttpContext>, DashManifestInput)> {
    let request_context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let limits = crate::web_media_adaptive_config::adaptive_transport_limits(network_config)?;
    let projected = candidate
        .dash_transport_components(&request_context)?
        .into_iter()
        .map(|component| project_component(component, source_config, limits))
        .collect::<Result<Vec<_>>>()?;
    let (http, input) = presentation_input(projected)?;
    match (http, input) {
        (DashVodHttpContext::Manifest(http), DashVodInput::Manifest(manifest)) => {
            Ok((http, manifest))
        }
        _ => bail!("fresh DASH live candidate не является manifest-backed"),
    }
}

/// Строит exact MPD либо serialized input и сохраняет component-scoped contexts.
fn presentation_input(
    mut components: Vec<ProjectedDashComponent<'_>>,
) -> Result<(DashVodHttpContext, DashVodInput)> {
    match components.as_mut_slice() {
        [single] => {
            let http = single.http.clone();
            let input = component_input(single)?;
            let http = match input {
                DashVodInput::Manifest(_) => DashVodHttpContext::Manifest(Box::new(http)),
                DashVodInput::Serialized(_) => DashVodHttpContext::SerializedSingle(Box::new(http)),
            };
            Ok((http, input))
        }
        [video, audio]
            if video.role == MediaComponentRole::Video
                && audio.role == MediaComponentRole::Audio =>
        {
            match (video.material.input().kind(), audio.material.input().kind()) {
                (YtDlpDashInputKind::Manifest, YtDlpDashInputKind::Manifest) => {
                    if !video
                        .material
                        .shares_manifest_runtime_context(&audio.material)
                    {
                        bail!("separate DASH Representation имеют разные MPD/request contexts");
                    }
                    Ok((
                        DashVodHttpContext::Manifest(Box::new(video.http.clone())),
                        manifest_input(&video.material)?,
                    ))
                }
                (
                    YtDlpDashInputKind::SerializedFragments,
                    YtDlpDashInputKind::SerializedFragments,
                ) => Ok((
                    serialized_separate_http_context(&video.http, &audio.http),
                    DashVodInput::Serialized(DashSerializedPresentation::Separate {
                        video: serialized_component(video)?,
                        audio: serialized_component(audio)?,
                    }),
                )),
                _ => bail!("separate DASH components используют разные authoritative input paths"),
            }
        }
        _ => bail!("DASH candidate component roles не совпадают с layout"),
    }
}

/// Строит single input без hidden MPD fallback.
fn component_input(component: &ProjectedDashComponent<'_>) -> Result<DashVodInput> {
    match component.material.input().kind() {
        YtDlpDashInputKind::Manifest => manifest_input(&component.material),
        YtDlpDashInputKind::SerializedFragments => Ok(DashVodInput::Serialized(
            DashSerializedPresentation::Single(serialized_component(component)?),
        )),
    }
}

/// Строит manifest input с explicit XML/schema budgets.
fn manifest_input(material: &YtDlpDashRequestMaterial<'_>) -> Result<DashVodInput> {
    let manifest = material
        .input()
        .manifest_url_for_fetch()
        .ok_or_else(|| anyhow!("DASH manifest type-state потерял endpoint"))?;
    Ok(DashVodInput::Manifest(DashManifestInput {
        target: HttpRequestTarget::parse_exact(manifest)
            .context("DASH manifest endpoint нарушил HTTP target boundary")?,
        xml_budgets: dash_xml_budgets()?,
        mpd_limits: dash_mpd_limits(),
    }))
}

/// Reject live/dynamic intent до candidate mapping, HTTP context и demux preparation.
fn ensure_static_dash_intent(live_intent: YtDlpLiveIntent) -> Result<()> {
    if matches!(
        live_intent,
        YtDlpLiveIntent::Unspecified | YtDlpLiveIntent::NotLive
    ) {
        return Ok(());
    }
    bail!("dynamic DASH относится к S35 и не входит в static S34 profile")
}

/// Сохраняет независимое ownership двух serialized component contexts.
fn serialized_separate_http_context(
    video: &AdaptiveHttpContext,
    audio: &AdaptiveHttpContext,
) -> DashVodHttpContext {
    DashVodHttpContext::SerializedSeparate {
        video: Box::new(video.clone()),
        audio: Box::new(audio.clone()),
    }
}

/// Минимальный service-fragment view для pure app mapping tests.
trait SerializedDashFragmentView {
    /// Initialization/media role.
    fn role(&self) -> YtDlpDashFragmentRole;
    /// Absolute либо relative locator shape.
    fn locator_kind(&self) -> YtDlpDashFragmentLocatorKind;
    /// Locator, раскрываемый только mapping boundary.
    fn locator_for_transport(&self) -> &str;
    /// Base для relative locator-а.
    fn base_url_for_relative_resolution(&self) -> Option<&str>;
    /// Exact finite duration evidence.
    fn duration_seconds(&self) -> Option<f64>;
}

impl SerializedDashFragmentView for service_ytdlp::YtDlpDashFragment<'_> {
    fn role(&self) -> YtDlpDashFragmentRole {
        service_ytdlp::YtDlpDashFragment::role(self)
    }

    fn locator_kind(&self) -> YtDlpDashFragmentLocatorKind {
        service_ytdlp::YtDlpDashFragment::locator_kind(self)
    }

    fn locator_for_transport(&self) -> &str {
        service_ytdlp::YtDlpDashFragment::locator_for_transport(self)
    }

    fn base_url_for_relative_resolution(&self) -> Option<&str> {
        service_ytdlp::YtDlpDashFragment::base_url_for_relative_resolution(self)
    }

    fn duration_seconds(&self) -> Option<f64> {
        service_ytdlp::YtDlpDashFragment::duration_seconds(self)
    }
}

/// Переносит exact init/media roles pinned serialized shape-а.
fn serialized_component(component: &ProjectedDashComponent<'_>) -> Result<DashSerializedComponent> {
    serialized_component_from_fragments(
        component.container,
        component.role,
        component.material.input().fragments(),
    )
}

/// Pure mapping validated service fragments в runtime-owned component.
fn serialized_component_from_fragments(
    container: ContainerFamily,
    role: MediaComponentRole,
    fragments: impl IntoIterator<Item = impl SerializedDashFragmentView>,
) -> Result<DashSerializedComponent> {
    let container = dash_container(container)?;
    let media_kind = dash_media_kind(role)?;
    let fragments = fragments
        .into_iter()
        .map(|fragment| {
            let target = match fragment.locator_kind() {
                YtDlpDashFragmentLocatorKind::AbsoluteUrl => DashResourceReference::absolute(
                    HttpRequestTarget::parse_exact(fragment.locator_for_transport())
                        .context("absolute DASH fragment нарушил HTTP target boundary")?,
                ),
                YtDlpDashFragmentLocatorKind::RelativePath => {
                    let base = fragment
                        .base_url_for_relative_resolution()
                        .ok_or_else(|| anyhow!("relative DASH fragment потерял validated base"))?;
                    DashResourceReference::relative(
                        HttpRequestTarget::parse_exact(base)
                            .context("DASH fragment base нарушил HTTP target boundary")?,
                        fragment.locator_for_transport(),
                    )
                }
            };
            let duration = fragment
                .duration_seconds()
                .map(Duration::try_from_secs_f64)
                .transpose()
                .context("DASH fragment duration нельзя выразить точно")?;
            Ok(DashSerializedFragment {
                kind: match fragment.role() {
                    YtDlpDashFragmentRole::Initialization => {
                        DashSerializedFragmentKind::Initialization
                    }
                    YtDlpDashFragmentRole::Media => DashSerializedFragmentKind::Media,
                },
                target,
                byte_range: None,
                duration,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DashSerializedComponent {
        container,
        media_kind,
        fragments,
        query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
    })
}

/// Строит ambiguity-rejecting Representation evidence из normalized descriptor-а.
fn presentation_selection(layout: &StreamLayout) -> Result<DashPresentationSelection> {
    match layout {
        StreamLayout::Muxed(component) => Ok(DashPresentationSelection::Single {
            main: muxed_evidence(component)?,
        }),
        StreamLayout::VideoOnly(component) => Ok(DashPresentationSelection::Single {
            main: video_evidence(
                component
                    .container()
                    .consistent_family()
                    .map_err(|conflict| anyhow!("DASH container hints conflict: {conflict:?}"))?,
                component.video(),
            )?,
        }),
        StreamLayout::AudioOnly(component) => Ok(DashPresentationSelection::Single {
            main: audio_evidence(
                component
                    .container()
                    .consistent_family()
                    .map_err(|conflict| anyhow!("DASH container hints conflict: {conflict:?}"))?,
                component.audio(),
            )?,
        }),
        StreamLayout::Separate { video, audio } => Ok(DashPresentationSelection::Separate {
            video: video_evidence(
                video.container().consistent_family().map_err(|conflict| {
                    anyhow!("DASH video container hints conflict: {conflict:?}")
                })?,
                video.video(),
            )?,
            audio: audio_evidence(
                audio.container().consistent_family().map_err(|conflict| {
                    anyhow!("DASH audio container hints conflict: {conflict:?}")
                })?,
                audio.audio(),
            )?,
        }),
    }
}

/// Muxed evidence использует exact codec order и dimensions, но не угадывает Representation id.
fn muxed_evidence(component: &MuxedComponentDescriptor) -> Result<DashRepresentationEvidence> {
    let mut evidence = video_evidence(
        component
            .container()
            .consistent_family()
            .map_err(|conflict| anyhow!("DASH muxed container hints conflict: {conflict:?}"))?,
        component.video(),
    )?;
    evidence.media_kind = DashMediaKind::Muxed;
    evidence.codecs = Some(format!(
        "{},{}",
        component.video().codec().raw().as_str(),
        component.audio().codec().raw().as_str()
    ));
    Ok(evidence)
}

/// Video evidence остаётся exact и допускает typed ambiguity rejection runtime-а.
fn video_evidence(
    container: Option<ContainerFamily>,
    track: &VideoTrackDescriptor,
) -> Result<DashRepresentationEvidence> {
    let dimensions = match (track.width_pixels(), track.height()) {
        (Some(width), Some(height)) => Some(DashVideoDimensions {
            width,
            height: height.pixels(),
        }),
        (None, None) => None,
        _ => bail!("DASH video dimensions evidence неполна"),
    };
    Ok(DashRepresentationEvidence {
        media_kind: DashMediaKind::Video,
        container: dash_container(
            container.ok_or_else(|| anyhow!("DASH container evidence отсутствует"))?,
        )?,
        representation_id: None,
        codecs: Some(track.codec().raw().as_str().to_owned()),
        bandwidth: None,
        dimensions,
    })
}

/// Audio evidence не делает предположений о Representation id/bandwidth.
fn audio_evidence(
    container: Option<ContainerFamily>,
    track: &AudioTrackDescriptor,
) -> Result<DashRepresentationEvidence> {
    Ok(DashRepresentationEvidence {
        media_kind: DashMediaKind::Audio,
        container: dash_container(
            container.ok_or_else(|| anyhow!("DASH container evidence отсутствует"))?,
        )?,
        representation_id: None,
        codecs: Some(track.codec().raw().as_str().to_owned()),
        bandwidth: None,
        dimensions: None,
    })
}

/// Отображает только proven S28A/S28B containers.
fn dash_container(container: ContainerFamily) -> Result<DashContainer> {
    match container {
        ContainerFamily::IsoBmff | ContainerFamily::FragmentedIsoBmff => Ok(DashContainer::IsoBmff),
        ContainerFamily::WebM => Ok(DashContainer::WebM),
        other => bail!("DASH container {other:?} не входит в fMP4/WebM profile"),
    }
}

/// Отображает explicit service role без positional inference.
fn dash_media_kind(role: MediaComponentRole) -> Result<DashMediaKind> {
    match role {
        MediaComponentRole::Muxed => Ok(DashMediaKind::Muxed),
        MediaComponentRole::Video => Ok(DashMediaKind::Video),
        MediaComponentRole::Audio => Ok(DashMediaKind::Audio),
        MediaComponentRole::Subtitle => bail!("DASH subtitles playback не входит в S34"),
    }
}

/// Обязательные S04X budgets задаются composition owner-ом.
fn dash_xml_budgets() -> Result<XmlBudgets> {
    XmlBudgets::builder()
        .maximum_document_bytes(2 * 1_024 * 1_024)
        .maximum_depth(48)
        .maximum_tokens(65_536)
        .maximum_attributes_per_element(64)
        .maximum_attribute_count(65_536)
        .maximum_attribute_bytes(512 * 1_024)
        .maximum_namespace_declarations_per_element(16)
        .maximum_namespace_declaration_count(1_024)
        .maximum_namespace_bytes(64 * 1_024)
        .maximum_text_bytes(512 * 1_024)
        .build()
        .context("DASH XML budgets invalid")
}

/// Bounded static MPD profile limits.
const fn dash_mpd_limits() -> DashMpdLimits {
    DashMpdLimits {
        maximum_periods: 64,
        maximum_adaptation_sets_per_period: 64,
        maximum_representations_per_adaptation_set: 256,
        maximum_segments_per_list: 8_192,
        maximum_timeline_entries: 8_192,
        maximum_schema_string_bytes: 8_192,
    }
}

/// Runtime queue/range/scan policy использует app-owned network budget.
fn dash_policy(limits: AdaptiveTransportLimits) -> Result<DashVodOpenPolicy> {
    Ok(DashVodOpenPolicy {
        maximum_manifest_bytes: limits.maximum_manifest_bytes,
        maximum_fragment_bytes: limits.maximum_segment_bytes,
        maximum_range_read_bytes: limits.maximum_segment_bytes,
        maximum_planned_segments: limits.maximum_snapshot_segments,
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(64 * 1_024).expect("DASH sniff bytes"),
            NonZeroUsize::new(8).expect("DASH sniff segments"),
            Duration::from_secs(2),
        )?,
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("DASH event queue"),
            NonZeroUsize::new(16 * 1_024 * 1_024).expect("DASH encoded queue"),
        ),
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(16).expect("DASH outstanding seek receipts"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(10))?,
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(3),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("DASH composite packet"),
        )?,
        maximum_seek_scan_events: NonZeroUsize::new(65_536).expect("DASH seek scan events"),
        maximum_seek_scan_bytes: limits.maximum_segment_bytes,
    })
}

/// Сохраняет typed enqueue categories player boundary-а.
const fn map_enqueue_error(
    error: ProgressiveAsyncSeekEnqueueError,
) -> PreparedDemuxSeekEnqueueError {
    match error {
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull => {
            PreparedDemuxSeekEnqueueError::ReceiptQueueFull
        }
        ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity => {
            PreparedDemuxSeekEnqueueError::NonMonotonicRequestIdentity
        }
        ProgressiveAsyncSeekEnqueueError::WorkerStopped => {
            PreparedDemuxSeekEnqueueError::WorkerStopped
        }
        ProgressiveAsyncSeekEnqueueError::CapabilityAbsent => {
            PreparedDemuxSeekEnqueueError::CapabilityUnavailable
        }
    }
}

#[cfg(test)]
mod tests;
