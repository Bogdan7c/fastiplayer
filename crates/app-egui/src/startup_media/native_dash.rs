//! App-owned direct static DASH admission поверх existing S34 data plane.

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use demux_api::DemuxRegistry;
use media_core::Demuxer;
use player_core::PreparedDemuxSeekPort;
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, WebMediaConfig};
use source_core::{CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportError,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantEdgeLimit,
    ExactSelectionIdentity, ExtractionGeneration, SemanticIdentity, WebMediaSelection,
    WebMediaSelectionRematchSource, WebMediaSelectionShape, WebMediaSemanticSelectionRequest,
};
use web_media_dash::{
    DashClockFetchObservation, DashFetchedLiveManifestInput, DashFetchedManifestInput,
    DashFetchedPresentationKind, DashVodCatalogDiscoveryError, DashVodOpenError, DashWallClock,
    NativeDashVodCatalogDiscoveryRequest, classify_fetched_dash_presentation,
    discover_native_dash_vod_catalog, prepare_discovered_dash_vod,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::{NativeDashSourceState, NativeDashUrl};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};

use super::orchestration::PreparedStartupMedia;

mod live_refresh;
mod live_runtime;

/// Fresh direct snapshots сохраняют source lineage, но не exact generation.
static NEXT_NATIVE_DASH_SNAPSHOT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Public native MPD redirects используют тот же bounded hop count, что native HLS.
const NATIVE_DASH_REDIRECT_HOPS: u8 = 5;

/// Единственные typed причины допустимого pre-Installed extractor fallback-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeDashFallbackReason {
    /// URL с `.mpd` вернул документ, который parser доказал как не-MPD.
    StrictlyNotDash,
    /// Authoritative сервер требует extractor-owned authorization material.
    AuthorizationRequired,
    /// Валидный MPD использует пока не admitted native profile.
    UnsupportedNativeProfile,
}

/// Результат content-based native DASH admission до strong install barrier-а.
pub(crate) enum NativeDashAttempt<Prepared> {
    /// Static MPD полностью подготовлен существующим DASH runtime-ом.
    Prepared(Prepared),
    /// Только initial request может передать source extractor adapter-у.
    RequiresYtDlpFallback(NativeDashFallbackReason),
}

/// Все production inputs одной native static DASH attempt.
pub(crate) struct NativeDashPreparationRequest<'request> {
    /// Stable app-owned MPD root.
    pub(crate) source: &'request NativeDashUrl,
    /// Installed semantic selection для switch/reopen/root refresh.
    pub(crate) expected_selection: Option<&'request WebMediaSemanticSelectionRequest>,
    /// Network budgets/retry/source policy.
    pub(crate) network_config: &'request NetworkConfig,
    /// Preferred-height policy и neutral stream projection preference.
    pub(crate) web_media_config: &'request WebMediaConfig,
    /// Existing Symphonia corruption limits.
    pub(crate) demux_config: &'request PlayerDemuxConfig,
    /// Actual video decoder capability snapshot.
    pub(crate) system_capabilities: &'request capability_core::SystemCapabilities,
    /// Actual audio decoder capability snapshot.
    pub(crate) audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    /// Cooperative cancellation одной physical attempt.
    pub(crate) cancellation: CancellationToken,
}

/// Ready native static DASH media и provider-neutral lifecycle state.
pub(crate) struct PreparedNativeDashMedia {
    /// Existing DASH progressive/composite demux runtime.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Worker-receipted VOD seek boundary.
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Stable root + neutral catalog selection projection.
    pub(crate) source_state: NativeDashSourceState,
    /// VOD recovery и live timeline нельзя случайно установить одновременно.
    pub(crate) lifecycle: PreparedNativeDashLifecycle,
}

/// Provider lifecycle attachments остаются взаимоисключающими до strong barrier-а.
pub(crate) enum PreparedNativeDashLifecycle {
    /// Static VOD arm-ит только endpoint recovery.
    Vod {
        endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
    },
    /// Dynamic live публикует только S31L timeline port.
    Live {
        timeline_port: media_core::DynamicMediaTimelinePort,
    },
}

