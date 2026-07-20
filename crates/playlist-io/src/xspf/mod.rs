//! Namespace-aware XSPF v1 model поверх hardened XML boundary.

// Export module владеет только URI eligibility, но не queue snapshot или file writing.
mod export;
// Error module сохраняет typed XML/schema/URI distinctions без input payload.
mod error;
// Extension module владеет minimal non-duplicating compound group schema.
mod extension;
// Limits module связывает format policy с обязательными XML budgets.
mod limits;
// Model module хранит bounded parser result без app admission authority.
mod model;
// Parser module реализует streaming schema state machine без DOM.
mod parser;
// Schema module владеет compact child-order state и bounded numeric lexical rules.
mod schema;
// URI module централизует strict references, inherited xml:base и document base.
mod uri;

// Public constants фиксируют exact XSPF и Rustiplayer extension namespaces.
pub const XSPF_NAMESPACE: &str = "http://xspf.org/ns/0/";
// Version входит в URI, поэтому несовместимая child schema потребует новый namespace.
pub const RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE: &str = "urn:rustiplayer:xspf:playlist-extension:1";

// Export facade не раскрывает url::Url и secret-bearing formatting.
pub use export::{XspfExportIneligible, XspfExportLocation};
// Parse errors публикуются отдельно от concrete parser layout.
pub use error::{XspfParseError, XspfParseErrorKind};
// Limits facade позволяет caller-у осознанно выбрать полный bounded profile.
pub use limits::{
    DEFAULT_MAX_XSPF_ATTRIBUTE_BYTES, DEFAULT_MAX_XSPF_ATTRIBUTE_COUNT,
    DEFAULT_MAX_XSPF_ATTRIBUTES_PER_ELEMENT, DEFAULT_MAX_XSPF_DEPTH,
    DEFAULT_MAX_XSPF_DOCUMENT_BYTES, DEFAULT_MAX_XSPF_GROUPS, DEFAULT_MAX_XSPF_LOCATIONS_PER_TRACK,
    DEFAULT_MAX_XSPF_NAMESPACE_BYTES, DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS,
    DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS_PER_ELEMENT, DEFAULT_MAX_XSPF_TEXT_BYTES,
    DEFAULT_MAX_XSPF_TOKENS, DEFAULT_MAX_XSPF_TRACKS, XspfParserLimits,
};
// Result model сохраняет candidates и hints, но не выбирает playback service.
pub use model::{
    XspfGroup, XspfGroupTrackCount, XspfLocationCandidate, XspfPlaylist, XspfTrack, XspfTrackIndex,
};
// Parse facade является единственным XSPF import entry point.
pub use parser::{XspfParseRequest, parse_xspf_document};
