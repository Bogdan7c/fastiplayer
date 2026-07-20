use std::fmt;

use media_core::MediaDuration;
use playlist_core::PlaylistSingleImportDraft;

use crate::{M3uDocumentSource, M3uIssueSummary, M3uLineNumber};

/// Полностью классифицированный S05 parse result.
#[derive(Clone, Debug)]
pub enum M3uDocument {
    /// Generic M3U rows готовы только к будущему app transaction preflight.
    Generic(GenericM3uPreview),
    /// Network HLS должен быть передан adaptive manifest service-у.
    AdaptiveManifestReference(AdaptiveManifestReference),
    /// Local HLS распознан, но playback roadmap его явно не поддерживает.
    LocalHlsManifestUnsupported(LocalHlsManifestUnsupported),
}

/// Bounded generic M3U preview без stable IDs и queue mutation.
#[derive(Clone, Debug)]
pub struct GenericM3uPreview {
    /// Entries в exact source order; duplicates сохраняются.
    entries: Box<[GenericM3uEntryDraft]>,
    /// Recoverable bounded issues.
    issues: M3uIssueSummary,
    /// Parse остановился на item cap.
    truncated_by_item_limit: bool,
}

impl GenericM3uPreview {
    /// Создаёт immutable preview внутри parser.
    pub(crate) fn new(
        entries: Vec<GenericM3uEntryDraft>,
        issues: M3uIssueSummary,
        truncated_by_item_limit: bool,
    ) -> Self {
        Self {
            entries: entries.into_boxed_slice(),
            issues,
            truncated_by_item_limit,
        }
    }

    /// Итерирует drafts без раскрытия storage.
    pub fn entries(
        &self,
    ) -> impl ExactSizeIterator<Item = &GenericM3uEntryDraft> + DoubleEndedIterator {
        self.entries.iter()
    }

    /// Возвращает retained entry count.
    pub const fn retained_entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Возвращает bounded issue summary.
    pub const fn issues(&self) -> &M3uIssueSummary {
        &self.issues
    }

    /// Сообщает, остановил ли item budget дальнейший preview.
    pub const fn truncated_by_item_limit(&self) -> bool {
        self.truncated_by_item_limit
    }
}

/// Одна generic строка как domain draft плюс exact EXTINF metadata hint.
#[derive(Clone, Debug)]
pub struct GenericM3uEntryDraft {
    /// One-based line locator-а.
    locator_line: M3uLineNumber,
    /// ID-less domain draft.
    import_draft: PlaylistSingleImportDraft,
    /// Optional EXTINF, связанный только с этой строкой.
    extinf_hint: Option<M3uExtInfHint>,
}

impl GenericM3uEntryDraft {
    /// Создаёт entry после успешного locator/draft validation.
    pub(crate) const fn new(
        locator_line: M3uLineNumber,
        import_draft: PlaylistSingleImportDraft,
        extinf_hint: Option<M3uExtInfHint>,
    ) -> Self {
        Self {
            locator_line,
            import_draft,
            extinf_hint,
        }
    }

    /// Возвращает source line locator-а.
    pub const fn locator_line(&self) -> M3uLineNumber {
        self.locator_line
    }

    /// Возвращает готовый ID-less playlist-domain draft.
    pub const fn import_draft(&self) -> &PlaylistSingleImportDraft {
        &self.import_draft
    }

    /// Возвращает exact generic EXTINF hint.
    pub const fn extinf_hint(&self) -> Option<&M3uExtInfHint> {
        self.extinf_hint.as_ref()
    }
}

/// Generic EXTINF hint, не превращающий negative duration в playback span.
#[derive(Clone, Debug, PartialEq)]
pub struct M3uExtInfHint {
    /// Known positive/zero либо explicit unknown duration.
    duration: M3uDurationHint,
    /// Optional human-readable title.
    display_title: Option<String>,
}

impl M3uExtInfHint {
    /// Создаёт parsed hint.
    pub(crate) fn new(duration: M3uDurationHint, display_title: Option<String>) -> Self {
        Self {
            duration,
            display_title,
        }
    }

    /// Возвращает typed duration semantics.
    pub const fn duration(&self) -> M3uDurationHint {
        self.duration
    }

    /// Возвращает display title.
    pub fn display_title(&self) -> Option<&str> {
        self.display_title.as_deref()
    }
}

/// Duration semantics generic M3U dialect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum M3uDurationHint {
    /// Non-negative finite duration представима в neutral duration.
    Known(MediaDuration),
    /// Negative duration означает unknown, а не playback end.
    Unknown,
}

