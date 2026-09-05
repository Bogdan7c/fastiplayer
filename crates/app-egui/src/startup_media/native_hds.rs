//! App-owned direct HDS VOD admission поверх existing S38 data plane.

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig, WebMediaConfig};
use media_core::Demuxer;
use player_core::{MediaPlaybackWindow, PreparedDemuxSeekPort};
use source_core::{CancellationToken, HttpPathScope, SourceRuntimeConfig};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportError,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ExactSelectionIdentity, ExtractionGeneration,
    SemanticIdentity, WebMediaFallbackTrigger, WebMediaSemanticSelectionRequest,
};
use web_media_hds::{HdsFetchedManifestInput, HdsPrepareFailureKind};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::{NativeHdsSourceState, NativeHdsUrl};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::web_media_open::{NativeHdsCandidatePreparation, prepare_native_hds_candidate};

use super::orchestration::PreparedStartupMedia;

/// Fresh direct snapshots сохраняют source lineage, но меняют exact generation.
static NEXT_NATIVE_HDS_SNAPSHOT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Direct HDS redirect budget совпадает с другими native manifest ingress-ами.
const NATIVE_HDS_REDIRECT_HOPS: u8 = 5;

/// HDS alias общего cross-protocol native admission результата.
pub(crate) type NativeHdsAttempt<Prepared> =
    crate::media_open::native_fallback::NativeWebMediaAttempt<Prepared>;

/// Все production inputs одной native HDS attempt.
pub(crate) struct NativeHdsPreparationRequest<'request> {
    /// Stable app-owned `.f4m` root.
    pub(crate) source: &'request NativeHdsUrl,
    /// Installed semantic selection для switch/reopen/root refresh.
    pub(crate) expected_selection: Option<&'request WebMediaSemanticSelectionRequest>,
    /// Network budgets/retry/source policy.
    pub(crate) network_config: &'request NetworkConfig,
    /// Preferred-height policy и neutral stream projection preference.
    pub(crate) web_media_config: &'request WebMediaConfig,
    /// Existing demux corruption/sniff limits.
    pub(crate) demux_config: &'request PlayerDemuxConfig,
    /// Actual video decoder capability snapshot.
    pub(crate) system_capabilities: &'request capability_core::SystemCapabilities,
    /// Actual audio decoder capability snapshot.
    pub(crate) audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    /// Cooperative cancellation одной physical attempt.
    pub(crate) cancellation: CancellationToken,
}

/// Ready native HDS VOD и provider-neutral lifecycle state.
pub(crate) struct PreparedNativeHdsMedia {
    /// Existing S38 transactional F4F demux runtime.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Worker-receipted transactional VOD seek boundary.
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Player-owned zero-based projection absolute HDS clock-а.
    pub(crate) playback_window: MediaPlaybackWindow,
    /// Stable root + neutral catalog selection projection.
    pub(crate) source_state: NativeHdsSourceState,
    /// VOD endpoint expiry owner arm-ится только после Installed.
    pub(crate) endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
}

/// Fresh parent и catalog получают одну generation и stable source lineage.
struct NativeHdsSnapshotIdentity {
    /// Exact parent текущей physical attempt.
    parent: ExactSelectionIdentity,
    /// Exact component catalog generation текущей attempt.
    catalog: ComponentVariantCatalogIdentity,
}

/// Готовит direct HDS VOD без parser/transport/runtime дубля.
pub(crate) fn prepare_native_hds_attempt(
    request: NativeHdsPreparationRequest<'_>,
) -> Result<NativeHdsAttempt<PreparedNativeHdsMedia>> {
    if request.cancellation.is_cancelled() {
        return Err(AdaptiveTransportError::Cancelled.into());
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
        .context("native HDS source config")?;
    let http = native_adaptive_http_context(transport.clone(), &source_config, adaptive_limits)?;

    // Единственный root GET одновременно служит content admission и HDS discovery.
    let fetched_manifest = match http.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        generation,
        request.source.target().clone(),
        adaptive_limits.maximum_manifest_bytes,
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )) {
        Ok(fetched_manifest) => fetched_manifest,
        Err(error) if matches!(error.http_status_code(), Some(401 | 403)) => {
            return Ok(NativeHdsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
            ));
        }
        Err(error @ AdaptiveTransportError::Cancelled) => {
            return Err(error).context("native HDS root fetch cancelled");
        }
        Err(error) => return Err(error).context("native HDS root fetch"),
    };

    let demux_registry = super::native_dash::native_dash_demux_registry(request.demux_config)?;
    let capability_probe =
        crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe::new(
            request.system_capabilities.clone(),
            request.audio_capabilities,
        );
    let prepared = match prepare_native_hds_candidate(NativeHdsCandidatePreparation {
        transport,
        fetched_manifest: HdsFetchedManifestInput::new(
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
        Err(error) if native_hds_failure_kind(&error) == HdsPrepareFailureKind::InvalidRoot => {
            return Ok(NativeHdsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ProviderDocument,
            ));
        }
        Err(error) => return Err(error),
    };
    let source_state = NativeHdsSourceState::new(
        prepared.neutral_selection,
        prepared.component_catalog,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            request.web_media_config,
        ),
    )
    .context("native HDS neutral catalog projection failed")?;

    Ok(NativeHdsAttempt::Prepared(PreparedNativeHdsMedia {
        demuxer: prepared.demuxer,
        seek_port: prepared.seek_port,
        playback_window: prepared.playback_window,
        source_state,
        endpoint_recovery,
    }))
}

