//! App-owned direct HDS VOD admission поверх existing S38 data plane.

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
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

use crate::native_web_source::hds::{NativeHdsSourceState, NativeHdsUrl};
use crate::web_media_open::{NativeHdsCandidatePreparation, prepare_native_hds_candidate};

/// Fresh direct snapshots сохраняют source lineage, но меняют exact generation.
static NEXT_NATIVE_HDS_SNAPSHOT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Direct HDS redirect budget совпадает с другими native manifest ingress-ами.
const NATIVE_HDS_REDIRECT_HOPS: u8 = 5;

/// HDS alias общего cross-protocol native admission результата.
pub type NativeHdsAttempt<Prepared> =
    crate::native_web_source::fallback::NativeWebMediaAttempt<Prepared>;

/// Все production inputs одной native HDS attempt.
pub struct NativeHdsPreparationRequest<'request> {
    /// Stable app-owned `.f4m` root.
    pub source: &'request NativeHdsUrl,
    /// Installed semantic selection для switch/reopen/root refresh.
    pub expected_selection: Option<&'request WebMediaSemanticSelectionRequest>,
    /// Network budgets/retry/source policy.
    pub network_config: &'request NetworkConfig,
    /// Preferred-height policy и neutral stream projection preference.
    pub web_media_config: &'request WebMediaConfig,
    /// Existing demux corruption/sniff limits.
    pub demux_config: &'request PlayerDemuxConfig,
    /// Actual video decoder capability snapshot.
    pub system_capabilities: &'request capability_core::SystemCapabilities,
    /// Actual audio decoder capability snapshot.
    pub audio_capabilities: audio_core::AudioDecodeCapabilitySnapshot,
    /// Cooperative cancellation одной physical attempt.
    pub cancellation: CancellationToken,
}

/// Ready native HDS VOD и provider-neutral lifecycle state.
pub struct PreparedNativeHdsMedia {
    /// Existing S38 transactional F4F demux runtime.
    pub demuxer: Box<dyn Demuxer + Send>,
    /// Worker-receipted transactional VOD seek boundary.
    pub seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Player-owned zero-based projection absolute HDS clock-а.
    pub playback_window: MediaPlaybackWindow,
    /// Stable root + neutral catalog selection projection.
    pub source_state: NativeHdsSourceState,
    /// VOD endpoint expiry owner arm-ится только после Installed.
    pub endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
}

/// Fresh parent и catalog получают одну generation и stable source lineage.
struct NativeHdsSnapshotIdentity {
    /// Exact parent текущей physical attempt.
    parent: ExactSelectionIdentity,
    /// Exact component catalog generation текущей attempt.
    catalog: ComponentVariantCatalogIdentity,
}

/// Готовит direct HDS VOD без parser/transport/runtime дубля.
pub fn prepare_native_hds_attempt(
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

    let demux_registry = super::dash::native_dash_demux_registry(request.demux_config)?;
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
pub fn native_hds_failure_kind(error: &anyhow::Error) -> HdsPrepareFailureKind {
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
            const { NonZeroU8::new(3).expect("native HDS retry attempts") },
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )
    .map_err(anyhow::Error::new)
}
