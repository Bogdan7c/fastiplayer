//! App-owned direct Smooth VOD admission поверх existing S36 data plane.

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use media_core::Demuxer;
use player_core::PreparedDemuxSeekPort;
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, WebMediaConfig};
use source_core::{CancellationToken, HttpPathScope, SourceRuntimeConfig};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportError,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ExactSelectionIdentity, ExtractionGeneration,
    SemanticIdentity, WebMediaSemanticSelectionRequest,
};
use web_media_smooth::{SmoothFetchedManifestInput, SmoothPrepareError};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::{NativeSmoothSourceState, NativeSmoothUrl};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::web_media_open::{NativeSmoothCandidatePreparation, prepare_native_smooth_candidate};

use super::orchestration::PreparedStartupMedia;

/// Fresh direct snapshots сохраняют source lineage, но меняют exact generation.
static NEXT_NATIVE_SMOOTH_SNAPSHOT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Direct Smooth redirect budget совпадает с другими native manifest ingress-ами.
const NATIVE_SMOOTH_REDIRECT_HOPS: u8 = 5;

/// Единственные причины допустимого initial pre-Installed extractor fallback-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeSmoothFallbackReason {
    /// `/Manifest` hint вернул well-formed document с чужим root element-ом.
    StrictlyNotSmooth,
    /// Authoritative сервер требует extractor-owned authorization material.
    AuthorizationRequired,
}

/// Результат content-based native Smooth admission до strong install barrier-а.
pub(crate) enum NativeSmoothAttempt<Prepared> {
    /// Static H.264/AAC Smooth VOD полностью подготовлен existing runtime-ом.
    Prepared(Prepared),
    /// Только initial request может передать source extractor adapter-у.
    RequiresYtDlpFallback(NativeSmoothFallbackReason),
}

/// Все production inputs одной native Smooth attempt.
pub(crate) struct NativeSmoothPreparationRequest<'request> {
    /// Stable app-owned `/Manifest` root.
    pub(crate) source: &'request NativeSmoothUrl,
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

/// Ready native Smooth VOD и provider-neutral lifecycle state.
pub(crate) struct PreparedNativeSmoothMedia {
    /// Existing S36 composite demux runtime.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Worker-receipted transactional VOD seek boundary.
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Stable root + neutral catalog selection projection.
    pub(crate) source_state: NativeSmoothSourceState,
    /// VOD endpoint expiry owner arm-ится только после Installed.
    pub(crate) endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
}

/// Fresh parent и catalog получают одну generation и stable source lineage.
struct NativeSmoothSnapshotIdentity {
    /// Exact parent текущей physical attempt.
    parent: ExactSelectionIdentity,
    /// Exact component catalog generation текущей attempt.
    catalog: ComponentVariantCatalogIdentity,
}

