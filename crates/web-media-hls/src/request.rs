use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use demux_api::{
    CompositeComponentLeadPolicy, DemuxContainerId, DemuxRegistry, DemuxSniffBudget,
    ProgressiveDemuxBufferLimits,
};
use hls_playlist_core::HlsParserLimits;
use media_core::{DemuxRetryHint, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration};
use source_core::HttpRequestTarget;
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;
use zeroize::Zeroizing;

use crate::{ExtractorAesOverride, HlsVariantSelectionIntent};

/// Authoritative top-level HLS manifest source.
pub enum HlsManifestInput {
    /// Manifest должен быть загружен ровно один раз по selected candidate URL.
    Fetch {
        /// Selected URL также является initial resolution base.
        selected_url: HttpRequestTarget,
    },
    /// Extractor уже передал authoritative media playlist; network fetch запрещён.
    InlineMedia {
        /// Selected URL остаётся base для всех relative references.
        selected_url: HttpRequestTarget,
        /// Secret-safe owned manifest bytes.
        playlist: SecretInlineMediaPlaylist,
    },
}

impl HlsManifestInput {
    /// Возвращает selected URL без раскрытия exact secret serialization.
    pub const fn selected_url(&self) -> &HttpRequestTarget {
        match self {
            Self::Fetch { selected_url } | Self::InlineMedia { selected_url, .. } => selected_url,
        }
    }
}

impl fmt::Debug for HlsManifestInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch { selected_url } => formatter
                .debug_struct("HlsManifestInput::Fetch")
                .field("selected_url", selected_url)
                .finish(),
            Self::InlineMedia {
                selected_url,
                playlist,
            } => formatter
                .debug_struct("HlsManifestInput::InlineMedia")
                .field("selected_url", selected_url)
                .field("playlist", playlist)
                .finish(),
        }
    }
}

/// Inline manifest buffer, обнуляемый при уничтожении и не печатающий содержимое.
pub struct SecretInlineMediaPlaylist(Zeroizing<Vec<u8>>);

impl SecretInlineMediaPlaylist {
    /// Копирует validated extractor string в secret-safe owned buffer.
    #[must_use]
    pub fn new(playlist: &str) -> Self {
        Self(Zeroizing::new(playlist.as_bytes().to_vec()))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretInlineMediaPlaylist {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretInlineMediaPlaylist")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Exact extractor overrides, применяемые только HLS owner-ом.
#[derive(Clone)]
pub struct HlsRequestOverrides {
    aes: Option<ExtractorAesOverride>,
}

impl HlsRequestOverrides {
    /// Создаёт HLS-only AES material.
    ///
    /// Segment/key query material намеренно отсутствует: единственный authoritative owner —
    /// scoped `SecretRequestContext` внутри `AdaptiveHttpContext`.
    #[must_use]
    pub const fn new(aes: Option<ExtractorAesOverride>) -> Self {
        Self { aes }
    }

    pub(crate) const fn aes(&self) -> Option<&ExtractorAesOverride> {
        self.aes.as_ref()
    }
}

impl fmt::Debug for HlsRequestOverrides {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsRequestOverrides")
            .field("has_aes", &self.aes.is_some())
            .finish()
    }
}

/// Content container, который composition evidence требует доказать sniff-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsRequiredContainer {
    /// MPEG transport stream, включая допустимый TS `EXT-X-MAP`.
    TransportStream,
    /// Fragmented ISO BMFF; каждый media segment обязан иметь active `EXT-X-MAP`.
    FragmentedMp4,
}

impl HlsRequiredContainer {
    pub(crate) fn demux_container_id(
        self,
    ) -> Result<DemuxContainerId, demux_api::DemuxIdentityError> {
        let identity = match self {
            Self::TransportStream => "mpeg-ts",
            Self::FragmentedMp4 => "iso-bmff",
        };
        DemuxContainerId::new(identity)
    }
}

/// Результат candidate/composition evidence без guessing по MAP/extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsContainerEvidence {
    /// Evidence однозначно задаёт required content container.
    Exact(HlsRequiredContainer),
    /// Candidate не предоставил достаточного container evidence.
    Missing,
    /// Candidate evidence противоречиво либо указывает несколько containers.
    Ambiguous,
    /// Container должен быть доказан bounded content sniff-ом, без extension/MAP guessing.
    ContentProbe,
}

/// Container evidence для main и optional alternate-audio component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsComponentContainerIntent {
    /// Main variant/media component.
    pub main: HlsContainerEvidence,
    /// Alternate audio component; обязателен для separate-audio selection.
    pub alternate_audio: Option<HlsContainerEvidence>,
}