/// Готовые neutral attachments для единого app composition boundary.
pub(crate) struct PreparedNativeDashWebAttachments {
    /// Exact installed presentation kind.
    pub(crate) presentation: web_media_core::WebMediaPresentationKind,
    /// Mutually-compatible player preparation attachments.
    pub(crate) prepared: crate::media_open::PreparedWebMediaAttachments,
    /// VOD-only recovery arm; live всегда возвращает `None`.
    pub(crate) vod_endpoint_recovery:
        Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

impl PreparedNativeDashLifecycle {
    /// Проецирует provider lifecycle в neutral app vocabulary.
    pub(crate) const fn presentation(&self) -> web_media_core::WebMediaPresentationKind {
        match self {
            Self::Vod { .. } => web_media_core::WebMediaPresentationKind::Vod,
            Self::Live { .. } => web_media_core::WebMediaPresentationKind::Live,
        }
    }

    /// Собирает seek/timeline/recovery attachments без недопустимых сочетаний.
    pub(crate) fn into_web_attachments(
        self,
        seek_port: Arc<dyn PreparedDemuxSeekPort>,
    ) -> PreparedNativeDashWebAttachments {
        let presentation = self.presentation();
        match self {
            Self::Vod { endpoint_recovery } => PreparedNativeDashWebAttachments {
                presentation,
                prepared: crate::media_open::PreparedWebMediaAttachments {
                    demux_seek: Some(
                        crate::media_open::PreparedWebMediaSeekAttachment::WorkerReceipted(
                            seek_port,
                        ),
                    ),
                    ..crate::media_open::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: Some(endpoint_recovery),
            },
            Self::Live { timeline_port } => PreparedNativeDashWebAttachments {
                presentation,
                prepared: crate::media_open::PreparedWebMediaAttachments {
                    timeline_port: Some(timeline_port),
                    demux_seek: Some(
                        crate::media_open::PreparedWebMediaSeekAttachment::WorkerReceipted(
                            seek_port,
                        ),
                    ),
                    ..crate::media_open::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: None,
            },
        }
    }
}

/// Готовит direct static/dynamic MPD, не создавая второй parser/transport/runtime.
pub(crate) fn prepare_native_dash_attempt(
    request: NativeDashPreparationRequest<'_>,
) -> Result<NativeDashAttempt<PreparedNativeDashMedia>> {
    if request.cancellation.is_cancelled() {
        return Err(anyhow!("native DASH admission cancelled"));
    }

    let snapshot_identity = fresh_snapshot_identity(request.source)?;
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(request.network_config)?;
    // До authoritative `type` transport context используется только для единственного root fetch-а.
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

    // Root response читается ровно один раз; parser/catalog получают owned fetched handoff.
    let wall_clock: Arc<dyn DashWallClock> =
        Arc::new(crate::web_media_dash_open::SystemDashWallClock);
    let fetch_started = Instant::now();
    let local_before_fetch = wall_clock.now_utc();
    let fetched_manifest =
        match admission_http.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            generation,
            request.source.target().clone(),
            adaptive_limits.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        )) {
            Ok(fetched_manifest) => fetched_manifest,
            Err(error) if matches!(error.http_status_code(), Some(401 | 403)) => {
                return Ok(NativeDashAttempt::RequiresYtDlpFallback(
                    NativeDashFallbackReason::AuthorizationRequired,
                ));
            }
            Err(AdaptiveTransportError::Cancelled) => {
                return Err(anyhow!("native DASH root fetch cancelled"));
            }
            Err(error) => return Err(error).context("native DASH root fetch"),
        };
    let local_after_fetch = wall_clock.now_utc();
    let manifest = DashFetchedManifestInput::new(
        request.source.target().clone(),
        fetched_manifest,
        &admission_http,
        crate::web_media_dash_open::dash_xml_budgets()?,
        crate::web_media_dash_open::dash_mpd_limits(),
    );
    let policy = crate::web_media_dash_open::dash_policy(adaptive_limits)?;
    let presentation =
        match classify_fetched_dash_presentation(&admission_http, generation, &manifest, policy) {
            Ok(presentation) => presentation,
            Err(error) => match native_open_fallback_reason(&error) {
                Some(reason) => return Ok(NativeDashAttempt::RequiresYtDlpFallback(reason)),
                None => return Err(error).context("native DASH presentation classification"),
            },
        };
    let demux_registry = native_dash_demux_registry(request.demux_config)?;