/// Готовит direct static Smooth VOD, не создавая parser/transport/runtime дубль.
pub(crate) fn prepare_native_smooth_attempt(
    request: NativeSmoothPreparationRequest<'_>,
) -> Result<NativeSmoothAttempt<PreparedNativeSmoothMedia>> {
    if request.cancellation.is_cancelled() {
        return Err(anyhow!("native Smooth admission cancelled"));
    }

    let snapshot_identity = fresh_snapshot_identity(request.source)?;
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(request.network_config)?;
    let endpoint_recovery = crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new();
    let transport = native_transport_request(
        &snapshot_identity.parent,
        request.source,
        generation,
        request.cancellation.clone(),
    )?
    .with_endpoint_expiry_observer(endpoint_recovery.observer());
    let source_config = SourceRuntimeConfig::from_network_config(request.network_config)
        .context("native Smooth source config")?;
    let http = native_adaptive_http_context(transport.clone(), &source_config, adaptive_limits)?;

    // Единственный root GET одновременно служит content admission и runtime preparation.
    let fetched_manifest = match http.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        generation,
        request.source.target().clone(),
        adaptive_limits.maximum_manifest_bytes,
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )) {
        Ok(fetched_manifest) => fetched_manifest,
        Err(error) if matches!(error.http_status_code(), Some(401 | 403)) => {
            return Ok(NativeSmoothAttempt::RequiresYtDlpFallback(
                NativeSmoothFallbackReason::AuthorizationRequired,
            ));
        }
        Err(AdaptiveTransportError::Cancelled) => {
            return Err(anyhow!("native Smooth root fetch cancelled"));
        }
        Err(error) => return Err(error).context("native Smooth root fetch"),
    };

    let demux_registry = super::native_dash::native_dash_demux_registry(request.demux_config)?;
    let capability_probe =
        crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe::new(
            request.system_capabilities.clone(),
            request.audio_capabilities,
        );
    let prepared = match prepare_native_smooth_candidate(NativeSmoothCandidatePreparation {
        transport,
        fetched_manifest: SmoothFetchedManifestInput::new(
            request.source.target().clone(),
            http,
            fetched_manifest,
        ),
        source_config: &source_config,
        network_config: request.network_config,
        demux_registry,
        catalog_identity: snapshot_identity.catalog,
        fresh_parent: snapshot_identity.parent,
        capability_probe: &capability_probe,
        preferred_height: crate::web_media_quality::preferred_height_policy(
            request.web_media_config.preferred_video_height,
        ),
        expected_selection: request.expected_selection,
    }) {
        Ok(prepared) => prepared,
        Err(error) if native_fallback_reason(&error).is_some() => {
            return Ok(NativeSmoothAttempt::RequiresYtDlpFallback(
                NativeSmoothFallbackReason::StrictlyNotSmooth,
            ));
        }
        Err(error) => return Err(error),
    };
    let source_state = NativeSmoothSourceState::new(
        prepared.neutral_selection,
        prepared.component_catalog,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            request.web_media_config,
        ),
    )
    .context("native Smooth neutral catalog projection failed")?;

    Ok(NativeSmoothAttempt::Prepared(PreparedNativeSmoothMedia {
        demuxer: prepared.demuxer,
        seek_port: prepared.seek_port,
        source_state,
        endpoint_recovery,
    }))
}

/// Только parser-owned invalid root открывает page-extractor fallback gate.
fn native_fallback_reason(error: &anyhow::Error) -> Option<NativeSmoothFallbackReason> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<SmoothPrepareError>()
            .and_then(|error| {
                error
                    .is_invalid_root()
                    .then_some(NativeSmoothFallbackReason::StrictlyNotSmooth)
            })
    })
}

/// Создаёт fresh exact parent/catalog identity без URL/hash material.
fn fresh_snapshot_identity(source: &NativeSmoothUrl) -> Result<NativeSmoothSnapshotIdentity> {
    let generation = NEXT_NATIVE_SMOOTH_SNAPSHOT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .max(1);
    let source_identity = source.source_identity();
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source_identity,
            ExtractionGeneration::new(generation),
            CandidateFormatIdentity::new("native-smooth-vod")?,
        ),
        SemanticIdentity::new(source_identity, "native-smooth-vod")?,
    )?;
    let catalog = ComponentVariantCatalogIdentity::new(
        parent.clone(),
        ComponentVariantCatalogGeneration::new(generation),
    );
    Ok(NativeSmoothSnapshotIdentity { parent, catalog })
}

/// Собирает public HTTP request с реальным origin/path scope и без raw secrets.
fn native_transport_request(
    parent: &ExactSelectionIdentity,
    source: &NativeSmoothUrl,
    generation: SourceGeneration,
    cancellation: CancellationToken,
) -> Result<TransportOpenRequest> {
    let component = MediaComponentIdentity::new(
        parent.exact().clone(),
        parent.semantic().clone(),
        MediaComponentRole::PresentationManifest,
    )?;
    let initial_target = source.target().clone();
    let path_scope = HttpPathScope::from_target_path(&initial_target);
    let request_context =
        SecretRequestContext::builder(SecretRequestScope::from_target(&initial_target, path_scope))
            .build();
    Ok(TransportOpenRequest::new(
        TransportProviderId::new("native-smooth-http")?,
        component,
        initial_target,
        MediaPresentation::Vod,
        generation,
        request_context,
        RedirectPolicy::cross_origin_without_secrets(RedirectHopLimit::new(
            NATIVE_SMOOTH_REDIRECT_HOPS,
        )?),
        cancellation,
    )?)
}

