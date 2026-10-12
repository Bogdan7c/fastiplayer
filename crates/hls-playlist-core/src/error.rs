use crate::HlsLineNumber;

/// Стабильная lexical/structural таксономия parser без раскрытия raw URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsParseErrorKind {
    DocumentLimitExceeded,
    InvalidUtf8,
    BomNotAllowed,
    LineLimitExceeded { line: HlsLineNumber },
    InvalidLineEnding { line: HlsLineNumber },
    ControlCharacter { line: HlsLineNumber },
    NotNfc,
    MissingHeader,
    InvalidTagCase { line: HlsLineNumber },
    WhitespaceNotAllowed { line: HlsLineNumber },
    InvalidTagSyntax { line: HlsLineNumber },
    DuplicateAttribute { line: HlsLineNumber },
    DuplicateTag { line: HlsLineNumber },
    InvalidReference { line: HlsLineNumber },
    MixedTopology,
    UnknownTopology,
    InvalidRequiredStructure { line: HlsLineNumber },
    SegmentLimitExceeded,
    VariantLimitExceeded,
    RenditionLimitExceeded,
    AttributeLimitExceeded { line: HlsLineNumber },
}

/// Parser error намеренно не содержит manifest/reference text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("HLS manifest failed bounded lexical/structural validation")]
pub struct HlsParseError {
    kind: HlsParseErrorKind,
}

impl HlsParseError {
    pub(crate) const fn new(kind: HlsParseErrorKind) -> Self {
        Self { kind }
    }

    /// Возвращает безопасную machine-readable identity ошибки.
    pub const fn kind(self) -> HlsParseErrorKind {
        self.kind
    }
}

/// Отклонение initial VOD compatibility profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsProfileError {
    #[error("Master playlist is not a media VOD playlist")]
    MasterPlaylist,
    #[error("Media playlist contains no EXT-X-ENDLIST")]
    NonVod,
    #[error("Media playlist has ended and is not an initial live snapshot")]
    EndedLivePlaylist,
    #[error("EXT-X-PLAYLIST-TYPE:VOD is incompatible with initial live profile")]
    LivePlaylistType,
    #[error("HLS encryption method is not included in the initial profile.")]
    UnsupportedEncryptionMethod,
    #[error("HLS key format is not equal to identity.")]
    UnsupportedKeyFormat,
    #[error("EXT-X-VERSION exceeds RFC 8216 compatibility version 7")]
    UnsupportedProtocolVersion,
    #[error("LL-HLS semantics are not included in the initial profile")]
    LowLatencySemantics,
    #[error("I-frame-only playlist is not included in the initial profile")]
    IFramesOnly,
    #[error("I-frame variant is not included in the initial profile")]
    IFrameVariant,
    #[error("EXT-X-SESSION-KEY does not have a downstream owner in the initial profile")]
    SessionKey,
    #[error("Alternate VIDEO rendition is outside the initial profile")]
    VideoRendition,
    #[error("Closed-caption rendition/group is outside the initial profile")]
    ClosedCaptions,
    #[error("EXT-X-START is not included in the initial profile.")]
    StartOffset,
    #[error("variant requires unsupported output-protection semantics")]
    OutputProtection,
    #[error("Variable substitution is outside the initial profile")]
    VariableSubstitution,
    #[error("Content steering is outside the initial profile")]
    ContentSteering,
    #[error("fMP4 media segment does not have a valid EXT-X-MAP")]
    FragmentedMp4MapRequired,
}
