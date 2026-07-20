//! Нейтральный bounded boundary импорта и экспорта playlist-документов.
//!
//! Crate реализует generic M3U/HLS distinction, namespace-aware XSPF v1
//! и bounded local-only recursive expansion. Network I/O, service admission,
//! media probe и mutation canonical queue остаются за пределами crate.

mod hls;
mod issue;
mod limits;
mod local_expansion;
mod locator;
mod m3u;
mod model;
mod source;
mod xspf;

pub use issue::{M3uImportIssue, M3uImportIssueKind, M3uIssueSummary, M3uLineNumber};
pub use limits::{
    DEFAULT_MAX_M3U_DOCUMENT_BYTES, DEFAULT_MAX_M3U_ISSUES, DEFAULT_MAX_M3U_ITEMS,
    DEFAULT_MAX_M3U_LINE_BYTES, M3uParserLimits, M3uParserLimitsError,
};
pub use local_expansion::{
    DEFAULT_MAX_LOCAL_EXPANSION_BYTES, DEFAULT_MAX_LOCAL_EXPANSION_DEPTH,
    DEFAULT_MAX_LOCAL_EXPANSION_DIAGNOSTICS, DEFAULT_MAX_LOCAL_EXPANSION_DOCUMENTS,
    DEFAULT_MAX_LOCAL_EXPANSION_ITEMS, DepthFirstExpandedEntries, ExpandedLocalPlaylistDocument,
    ExpandedLocalPlaylistEntry, LocalPlaylistDocumentFormat, LocalPlaylistExpansion,
    LocalPlaylistExpansionCancellation, LocalPlaylistExpansionIssue,
    LocalPlaylistExpansionIssueKind, LocalPlaylistExpansionLimits,
    LocalPlaylistExpansionLimitsError, LocalPlaylistExpansionRequest,
    LocalPlaylistExpansionStartError, LocalPlaylistExpansionSummary,
    UnexpandedLocalPlaylistInclude, expand_local_playlist,
};
pub use m3u::{M3uDeclaredFormat, M3uParseRequest, parse_m3u_document};
pub use model::{
    AdaptiveManifestReference, GenericM3uEntryDraft, GenericM3uPreview, HlsManifestTopology,
    LocalHlsManifestUnsupported, M3uDocument, M3uDurationHint, M3uExtInfHint, M3uParseError,
    M3uParseErrorKind,
};
pub use source::{
    M3uDocumentSource, M3uDocumentSourceError, PlaylistDocumentSource, PlaylistDocumentSourceError,
    XspfDocumentSource, XspfDocumentSourceError,
};
pub use xspf::{
    DEFAULT_MAX_XSPF_ATTRIBUTE_BYTES, DEFAULT_MAX_XSPF_ATTRIBUTE_COUNT,
    DEFAULT_MAX_XSPF_ATTRIBUTES_PER_ELEMENT, DEFAULT_MAX_XSPF_DEPTH,
    DEFAULT_MAX_XSPF_DOCUMENT_BYTES, DEFAULT_MAX_XSPF_GROUPS, DEFAULT_MAX_XSPF_LOCATIONS_PER_TRACK,
    DEFAULT_MAX_XSPF_NAMESPACE_BYTES, DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS,
    DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS_PER_ELEMENT, DEFAULT_MAX_XSPF_TEXT_BYTES,
    DEFAULT_MAX_XSPF_TOKENS, DEFAULT_MAX_XSPF_TRACKS, RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE,
    XSPF_NAMESPACE, XspfExportIneligible, XspfExportLocation, XspfGroup, XspfGroupTrackCount,
    XspfLocationCandidate, XspfParseError, XspfParseErrorKind, XspfParseRequest, XspfParserLimits,
    XspfPlaylist, XspfTrack, XspfTrackIndex, parse_xspf_document,
};