    if presentation == DashFetchedPresentationKind::Live {
        let live_transport_request = native_transport_request(
            &snapshot_identity.parent,
            request.source,
            MediaPresentation::Live,
            generation,
            request.cancellation.clone(),
        )?;
        let live_http = native_adaptive_http_context(
            live_transport_request,
            request.network_config,
            adaptive_limits,
        )?;
        let prepared =
            match live_runtime::prepare_native_dash_live(live_runtime::NativeDashLivePreparation {
                request: &request,
                snapshot_identity,
                http: live_http,
                generation,
                manifest: DashFetchedLiveManifestInput::new(
                    manifest,
                    fetch_started,
                    DashClockFetchObservation::new(local_before_fetch, local_after_fetch),
                ),
                demux_registry,
            }) {
                Ok(prepared) => prepared,
                Err(error) if error.is_profile_exclusion() => {
                    return Ok(NativeDashAttempt::RequiresYtDlpFallback(
                        NativeDashFallbackReason::UnsupportedNativeProfile,
                    ));
                }
                Err(error) => return Err(anyhow::Error::new(error)),
            };
        return Ok(NativeDashAttempt::Prepared(prepared));
    }

    let vod_endpoint_recovery = crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new();
    let vod_transport_request = native_transport_request(
        &snapshot_identity.parent,
        request.source,
        MediaPresentation::Vod,
        generation,
        request.cancellation.clone(),
    )?
    .with_endpoint_expiry_observer(vod_endpoint_recovery.observer());
    let vod_http = native_adaptive_http_context(
        vod_transport_request,
        request.network_config,
        adaptive_limits,
    )?;
    let capability_probe =
        crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe::new(
            request.system_capabilities.clone(),
            request.audio_capabilities,
        );
    let discovered = match discover_native_dash_vod_catalog(NativeDashVodCatalogDiscoveryRequest {
        http: Box::new(vod_http),
        generation,
        manifest,
        demux_registry,
        policy,
        catalog_identity: snapshot_identity.catalog,
        catalog_limit: ComponentVariantCatalogLimit::new(256)?,
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(4_096)?,
        capability_probe: &capability_probe,
        preferred_height: crate::web_media_quality::preferred_height_policy(
            request.web_media_config.preferred_video_height,
        ),
    }) {
        Ok(discovered) => discovered,
        Err(error) => match native_fallback_reason(&error) {
            Some(reason) => return Ok(NativeDashAttempt::RequiresYtDlpFallback(reason)),
            None => return Err(error).context("native DASH static catalog discovery"),
        },
    };

    let component_catalog = Arc::new(discovered.catalog().clone());
    let neutral_selection = match request.expected_selection {
        Some(expected) => expected
            .rematch(
                snapshot_identity.parent.clone(),
                WebMediaSelectionRematchSource::ComponentCatalog(&component_catalog),
            )
            .context("native DASH semantic selection rematch failed")?,
        None => WebMediaSelection::with_components(
            snapshot_identity.parent.clone(),
            discovered.provider_default().clone(),
        )
        .context("native DASH provider default нарушил catalog parent identity")?,
    };
    let WebMediaSelectionShape::Components(component_selection) = neutral_selection.shape() else {
        return Err(anyhow!(
            "native DASH selection потерял component catalog shape"
        ));
    };
    let opened = prepare_discovered_dash_vod(discovered, component_selection.clone())
        .context("native DASH exact discovered selection open failed")?;
    let seek_port = crate::web_media_dash_open::prepared_dash_seek_port(opened.async_seek_handle());
    let source_state = NativeDashSourceState::new(
        neutral_selection,
        component_catalog,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            request.web_media_config,
        ),
    )
    .context("native DASH neutral catalog projection failed")?;

    Ok(NativeDashAttempt::Prepared(PreparedNativeDashMedia {
        demuxer: Box::new(opened.into_demuxer()),
        seek_port,
        source_state,
        lifecycle: PreparedNativeDashLifecycle::Vod {
            endpoint_recovery: vod_endpoint_recovery,
        },
    }))
}

/// Только parser-owned content/profile categories могут открыть fallback gate.
fn native_fallback_reason(
    error: &DashVodCatalogDiscoveryError,
) -> Option<NativeDashFallbackReason> {
    let DashVodCatalogDiscoveryError::Open(DashVodOpenError::Manifest(error)) = error else {
        return None;
    };
    native_manifest_fallback_reason(error)
}

/// Classification сохраняет те же initial-only fallback categories, что VOD discovery.
fn native_open_fallback_reason(error: &DashVodOpenError) -> Option<NativeDashFallbackReason> {
    let DashVodOpenError::Manifest(error) = error else {
        return None;
    };
    native_manifest_fallback_reason(error)
}

