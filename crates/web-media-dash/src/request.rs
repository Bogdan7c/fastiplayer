//! Typed manifest-backed и serialized DASH VOD open inputs.

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashContainer, DashMediaKind, DashMpdLimits};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxRegistry, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveDemuxBufferLimits,
};
use media_core::DemuxRetryHint;
use source_core::{HttpBoundedByteRange, HttpRequestTarget};
use web_media_adaptive::{
    AdaptiveFetchedResource, AdaptiveHttpContext, AdaptiveResourceQueryApplication,
};
use web_media_transport_api::SourceGeneration;

use crate::DashPresentationSelection;

/// MPD fetch и parser budgets.
#[derive(Clone)]
pub struct DashManifestInput {
    /// Exact secret-safe manifest target.
    pub target: HttpRequestTarget,
    /// Обязательные S04X XML budgets.
    pub xml_budgets: XmlBudgets,
    /// Обязательные S34A schema/profile bounds.
    pub mpd_limits: DashMpdLimits,
}

impl fmt::Debug for DashManifestInput {
    /// Не раскрывает target path/query.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashManifestInput")
            .field("target", &self.target)
            .field("xml_budgets", &self.xml_budgets)
            .field("mpd_limits", &self.mpd_limits)
            .finish()
    }
}

/// Уже загруженный bounded MPD с provenance исходного HTTP context-а.
///
/// Этот type-state нужен direct ingress-у: суффикс `.mpd` только выбирает
/// adapter, а authoritative response body один раз загружается transport owner-ом
/// и затем без второго root request-а передаётся DASH parser/catalog owner-у.
pub struct DashFetchedManifestInput {
    /// Selected root target сохраняет identity первой попытки, но не печатается.
    selected_target: HttpRequestTarget,
    /// Effective redirect target и exact bounded response body.
    fetched: AdaptiveFetchedResource,
    /// Generation context-а, который действительно получил response.
    source_generation: SourceGeneration,
    /// Обязательные S04X XML budgets.
    xml_budgets: XmlBudgets,
    /// Обязательные S34A schema/profile bounds.
    mpd_limits: DashMpdLimits,
}

impl DashFetchedManifestInput {
    /// Связывает fetched response с exact context generation без копирования body.
    #[must_use]
    pub fn new(
        selected_target: HttpRequestTarget,
        fetched: AdaptiveFetchedResource,
        http: &AdaptiveHttpContext,
        xml_budgets: XmlBudgets,
        mpd_limits: DashMpdLimits,
    ) -> Self {
        Self {
            selected_target,
            fetched,
            source_generation: http.source_generation(),
            xml_budgets,
            mpd_limits,
        }
    }

    /// Возвращает generation provenance для fail-closed handoff-а.
    pub(crate) const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Передаёт parser owner-у effective base, bytes и explicit parser budgets.
    pub(crate) fn into_parse_parts(
        self,
    ) -> (HttpRequestTarget, Vec<u8>, XmlBudgets, DashMpdLimits) {
        let effective_target = self.fetched.final_target().clone();
        (
            effective_target,
            self.fetched.into_bytes(),
            self.xml_budgets,
            self.mpd_limits,
        )
    }
}

impl fmt::Debug for DashFetchedManifestInput {
    /// Не раскрывает selected/effective path, query или response body.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashFetchedManifestInput")
            .field("selected_target", &self.selected_target)
            .field("fetched", &self.fetched)
            .field("source_generation", &self.source_generation)
            .field("xml_budgets", &self.xml_budgets)
            .field("mpd_limits", &self.mpd_limits)
            .finish()
    }
}

/// Locator одного serialized fragment до secret-safe resolution.
#[derive(Clone)]
pub enum DashResourceReference {
    /// Уже validated absolute HTTP(S) target.
    Absolute(HttpRequestTarget),
    /// Relative reference и explicit base.
    Relative {
        /// Validated absolute base.
        base: HttpRequestTarget,
        /// Exact relative reference; formatter её не раскрывает.
        reference: String,
    },
}

impl DashResourceReference {
    /// Создаёт absolute resource locator.
    #[must_use]
    pub const fn absolute(target: HttpRequestTarget) -> Self {
        Self::Absolute(target)
    }

    /// Создаёт relative locator с explicit base.
    #[must_use]
    pub fn relative(base: HttpRequestTarget, reference: impl Into<String>) -> Self {
        Self::Relative {
            base,
            reference: reference.into(),
        }
    }

    /// Разрешает locator только в точке runtime planning.
    pub(crate) fn resolve(&self) -> Result<HttpRequestTarget, source_core::HttpRequestTargetError> {
        match self {
            Self::Absolute(target) => Ok(target.clone()),
            Self::Relative { base, reference } => base.resolve_reference(reference),
        }
    }
}

impl fmt::Debug for DashResourceReference {
    /// Не раскрывает absolute или relative locator.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute(target) => formatter.debug_tuple("Absolute").field(target).finish(),
            Self::Relative { base, .. } => formatter
                .debug_struct("Relative")
                .field("base", base)
                .field("reference", &"<redacted>")
                .finish(),
        }
    }
}

