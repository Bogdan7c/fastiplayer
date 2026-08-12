//! Стабильная redacted taxonomy ошибок preparation boundary.

use std::fmt;

use smooth_streaming_fmp4::{SmoothInitializationError, SmoothTrackMappingError};
use smooth_streaming_manifest_core::SmoothManifestError;
use web_media_adaptive::AdaptiveTransportError;
use web_media_core::{ComponentKind, ComponentVariantError, ComponentVariantKeyError};

/// Безопасная причина изоляции одной sibling quality без manifest/runtime payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmoothSiblingRejectionReason {
    /// Quality нельзя отобразить в sealed Smooth fMP4 contract.
    #[error("Smooth sibling track mapping is unsupported")]
    TrackMapping,
    /// Initialization segment quality не прошёл bounded construction.
    #[error("Smooth sibling initialization is unsupported")]
    Initialization,
    /// Manifest metadata/identity quality нельзя выразить в neutral catalog.
    #[error("Smooth sibling metadata is unsupported")]
    UnsupportedMetadata,
    /// Две qualities имеют неразличимую refresh-stable identity.
    #[error("Smooth sibling semantic identity is ambiguous")]
    AmbiguousSemanticIdentity,
    /// Первый fragment не прошёл bounded planning/fetch/reconstruction.
    #[error("Smooth sibling transport or content is unavailable")]
    TransportOrContentUnavailable,
    /// Injected ISO-BMFF demuxer не открыл bounded content proof.
    #[error("Smooth sibling demux proof failed")]
    DemuxFailed,
    /// Demuxer не доказал ровно один track ожидаемой component axis.
    #[error("Smooth sibling track shape is unsupported")]
    UnsupportedTrackShape,
    /// Фактический track противоречит manifest descriptor.
    #[error("Smooth sibling manifest evidence conflicts with media")]
    ManifestEvidenceConflict,
    /// Immutable decoder/renderer capability snapshot отклонил track.
    #[error("Smooth sibling capability is unavailable")]
    CapabilityUnavailable,
}

/// Одна bounded diagnostic row без quality index, URL или codec-private bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmoothSiblingRejection {
    component: ComponentKind,
    reason: SmoothSiblingRejectionReason,
}

impl SmoothSiblingRejection {
    pub(crate) const fn new(
        component: ComponentKind,
        reason: SmoothSiblingRejectionReason,
    ) -> Self {
        Self { component, reason }
    }

    /// Возвращает component axis отброшенной quality.
    #[must_use]
    pub const fn component(self) -> ComponentKind {
        self.component
    }

    /// Возвращает safe typed причину без provider payload.
    #[must_use]
    pub const fn reason(self) -> SmoothSiblingRejectionReason {
        self.reason
    }
}

/// Точная причина несовместимости neutral transport intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmoothTransportProfileError {
    /// Этот finite preparation boundary не владеет live refresh.
    #[error("Smooth preparation поддерживает только VOD presentation")]
    NonVodPresentation,
    /// Parent request обязан описывать presentation-level manifest resource.
    #[error("Smooth preparation требует PresentationManifest component role")]
    NonPresentationManifestComponent,
    /// Exact и semantic identities presentation manifest-а принадлежат разным source lineages.
    #[error("Smooth presentation manifest identity нарушает source lineage")]
    InvalidComponentIdentity,
    /// Manifest fetch всегда full-body и не принимает media range policy.
    #[error("Smooth manifest request не должен содержать HTTP range limit")]
    UnexpectedRangeLimit,
}

/// Ошибка versioned semantic-key framing без раскрытия исходных полей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmoothSemanticKeyError {
    /// `usize` длину поля нельзя losslessly представить в canonical `u64`.
    #[error("длина semantic-key field не помещается в canonical u64 framing")]
    FieldLengthOutOfRange,
    /// Суммарная длина output key не представима текущей платформой.
    #[error("длина semantic-key output переполнена")]
    OutputLengthOverflow,
}

