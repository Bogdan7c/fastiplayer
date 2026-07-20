use crate::{
    CodecMediaKind, ContainerHintConflict, ContainerIdentity, NormalizedCodec, NormalizedTransport,
    StreamLayoutKind,
};

/// Статическая причина profile exclusion без runtime/provider failure semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileExclusionReason {
    /// DRM либо encrypted-only path.
    Drm,
    /// Для открытия требуется несериализуемое live extractor state.
    RequiresLiveExtractorState,
    /// Transport сознательно исключён roadmap-ом.
    RoadmapExcludedTransport,
    /// Container остаётся provisional до отдельного evidence.
    ProvisionalContainer,
    /// Codec family остаётся provisional до отдельного evidence.
    ProvisionalCodec,
    /// Result не является основным audio/video media.
    NonMedia,
    /// Third-party contract не входит в compatibility guarantee.
    ThirdPartyContract,
    /// Encryption scheme не входит в static profile.
    UnsupportedEncryption,
}

/// Поле normalized descriptor-а для typed metadata rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticDescriptorField {
    /// Transport identity.
    Transport,
    /// Container hints.
    Container,
    /// Video codec identity.
    VideoCodec,
    /// Audio codec identity.
    AudioCodec,
    /// Video dimensions.
    VideoDimensions,
    /// Frame rate.
    FrameRate,
    /// Audio sample rate.
    AudioSampleRate,
    /// Audio channel count.
    AudioChannels,
    /// Stream layout.
    Layout,
}

/// Причина, по которой descriptor metadata нельзя безопасно интерпретировать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticMetadataViolation {
    /// Обязательное поле отсутствует.
    Missing,
    /// Значение лежит вне named bounds.
    OutOfBounds,
    /// Два hints противоречат друг другу.
    ContradictoryHints,
    /// Codec media kind не совпадает с дорожкой.
    WrongMediaKind,
    /// Значение известно, но его семантики недостаточно для статического решения.
    Insufficient,
}

/// Typed static rejection; operational open/provider errors намеренно отсутствуют.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticCompatibilityRejection {
    /// Transport family неизвестна.
    UnknownTransport {
        /// Raw+parsed transport с safe Debug.
        transport: NormalizedTransport,
    },
    /// Container family неизвестна.
    UnknownContainer {
        /// Raw+parsed container hints с safe Debug.
        container: ContainerIdentity,
    },
    /// Известные ext/container hints конфликтуют.
    ContainerHintsConflict {
        /// Обе parsed families.
        conflict: ContainerHintConflict,
    },
    /// Codec family неизвестна либо не входит в static profile.
    UnsupportedCodec {
        /// Ожидаемый media kind.
        expected_media: CodecMediaKind,
        /// Raw+parsed codec с safe Debug.
        codec: NormalizedCodec,
    },
    /// Layout не поддерживается данным static compatibility profile.
    UnsupportedLayout {
        /// Точная shape.
        layout: StreamLayoutKind,
    },
    /// Candidate попал в явный profile exclusion.
    ProfileExcluded {
        /// Нейтральная причина.
        reason: ProfileExclusionReason,
    },
    /// Metadata нарушает named invariant.
    InvalidMetadata {
        /// Поле.
        field: StaticDescriptorField,
        /// Тип нарушения.
        violation: StaticMetadataViolation,
    },
}