/// Parser-owned категории не смешиваются с transport/cancellation/runtime failures.
fn native_manifest_fallback_reason(
    error: &dash_mpd_core::DashMpdError,
) -> Option<NativeDashFallbackReason> {
    match error.kind() {
        dash_mpd_core::DashMpdErrorKind::InvalidRoot => {
            Some(NativeDashFallbackReason::StrictlyNotDash)
        }
        dash_mpd_core::DashMpdErrorKind::DynamicPresentation
        | dash_mpd_core::DashMpdErrorKind::UnsupportedProfile
        | dash_mpd_core::DashMpdErrorKind::UnsupportedAvailabilityOffset
        | dash_mpd_core::DashMpdErrorKind::UnsupportedConstruct
        | dash_mpd_core::DashMpdErrorKind::ContentProtection
        | dash_mpd_core::DashMpdErrorKind::UnsupportedMediaEvidence => {
            Some(NativeDashFallbackReason::UnsupportedNativeProfile)
        }
        dash_mpd_core::DashMpdErrorKind::Xml
        | dash_mpd_core::DashMpdErrorKind::InvalidAttribute
        | dash_mpd_core::DashMpdErrorKind::MultipleBaseUrls
        | dash_mpd_core::DashMpdErrorKind::LimitExceeded
        | dash_mpd_core::DashMpdErrorKind::InvalidAddressing
        | dash_mpd_core::DashMpdErrorKind::InvalidPeriodTimeline
        | dash_mpd_core::DashMpdErrorKind::MalformedSchema => None,
    }
}

/// Создаёт fresh exact parent/catalog identity без URL/hash material.
fn fresh_snapshot_identity(source: &NativeDashUrl) -> Result<NativeDashSnapshotIdentity> {
    let generation = NEXT_NATIVE_DASH_SNAPSHOT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .max(1);
    let source_identity = source.source_identity();
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source_identity,
            ExtractionGeneration::new(generation),
            CandidateFormatIdentity::new("native-dash-vod")?,
        ),
        SemanticIdentity::new(source_identity, "native-dash-vod")?,
    )?;
    let catalog = ComponentVariantCatalogIdentity::new(
        parent.clone(),
        ComponentVariantCatalogGeneration::new(generation),
    );
    Ok(NativeDashSnapshotIdentity { parent, catalog })
}

/// Fresh parent и catalog получают одну generation и stable source lineage.
pub(super) struct NativeDashSnapshotIdentity {
    /// Exact parent текущей open attempt.
    pub(super) parent: ExactSelectionIdentity,
    /// Exact component catalog generation текущей open attempt.
    pub(super) catalog: ComponentVariantCatalogIdentity,
}

/// Собирает public HTTP request с real scope proof и без retained secrets.
pub(super) fn native_transport_request(
    parent: &ExactSelectionIdentity,
    source: &NativeDashUrl,
    presentation: MediaPresentation,
    generation: SourceGeneration,
    cancellation: CancellationToken,
) -> Result<TransportOpenRequest> {
    let component = MediaComponentIdentity::new(
        parent.exact().clone(),
        parent.semantic().clone(),
        MediaComponentRole::PresentationManifest,
    )?;
    let initial_target = source.target().clone();
    let request_context = native_public_request_context(&initial_target);
    Ok(TransportOpenRequest::new(
        TransportProviderId::new("native-dash-http")?,
        component,
        initial_target,
        presentation,
        generation,
        request_context,
        RedirectPolicy::cross_origin_without_secrets(RedirectHopLimit::new(
            NATIVE_DASH_REDIRECT_HOPS,
        )?),
        cancellation,
    )?)
}