/// Роль serialized fragment-а в finite ordered stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashSerializedFragmentKind {
    /// Initialization bytes.
    Initialization,
    /// Один finite media fragment.
    Media,
}

/// Один bounded serialized fragment без generator state.
#[derive(Clone)]
pub struct DashSerializedFragment {
    /// Explicit fragment role.
    pub kind: DashSerializedFragmentKind,
    /// Absolute либо relative locator.
    pub target: DashResourceReference,
    /// Optional exact byte range.
    pub byte_range: Option<HttpBoundedByteRange>,
    /// Media duration; initialization обязана иметь `None`.
    pub duration: Option<Duration>,
}

impl fmt::Debug for DashSerializedFragment {
    /// Показывает только безопасную форму fragment-а.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashSerializedFragment")
            .field("kind", &self.kind)
            .field("target", &self.target)
            .field("byte_range", &self.byte_range)
            .field("duration", &self.duration)
            .finish()
    }
}

/// Один serialized single-period component.
#[derive(Debug, Clone)]
pub struct DashSerializedComponent {
    /// Proven container.
    pub container: DashContainer,
    /// Explicit component kind.
    pub media_kind: DashMediaKind,
    /// Bounded ordered fragments.
    pub fragments: Vec<DashSerializedFragment>,
    /// Pinned query projection для каждого fragment request-а.
    pub query_application: AdaptiveResourceQueryApplication,
}

/// Authoritative serialized path; multi-period state намеренно отсутствует в типе.
#[derive(Debug, Clone)]
pub enum DashSerializedPresentation {
    /// Один muxed/video-only/audio-only component.
    Single(DashSerializedComponent),
    /// Exact aligned video/audio components.
    Separate {
        /// Video component.
        video: DashSerializedComponent,
        /// Audio component.
        audio: DashSerializedComponent,
    },
}

/// Ровно один authoritative input path.
#[derive(Debug)]
pub enum DashVodInput {
    /// Fetch и parse static MPD.
    Manifest(DashManifestInput),
    /// Parse уже fetched static MPD без повторного root request-а.
    FetchedManifest(DashFetchedManifestInput),
    /// Concrete single-period fragments; MPD fallback после ошибки запрещён типом.
    Serialized(DashSerializedPresentation),
}

/// Input-shape-aligned HTTP contexts без positional component guessing.
#[derive(Debug, Clone)]
pub enum DashVodHttpContext {
    /// Один MPD request context обслуживает выбранные Representation resources.
    Manifest(Box<AdaptiveHttpContext>),
    /// Один serialized component имеет собственный request scope.
    SerializedSingle(Box<AdaptiveHttpContext>),
    /// Separate serialized components сохраняют независимые secret scopes.
    SerializedSeparate {
        /// Video fragment request context.
        video: Box<AdaptiveHttpContext>,
        /// Audio fragment request context.
        audio: Box<AdaptiveHttpContext>,
    },
}

/// Caller-owned bounds и demux/backpressure policies.
#[derive(Debug, Clone, Copy)]
pub struct DashVodOpenPolicy {
    /// Максимум MPD body bytes.
    pub maximum_manifest_bytes: NonZeroUsize,
    /// Максимум одного init/media fragment body.
    pub maximum_fragment_bytes: NonZeroUsize,
    /// Максимум одного SegmentBase Range read-а.
    pub maximum_range_read_bytes: NonZeroUsize,
    /// Максимум planned media fragments на component.
    pub maximum_planned_segments: NonZeroUsize,
    /// Registry sniff/replay bounds.
    pub demux_sniff_budget: DemuxSniffBudget,
    /// Progressive worker queue bounds.
    pub progressive_limits: ProgressiveDemuxBufferLimits,
    /// Bound accepted async seek requests без drained terminal receipt-а.
    pub asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
    /// Player-facing temporary readiness hint.
    pub retry_hint: DemuxRetryHint,
    /// Independent component interleave/backpressure policy.
    pub composite_lead_policy: CompositeComponentLeadPolicy,
    /// Максимум events initial/seek anchor scan-а.
    pub maximum_seek_scan_events: NonZeroUsize,
    /// Максимум encoded bytes initial/seek anchor scan-а.
    pub maximum_seek_scan_bytes: NonZeroUsize,
}

/// Полный неустановленный S34B request.
pub struct DashVodOpenRequest {
    /// Input-shape-aligned S31 contexts с exact generation/secrets/cancel policy.
    pub http: DashVodHttpContext,
    /// Exact generation request-а.
    pub generation: SourceGeneration,
    /// Authoritative MPD либо serialized path.
    pub input: DashVodInput,
    /// Exact selection evidence для manifest path.
    pub selection: DashPresentationSelection,
    /// Injected reusable neutral demux registry.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Explicit bounds без runtime defaults.
    pub policy: DashVodOpenPolicy,
}