/// HLS topology, определённая до generic EXTINF interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsManifestTopology {
    /// Master Playlist references Media Playlists/renditions.
    Master,
    /// Media Playlist references segments, которые не являются queue rows.
    Media,
}

/// Network HLS handoff без segment rows.
#[derive(Clone, Debug)]
pub struct AdaptiveManifestReference {
    /// Exact manifest identity.
    manifest_source: M3uDocumentSource,
    /// Strictly validated topology.
    topology: HlsManifestTopology,
}

impl AdaptiveManifestReference {
    /// Создаёт network-only handoff.
    pub(crate) const fn new(
        manifest_source: M3uDocumentSource,
        topology: HlsManifestTopology,
    ) -> Self {
        Self {
            manifest_source,
            topology,
        }
    }

    /// Возвращает manifest source explicit adaptive owner-у.
    pub const fn manifest_source(&self) -> &M3uDocumentSource {
        &self.manifest_source
    }

    /// Возвращает validated topology.
    pub const fn topology(&self) -> HlsManifestTopology {
        self.topology
    }
}

/// Typed local-HLS rejection после успешной strict classification.
#[derive(Clone, Debug)]
pub struct LocalHlsManifestUnsupported {
    /// Exact local manifest identity.
    manifest_source: M3uDocumentSource,
    /// Strictly validated topology.
    topology: HlsManifestTopology,
}

impl LocalHlsManifestUnsupported {
    /// Создаёт local-only outcome.
    pub(crate) const fn new(
        manifest_source: M3uDocumentSource,
        topology: HlsManifestTopology,
    ) -> Self {
        Self {
            manifest_source,
            topology,
        }
    }

    /// Возвращает local source explicit presentation owner-у.
    pub const fn manifest_source(&self) -> &M3uDocumentSource {
        &self.manifest_source
    }

    /// Возвращает validated topology.
    pub const fn topology(&self) -> HlsManifestTopology {
        self.topology
    }
}

/// Fatal document-level parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3uParseError {
    /// Stable category без raw document content.
    kind: M3uParseErrorKind,
}

impl M3uParseError {
    /// Создаёт safe fatal error.
    pub(crate) const fn new(kind: M3uParseErrorKind) -> Self {
        Self { kind }
    }

    /// Возвращает typed failure category.
    pub const fn kind(&self) -> M3uParseErrorKind {
        self.kind
    }
}

/// Fatal parse taxonomy generic/HLS boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M3uParseErrorKind {
    /// Byte slice превышает caller budget.
    DocumentLimitExceeded,
    /// Document не является strict UTF-8.
    InvalidUtf8,
    /// HLS запрещает BOM.
    HlsBomNotAllowed,
    /// Generic UTF-8 M3U8 запрещает BOM; warning разрешён только M3U.
    GenericM3u8BomNotAllowed,
    /// HLS line превышает caller budget.
    HlsLineLimitExceeded {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS содержит lone CR либо иной line-ending violation.
    HlsInvalidLineEnding {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS содержит запрещённый control character.
    HlsControlCharacter {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS text не находится в NFC.
    HlsNotNfc,
    /// `#EXTM3U` отсутствует на физической первой строке.
    HlsMissingHeader,
    /// HLS tag case не соответствует RFC.
    HlsInvalidTagCase {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS содержит whitespace вне явно разрешённого значения.
    HlsWhitespaceNotAllowed {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS tag/value grammar malformed.
    HlsInvalidTagSyntax {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// HLS attribute name повторён в одном attribute-list.
    HlsDuplicateAttribute {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// RFC singleton tag повторён.
    HlsDuplicateTag {
        /// One-based line number второго tag.
        line: M3uLineNumber,
    },
    /// URI line/attribute не разрешается относительно manifest base.
    HlsInvalidUri {
        /// One-based line number.
        line: M3uLineNumber,
    },
    /// Master и Media tags смешаны.
    HlsMixedTopology,
    /// HLS marker есть, но topology нельзя доказать.
    HlsUnknownTopology,
    /// Обязательная связь tag→URI нарушена.
    HlsInvalidRequiredStructure {
        /// One-based line number, где нарушение стало observable.
        line: M3uLineNumber,
    },
}

impl fmt::Display for M3uParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "playlist document rejected: {:?}", self.kind)
    }
}

impl std::error::Error for M3uParseError {}