/// Нарушение provider-level Smooth profile после sealed parser validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmoothProfileError {
    /// Требуется ровно один video и один audio stream.
    #[error("Smooth VOD должен содержать ровно один video и один audio stream без extras")]
    StreamShape,
    /// Каждая обязательная component axis должна иметь хотя бы одно качество.
    #[error("Smooth component axis не содержит quality levels")]
    EmptyQualityAxis,
    /// Timeline обязан начинаться точно в нуле.
    #[error("Smooth component timeline начинается не в нуле")]
    NonZeroStart,
    /// Component timeline не может выходить за authoritative root duration.
    #[error("Smooth component timeline выходит за root duration")]
    ComponentExceedsRootDuration,
    /// Video обязан покрывать весь authoritative root duration без tolerance.
    #[error("Smooth video timeline не совпадает с root duration")]
    VideoDurationMismatch,
    /// Parsed quality shape не соответствует stream axis.
    #[error("Smooth quality kind не соответствует component axis")]
    QualityKindMismatch,
    /// Сумма initialization segments превысила caller budget.
    #[error("aggregate Smooth initialization bytes превысили caller budget")]
    AggregateInitializationLimit,
    /// Neutral descriptor не смог представить validated manifest metadata.
    #[error("Smooth metadata выходит за neutral descriptor bounds")]
    DescriptorBounds,
}

/// Ошибка атомарной подготовки: частичный catalog никогда не публикуется.
#[derive(thiserror::Error)]
pub enum SmoothPrepareError {
    /// Cancellation всех нижних слоёв схлопывается в один terminal outcome.
    #[error("подготовка Smooth VOD отменена")]
    Cancelled,
    /// Transport intent несовместим с finite muxed Smooth preparation.
    #[error(transparent)]
    TransportProfile(#[from] SmoothTransportProfileError),
    /// Единственный manifest fetch завершился ошибкой.
    #[error("не удалось получить Smooth manifest")]
    Fetch(#[source] AdaptiveTransportError),
    /// Hardened XML/schema/profile parse завершился ошибкой.
    #[error("не удалось разобрать Smooth manifest")]
    Manifest(#[source] SmoothManifestError),
    /// Дополнительный provider-level profile invariant нарушен.
    #[error(transparent)]
    Profile(#[from] SmoothProfileError),
    /// S36F2 mapping одного объявленного качества завершился ошибкой.
    #[error("не удалось отобразить Smooth quality в fragmented MP4 contract")]
    Mapping(#[source] SmoothTrackMappingError),
    /// F1 initialization одного объявленного качества завершился ошибкой.
    #[error("не удалось построить Smooth initialization segment")]
    Initialization(#[source] SmoothInitializationError),
    /// Канонический opaque key нарушил C3 bounds.
    #[error("не удалось построить bounded Smooth variant identity")]
    VariantKey(#[source] ComponentVariantKeyError),
    /// Versioned semantic key нельзя закодировать без потери длины.
    #[error("не удалось канонически закодировать Smooth semantic key")]
    SemanticKey(#[source] SmoothSemanticKeyError),
    /// Готовые rows нарушили C3 catalog invariants.
    #[error("не удалось собрать Smooth component catalog")]
    Catalog(#[source] ComponentVariantError),
}

impl fmt::Debug for SmoothPrepareError {
    /// Debug публикует только стабильную классификацию без URL/XML/attributes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothPrepareError")
            .field("kind", &self.kind_name())
            .finish()
    }
}

impl SmoothPrepareError {
    /// Возвращает безопасное имя варианта для redacted diagnostics.
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::TransportProfile(_) => "transport-profile",
            Self::Fetch(_) => "fetch",
            Self::Manifest(_) => "manifest",
            Self::Profile(_) => "profile",
            Self::Mapping(_) => "mapping",
            Self::Initialization(_) => "initialization",
            Self::VariantKey(_) => "variant-key",
            Self::SemanticKey(_) => "semantic-key",
            Self::Catalog(_) => "catalog",
        }
    }
}