/// Создаёт bounded adaptive context для root и Representation resources.
pub(super) fn native_adaptive_http_context(
    transport_request: TransportOpenRequest,
    network_config: &NetworkConfig,
    adaptive_limits: web_media_adaptive::AdaptiveTransportLimits,
) -> Result<AdaptiveHttpContext> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("native DASH source config")?;
    AdaptiveHttpContext::new(
        transport_request,
        &source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("native DASH retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )
    .map_err(anyhow::Error::new)
}

/// Строит empty-bytes secret context с корректным HTTP origin/path scope.
fn native_public_request_context(initial_target: &HttpRequestTarget) -> SecretRequestContext {
    let path_scope = HttpPathScope::from_target_path(initial_target);
    SecretRequestContext::builder(SecretRequestScope::from_target(initial_target, path_scope))
        .build()
}

/// Переиспользует existing Symphonia fMP4/WebM registrations.
pub(super) fn native_dash_demux_registry(
    demux_config: &PlayerDemuxConfig,
) -> Result<Arc<DemuxRegistry>> {
    let options = DemuxerOptions::from_max_consecutive_corrupted_packets(
        demux_config.max_consecutive_corrupted_packets,
    )
    .context("native DASH demux corruption limit must be non-zero")?;
    let composition = crate::web_media_demux_registry::WebDemuxComposition::new(options)?;
    Ok(Arc::new(composition.registry))
}

/// Результат одного sequential native-admission/extractor-fallback startup job-а.
type NativeDashStartupResult = std::result::Result<PreparedStartupMedia, String>;

/// Фоновый CLI job сначала завершает native content admission и только затем решает fallback.
pub(super) struct NativeDashStartupJob {
    /// Bounded pending label для startup overlay.
    pending_message: String,
    /// Exactly-once completion mailbox фонового resolver-а.
    result_receiver: OwnerMailboxReceiver<(), NativeDashStartupResult>,
    /// JoinHandle нужен bounded shutdown owner-у.
    pub(super) join_handle: Option<JoinHandle<()>>,
    /// Result удерживается до physical worker exit.
    pending_result: Option<NativeDashStartupResult>,
    /// Cooperative publication fence.
    pub(super) cancellation_requested: Arc<AtomicBool>,
    /// Тот же token физически отменяет HTTP/DASH work.
    pub(super) source_cancellation: CancellationToken,
}

impl NativeDashStartupJob {
    /// Запускает одну mutually-exclusive direct MPD attempt.
    pub(super) fn spawn(
        source: NativeDashUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_config: rustiplayer_config::AppConfig,
        system_capabilities: capability_core::SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let source_cancellation = CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("native-dash-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_dash_startup_media(
                    source,
                    fallback_locator,
                    &app_config,
                    &system_capabilities,
                    audio_capabilities,
                    worker_source_cancellation,
                    || worker_cancellation_requested.load(Ordering::Acquire),
                )
                .map_err(|error| format!("{error:#}"));
                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }
                match result_publisher.publish_completion(result) {
                    Ok(WakeDelivery::EventLoopClosed) => tracing::debug!(
                        "Event loop закрыт; native DASH terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native DASH startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| format!("Не удалось запустить native DASH startup opener: {error}"))?;
        Ok(Self {
            pending_message: "Проверка native DASH...".to_owned(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
            source_cancellation,
        })
    }

    /// Возвращает safe pending label без locator material.
    pub(super) fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Публикует result только после exact worker join.
    pub(super) fn try_take_result(&mut self) -> Option<NativeDashStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Native DASH startup opener завершился без результата".to_owned())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err("Native DASH startup opener завершился panic".to_owned()))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Выполняет native content admission и единственный typed extractor fallback.
fn resolve_native_dash_startup_media(
    source: NativeDashUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    app_config: &rustiplayer_config::AppConfig,
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    match prepare_native_dash_attempt(NativeDashPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        system_capabilities,
        audio_capabilities,
        cancellation: cancellation.clone(),
    })? {
        NativeDashAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeDash {
            source,
            prepared: Box::new(prepared),
        }),
        NativeDashAttempt::RequiresYtDlpFallback(reason) => {
            if !app_config.yt_dlp.enabled {
                return Err(anyhow!(
                    "native DASH admission requires extractor fallback ({reason:?}), но YtDlp отключён"
                ));
            }
            tracing::info!(
                ?reason,
                "CLI native DASH admission передан единственному YtDlp fallback"
            );
            let prepared = super::resolve_yt_dlp_startup_media(
                &fallback_locator,
                app_config,
                system_capabilities,
                audio_capabilities,
                cancellation,
                is_cancelled,
            )?;
            Ok(PreparedStartupMedia::Extractor {
                source_locator: fallback_locator,
                prepared: Box::new(prepared),
            })
        }
    }
}

impl super::StartupMediaController {
    /// Запускает один sequential CLI direct MPD job без параллельного extractor probe-а.
    pub(crate) fn start_native_dash_startup_job(
        &mut self,
        source: NativeDashUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut crate::state::AppState,
        app_config: &rustiplayer_config::AppConfig,
        system_capabilities: &capability_core::SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Проверка native DASH...".to_owned());
        match NativeDashStartupJob::spawn(
            source,
            fallback_locator,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_dash_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native DASH startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}
