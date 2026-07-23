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
#[error("HLS manifest не прошёл bounded lexical/structural validation")]
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
    #[error("master playlist не является media VOD playlist")]
    MasterPlaylist,
    #[error("media playlist не содержит EXT-X-ENDLIST")]
    NonVod,
    #[error("HLS encryption method не входит в initial profile")]
    UnsupportedEncryptionMethod,
    #[error("HLS key format не равен identity")]
    UnsupportedKeyFormat,
    #[error("EXT-X-VERSION превышает RFC 8216 compatibility version 7")]
    UnsupportedProtocolVersion,
    #[error("LL-HLS semantics не входят в initial profile")]
    LowLatencySemantics,
    #[error("I-frame-only playlist не входит в initial profile")]
    IFramesOnly,
    #[error("I-frame variant не входит в initial profile")]
    IFrameVariant,
    #[error("EXT-X-SESSION-KEY не имеет downstream owner в initial profile")]
    SessionKey,
    #[error("alternate VIDEO rendition не входит в initial profile")]
    VideoRendition,
    #[error("closed-caption rendition/group не входит в initial profile")]
    ClosedCaptions,
    #[error("EXT-X-START не входит в initial profile")]
    StartOffset,
    #[error("variant требует неподдерживаемую output-protection semantics")]
    OutputProtection,
    #[error("variable substitution не входит в initial profile")]
    VariableSubstitution,
    #[error("content steering не входит в initial profile")]
    ContentSteering,
    #[error("EVENT playlist не является VOD profile")]
    EventPlaylist,
    #[error("fMP4 media segment не имеет действующего EXT-X-MAP")]
    FragmentedMp4MapRequired,
}