/// Создаёт единственный adaptive context для root и fragment resources.
fn native_adaptive_http_context(
    transport: TransportOpenRequest,
    source_config: &SourceRuntimeConfig,
    adaptive_limits: web_media_adaptive::AdaptiveTransportLimits,
) -> Result<AdaptiveHttpContext> {
    AdaptiveHttpContext::new(
        transport,
        source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("native Smooth retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )
    .map_err(anyhow::Error::new)
}

/// Результат одного sequential native-admission/extractor-fallback startup job-а.
type NativeSmoothStartupResult = std::result::Result<PreparedStartupMedia, String>;

/// Фоновый startup job сначала завершает native admission и только потом решает fallback.
pub(super) struct NativeSmoothStartupJob {
    /// Bounded pending label для startup overlay.
    pending_message: String,
    /// Exactly-once completion mailbox фонового resolver-а.
    result_receiver: OwnerMailboxReceiver<(), NativeSmoothStartupResult>,
    /// JoinHandle нужен bounded shutdown owner-у.
    pub(super) join_handle: Option<JoinHandle<()>>,
    /// Result удерживается до physical worker exit.
    pending_result: Option<NativeSmoothStartupResult>,
    /// Cooperative publication fence.
    pub(super) cancellation_requested: Arc<AtomicBool>,
    /// Тот же token физически отменяет HTTP/Smooth work.
    pub(super) source_cancellation: CancellationToken,
}

impl NativeSmoothStartupJob {
    /// Запускает одну mutually-exclusive direct `/Manifest` attempt.
    pub(super) fn spawn(
        source: NativeSmoothUrl,
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
            .name("native-smooth-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_smooth_startup_media(
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
                        "Event loop закрыт; native Smooth terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native Smooth startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| {
                format!("Не удалось запустить native Smooth startup opener: {error}")
            })?;
        Ok(Self {
            pending_message: "Проверка native Smooth...".to_owned(),
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
    pub(super) fn try_take_result(&mut self) -> Option<NativeSmoothStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Native Smooth startup opener завершился без результата".to_owned())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err(
                    "Native Smooth startup opener завершился panic".to_owned()
                ))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Выполняет native admission и единственный typed extractor fallback.
fn resolve_native_smooth_startup_media(
    source: NativeSmoothUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    app_config: &rustiplayer_config::AppConfig,
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    match prepare_native_smooth_attempt(NativeSmoothPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        system_capabilities,
        audio_capabilities,
        cancellation: cancellation.clone(),
    })? {
        NativeSmoothAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeSmooth {
            source,
            prepared: Box::new(prepared),
        }),
        NativeSmoothAttempt::RequiresYtDlpFallback(reason) => {
            if !app_config.yt_dlp.enabled {
                return Err(anyhow!(
                    "native Smooth admission requires extractor fallback ({reason:?}), но YtDlp отключён"
                ));
            }
            tracing::info!(
                ?reason,
                "CLI native Smooth admission передан единственному YtDlp fallback"
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
    /// Запускает один sequential direct Smooth job без параллельного extractor probe-а.
    pub(crate) fn start_native_smooth_startup_job(
        &mut self,
        source: NativeSmoothUrl,
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
        app_state.set_startup_pending("Проверка native Smooth...".to_owned());
        match NativeSmoothStartupJob::spawn(
            source,
            fallback_locator,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_smooth_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native Smooth startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}
