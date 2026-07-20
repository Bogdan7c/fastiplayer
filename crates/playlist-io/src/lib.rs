//! Нейтральный bounded boundary импорта и экспорта playlist-документов.
//!
//! S05 намеренно реализует только generic M3U dialect и строгую HLS
//! classification. Crate не читает filesystem/network, не запускает service,
//! не probe-ит media и не мутирует canonical queue.

mod hls;
mod issue;
mod limits;
mod locator;
mod m3u;
mod model;
mod source;

pub use issue::{M3uImportIssue, M3uImportIssueKind, M3uIssueSummary, M3uLineNumber};
pub use limits::{
    DEFAULT_MAX_M3U_DOCUMENT_BYTES, DEFAULT_MAX_M3U_ISSUES, DEFAULT_MAX_M3U_ITEMS,
    DEFAULT_MAX_M3U_LINE_BYTES, M3uParserLimits, M3uParserLimitsError,
};
pub use m3u::{M3uDeclaredFormat, M3uParseRequest, parse_m3u_document};
pub use model::{
    AdaptiveManifestReference, GenericM3uEntryDraft, GenericM3uPreview, HlsManifestTopology,
    LocalHlsManifestUnsupported, M3uDocument, M3uDurationHint, M3uExtInfHint, M3uParseError,
    M3uParseErrorKind,
};
pub use source::{M3uDocumentSource, M3uDocumentSourceError};
