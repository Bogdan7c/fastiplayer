//! Typed XSPF failures без raw XML, path или URI payload.

use std::fmt;

use bounded_xml_reader::XmlReadError;

/// Typed XSPF parse failure без raw XML/URI payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XspfParseError {
    /// Kind остаётся единственным diagnostic payload.
    kind: XspfParseErrorKind,
}

impl XspfParseError {
    /// Создаёт secret-safe error.
    pub(crate) const fn new(kind: XspfParseErrorKind) -> Self {
        Self { kind }
    }

    /// Сохраняет exact XML security/budget distinction.
    pub(crate) fn from_xml(error: XmlReadError) -> Self {
        Self::new(XspfParseErrorKind::Xml(error))
    }

    /// Возвращает typed kind caller-у.
    pub const fn kind(&self) -> &XspfParseErrorKind {
        &self.kind
    }
}

impl fmt::Display for XspfParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "XSPF document rejected: {}", self.kind)
    }
}

impl std::error::Error for XspfParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            XspfParseErrorKind::Xml(error) => Some(error),
            _ => None,
        }
    }
}

/// Ошибки разделяют XML security, XSPF schema, URI и resource limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XspfParseErrorKind {
    /// Hardened XML boundary rejected bytes before schema interpretation.
    Xml(XmlReadError),
    /// Root не является non-empty XSPF playlist.
    InvalidRoot,
    /// Exact XSPF version `1` отсутствует.
    UnsupportedVersion,
    /// Element находится в неправильном namespace.
    UnexpectedNamespace,
    /// Element не разрешён текущей schema position.
    UnexpectedElement,
    /// Attribute не разрешён exact element schema.
    UnexpectedAttribute,
    /// Required unqualified attribute отсутствует.
    MissingRequiredAttribute,
    /// Optional child повторён.
    DuplicateChild,
    /// Child появился раньше уже пройденного schema rank-а.
    ChildOrderViolation,
    /// Root не содержит ровно один mandatory trackList.
    MissingTrackList,
    /// Container содержит non-whitespace character data.
    TextNotAllowed,
    /// Plain-text XSPF element содержит nested markup.
    MarkupNotAllowed,
    /// XML закончился внутри expected schema scope.
    UnexpectedEof,
    /// Initial local/network XML Base нельзя построить без догадки.
    DocumentBaseUnavailable,
    /// URI spelling/base resolution некорректны.
    InvalidUri,
    /// Bounded integer lexical form либо range некорректны.
    InvalidInteger,
    /// Flattened track budget исчерпан.
    TrackLimitExceeded,
    /// Per-track ordered location budget исчерпан.
    LocationLimitExceeded,
    /// Fastiplayer group-record budget исчерпан.
    GroupLimitExceeded,
    /// Track budget превышает canonical retained capacity.
    TrackLimitExceedsDomainCapacity,
    /// Group budget превышает canonical top-level capacity.
    GroupLimitExceedsDomainCapacity,
    /// Known Fastiplayer extension объявлен больше одного раза.
    DuplicateFastiplayerExtension,
    /// Known playlist extension ошибочно помещён внутрь track.
    FastiplayerExtensionWrongScope,
    /// Group не содержит mandatory root location.
    MissingGroupLocation,
    /// Group range выходит за tracks, перекрывается или нарушает order.
    InvalidGroupRange,
}

impl fmt::Display for XspfParseErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(error) => write!(formatter, "{error}"),
            Self::InvalidRoot => formatter.write_str("некорректный root"),
            Self::UnsupportedVersion => formatter.write_str("неподдерживаемая version"),
            Self::UnexpectedNamespace => formatter.write_str("неожиданный namespace"),
            Self::UnexpectedElement => formatter.write_str("неожиданный element"),
            Self::UnexpectedAttribute => formatter.write_str("неожиданный attribute"),
            Self::MissingRequiredAttribute => formatter.write_str("отсутствует required attribute"),
            Self::DuplicateChild => formatter.write_str("повторён singleton child"),
            Self::ChildOrderViolation => formatter.write_str("нарушен порядок children"),
            Self::MissingTrackList => formatter.write_str("отсутствует mandatory trackList"),
            Self::TextNotAllowed => formatter.write_str("character data запрещены в container"),
            Self::MarkupNotAllowed => formatter.write_str("nested markup запрещён"),
            Self::UnexpectedEof => formatter.write_str("неожиданный конец XML"),
            Self::DocumentBaseUnavailable => formatter.write_str("document base недоступен"),
            Self::InvalidUri => formatter.write_str("URI некорректен"),
            Self::InvalidInteger => formatter.write_str("nonNegativeInteger некорректен"),
            Self::TrackLimitExceeded => formatter.write_str("track budget исчерпан"),
            Self::LocationLimitExceeded => formatter.write_str("location budget исчерпан"),
            Self::GroupLimitExceeded => formatter.write_str("group budget исчерпан"),
            Self::TrackLimitExceedsDomainCapacity => {
                formatter.write_str("track budget превышает domain capacity")
            }
            Self::GroupLimitExceedsDomainCapacity => {
                formatter.write_str("group budget превышает domain capacity")
            }
            Self::DuplicateFastiplayerExtension => {
                formatter.write_str("Fastiplayer extension повторён")
            }
            Self::FastiplayerExtensionWrongScope => {
                formatter.write_str("Fastiplayer extension находится в неправильном scope")
            }
            Self::MissingGroupLocation => formatter.write_str("group location отсутствует"),
            Self::InvalidGroupRange => formatter.write_str("group range некорректен"),
        }
    }
}
