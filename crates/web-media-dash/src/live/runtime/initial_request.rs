//! Initial fetched/fetch type-state для единого S35 runtime open-а.

use std::sync::Arc;
use std::time::Instant;

use demux_api::DemuxRegistry;
use media_core::{DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::{DashEndpointRefreshPort, DashLiveOpenRequest};
use crate::live::{DashClockFetchObservation, DashWallClock};
use crate::request::{DashFetchedManifestInput, DashManifestInput, DashVodOpenPolicy};

/// Уже fetched dynamic root с точным clock/deadline observation первой попытки.
#[derive(Clone)]
pub struct DashFetchedLiveManifestInput {
    /// Общий N09 fetched handoff сохраняет body, effective base и generation provenance.
    pub(super) manifest: Arc<DashFetchedManifestInput>,
    /// Refresh cadence измеряется от фактического начала первого root request-а.
    pub(super) fetch_started: Instant,
    /// Direct UTC correction использует локальные границы именно этого request-а.
    pub(super) observation: DashClockFetchObservation,
}

impl DashFetchedLiveManifestInput {
    /// Создаёт live-only handoff без повторного root GET или потери clock sample-а.
    #[must_use]
    pub fn new(
        manifest: DashFetchedManifestInput,
        fetch_started: Instant,
        observation: DashClockFetchObservation,
    ) -> Self {
        Self {
            manifest: Arc::new(manifest),
            fetch_started,
            observation,
        }
    }

    /// Возвращает immutable fetched handoff для authoritative discovery.
    pub(crate) fn manifest(&self) -> &DashFetchedManifestInput {
        &self.manifest
    }

    /// Возвращает локальный clock sample именно первого root request-а.
    pub(crate) const fn observation(&self) -> DashClockFetchObservation {
        self.observation
    }
}

impl std::fmt::Debug for DashFetchedLiveManifestInput {
    /// Не раскрывает root/effective URL, query или MPD body.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashFetchedLiveManifestInput")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

/// Initial root может быть fetched runtime-ом либо безопасно передан direct ingress-ом.
#[derive(Clone)]
pub(crate) enum DashLiveInitialManifest {
    /// Existing extractor-backed/runtime-owned fetch path.
    Fetch(DashManifestInput),
    /// Direct ingress уже выполнил bounded authoritative root fetch.
    Fetched(DashFetchedLiveManifestInput),
}

/// Runtime-owned request не требует extractor evidence для logical catalog selection.
#[derive(Clone)]
pub(crate) struct DashLiveRuntimeOpenRequest {
    /// Manifest/clock/component HTTP context одной source generation.
    pub http: Box<AdaptiveHttpContext>,
    /// Exact source generation всех initial resources.
    pub generation: SourceGeneration,
    /// Initial-only fetch state; worker clone немедленно нормализуется к stable `Fetch`.
    pub initial_manifest: DashLiveInitialManifest,
    /// Existing injected fMP4/WebM factories.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Explicit bounded manifest/segment/seek policy.
    pub policy: DashVodOpenPolicy,
    /// Injected local wall clock для UTC synchronization.
    pub wall_clock: Arc<dyn DashWallClock>,
    /// Neutral dynamic timeline port generation.
    pub timeline_port_generation: DynamicMediaTimelinePortGeneration,
    /// Initial source-side dynamic timeline epoch.
    pub initial_source_epoch: DynamicMediaTimelineEpoch,
    /// App-owned endpoint generation recovery boundary.
    pub endpoint_refresh: Arc<dyn DashEndpointRefreshPort>,
}

impl From<DashLiveOpenRequest> for DashLiveRuntimeOpenRequest {
    /// Нормализует legacy evidence request к общему runtime boundary.
    fn from(request: DashLiveOpenRequest) -> Self {
        Self {
            http: request.http,
            generation: request.generation,
            initial_manifest: DashLiveInitialManifest::Fetch(request.manifest),
            demux_registry: request.demux_registry,
            policy: request.policy,
            wall_clock: request.wall_clock,
            timeline_port_generation: request.timeline_port_generation,
            initial_source_epoch: request.initial_source_epoch,
            endpoint_refresh: request.endpoint_refresh,
        }
    }
}