/// Caller-owned bounds и backend policies; HLS runtime ничего не хардкодит.
#[derive(Debug, Clone, Copy)]
pub struct HlsVodOpenPolicy {
    /// Shared bounded parser policy.
    pub parser_limits: HlsParserLimits,
    /// Registry sniff/replay bounds.
    pub demux_sniff_budget: DemuxSniffBudget,
    /// Player-facing progressive queue bounds.
    pub progressive_limits: ProgressiveDemuxBufferLimits,
    /// Temporary readiness hint до deferred open completion.
    pub retry_hint: DemuxRetryHint,
    /// Existing independent-component interleave policy.
    pub composite_lead_policy: CompositeComponentLeadPolicy,
    /// Maximum key response bytes; должен позволять exact 16-byte validation.
    pub maximum_key_resource_bytes: NonZeroUsize,
    /// Максимум доказанных packet anchors одного component index-а.
    pub maximum_seek_index_entries: NonZeroUsize,
    /// Максимум demux events при transactional восстановлении одного anchor-а.
    pub maximum_seek_replay_events: NonZeroUsize,
    /// Максимум encoded bytes, просмотренных при transactional восстановлении anchor-а.
    pub maximum_seek_replay_bytes: NonZeroUsize,
}

/// Полный неустановленный S32B open request.
pub struct HlsVodOpenRequest {
    /// Shared S31 transport context; содержит cancellation/generation/secrets.
    pub http: AdaptiveHttpContext,
    /// Exact generation должна совпасть с immutable context.
    pub generation: SourceGeneration,
    /// Fetch либо authoritative inline media playlist.
    pub manifest: HlsManifestInput,
    /// Candidate evidence для строгого master/rendition выбора.
    pub selection: HlsVariantSelectionIntent,
    /// Exact extractor AES override; scoped query material принадлежит HTTP context.
    pub overrides: HlsRequestOverrides,
    /// Explicit per-component container evidence; MAP никогда не используется как guess.
    pub containers: HlsComponentContainerIntent,
    /// Reusable neutral registry, собранный composition root-ом.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Explicit parser/demux/backpressure policy.
    pub policy: HlsVodOpenPolicy,
}

/// Причина app-owned fresh endpoint extraction без URL/status payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsEndpointRefreshReason {
    AuthorizationExpired,
    ResourceExpired,
}

impl HlsEndpointRefreshReason {
    /// Классифицирует только statuses, доказательно означающие expiry/re-authorization.
    #[must_use]
    pub(crate) const fn from_http_status(status: u16) -> Option<Self> {
        match status {
            401 | 403 => Some(Self::AuthorizationExpired),
            404 | 410 => Some(Self::ResourceExpired),
            _ => None,
        }
    }
}

/// Neutral request от HLS refresh owner-а к app process owner-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsEndpointRefreshRequest {
    pub previous_generation: SourceGeneration,
    pub reason: HlsEndpointRefreshReason,
}

/// Fresh transport material после app-owned re-extraction и semantic rematch.
pub struct HlsEndpointRefreshReply {
    pub http: AdaptiveHttpContext,
    pub generation: SourceGeneration,
    pub manifest: HlsManifestInput,
    pub overrides: HlsRequestOverrides,
}

/// Bounded typed отказ app-owned refresh owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HlsEndpointRefreshError {
    #[error("HLS endpoint refresh cancelled")]
    Cancelled,
    #[error("fresh extraction не содержит semantic candidate match")]
    SemanticRematchFailed,
    #[error("fresh candidate больше не является compatible public HLS live")]
    IncompatibleLiveCandidate,
    #[error("HLS endpoint refresh owner disconnected")]
    OwnerDisconnected,
    #[error("HLS endpoint refresh attempts exhausted")]
    AttemptsExhausted,
}

/// App-owned process boundary; implementation не выполняется demux worker-ом.
pub trait HlsEndpointRefreshPort: Send + Sync {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError>;
}

/// Полный S33 live open intent поверх proven S32 common request.
pub struct HlsLiveOpenRequest {
    pub common: HlsVodOpenRequest,
    pub endpoint_refresh: Arc<dyn HlsEndpointRefreshPort>,
    pub timeline_port_generation: DynamicMediaTimelinePortGeneration,
    pub initial_source_epoch: DynamicMediaTimelineEpoch,
}

impl fmt::Debug for HlsVodOpenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsVodOpenRequest")
            .field("http", &self.http)
            .field("generation", &self.generation)
            .field("manifest", &self.manifest)
            .field("selection", &self.selection)
            .field("overrides", &self.overrides)
            .field("containers", &self.containers)
            .field("demux_registry", &"<injected>")
            .field("policy", &self.policy)
            .finish()
    }
}