/// Возвращает typed failure kind без анализа display strings.
#[must_use]
pub(crate) fn native_hds_failure_kind(error: &anyhow::Error) -> HdsPrepareFailureKind {
    web_media_hds::classify_hds_prepare_error(error)
}

/// Создаёт fresh exact parent/catalog identity без URL/hash material.
fn fresh_snapshot_identity(source: &NativeHdsUrl) -> Result<NativeHdsSnapshotIdentity> {
    let generation = NEXT_NATIVE_HDS_SNAPSHOT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .max(1);
    let source_identity = source.source_identity();
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source_identity,
            ExtractionGeneration::new(generation),
            CandidateFormatIdentity::new("native-hds-vod")?,
        ),
        SemanticIdentity::new(source_identity, "native-hds-vod")?,
    )?;
    let catalog = ComponentVariantCatalogIdentity::new(
        parent.clone(),
        ComponentVariantCatalogGeneration::new(generation),
    );
    Ok(NativeHdsSnapshotIdentity { parent, catalog })
}

/// Собирает public HTTP request с реальным origin/path scope и без raw secrets.
fn native_transport_request(
    parent: &ExactSelectionIdentity,
    source: &NativeHdsUrl,
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
        TransportProviderId::new("native-hds-http")?,
        component,
        initial_target,
        MediaPresentation::Vod,
        generation,
        request_context,
        RedirectPolicy::cross_origin_without_secrets(RedirectHopLimit::new(
            NATIVE_HDS_REDIRECT_HOPS,
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
            NonZeroU8::new(3).expect("native HDS retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )
    .map_err(anyhow::Error::new)
}

/// Результат одного sequential native-admission/extractor-fallback startup job-а.
type NativeHdsStartupResult = std::result::Result<PreparedStartupMedia, String>;

/// Фоновый startup job сначала завершает native admission и только потом решает fallback.
pub(super) struct NativeHdsStartupJob {
    /// Bounded pending label для startup overlay.
    pending_message: String,
    /// Exactly-once completion mailbox фонового resolver-а.
    result_receiver: OwnerMailboxReceiver<(), NativeHdsStartupResult>,
    /// JoinHandle нужен bounded shutdown owner-у.
    pub(super) join_handle: Option<JoinHandle<()>>,
    /// Result удерживается до physical worker exit.
    pending_result: Option<NativeHdsStartupResult>,
    /// Cooperative publication fence.
    pub(super) cancellation_requested: Arc<AtomicBool>,
    /// Тот же token физически отменяет HTTP/HDS work.
    pub(super) source_cancellation: CancellationToken,
}

impl NativeHdsStartupJob {
    /// Запускает одну mutually-exclusive direct `.f4m` attempt.
    pub(super) fn spawn(
        source: NativeHdsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_config: fastiplayer_config::AppConfig,
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
            .name("native-hds-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_hds_startup_media(
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
                        "Event loop закрыт; native HDS terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native HDS startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| format!("Не удалось запустить native HDS startup opener: {error}"))?;
        Ok(Self {
            pending_message: "Проверка native HDS...".to_owned(),
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
    pub(super) fn try_take_result(&mut self) -> Option<NativeHdsStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Native HDS startup opener завершился без результата".to_owned())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err("Native HDS startup opener завершился panic".to_owned()))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

/// Выполняет native admission и единственный typed extractor fallback.
fn resolve_native_hds_startup_media(
    source: NativeHdsUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    app_config: &fastiplayer_config::AppConfig,
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    match prepare_native_hds_attempt(NativeHdsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        system_capabilities,
        audio_capabilities,
        cancellation: cancellation.clone(),
    })? {
        NativeHdsAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeHds {
            source,
            prepared: Box::new(prepared),
        }),
        NativeHdsAttempt::RequiresExtractorFallback(trigger) => {
            let mut fallback_owner =
                crate::media_open::native_fallback::NativeWebFallbackOwner::before_installed(
                    fallback_locator,
                );
            let fallback = fallback_owner
                .claim(trigger)
                .map_err(|rejection| anyhow!("native HDS fallback rejected: {rejection:?}"))?;
            let (fallback_locator, invocation_reason) = fallback.into_parts();
            if !app_config.yt_dlp.enabled {
                return Err(anyhow!(
                    "native HDS admission requires extractor fallback ({invocation_reason:?}), но YtDlp отключён"
                ));
            }
            tracing::info!(
                ?invocation_reason,
                "CLI native HDS admission передан единственному YtDlp fallback"
            );
            let prepared = super::resolve_yt_dlp_startup_media(
                &fallback_locator,
                app_config,
                system_capabilities,
                audio_capabilities,
                invocation_reason,
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
    /// Запускает один sequential direct HDS job без параллельного extractor probe-а.
    pub(crate) fn start_native_hds_startup_job(
        &mut self,
        source: NativeHdsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut crate::state::AppState,
        app_config: &fastiplayer_config::AppConfig,
        system_capabilities: &capability_core::SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Проверка native HDS...".to_owned());
        match NativeHdsStartupJob::spawn(
            source,
            fallback_locator,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_hds_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native HDS startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}
