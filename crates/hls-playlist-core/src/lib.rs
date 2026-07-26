//! Чистая bounded-модель и RFC 8216 parser без I/O, demux и crypto.

mod attribute;
mod error;
mod lexical;
mod limits;
mod master;
mod media;
mod model;
mod parser;
mod profile;
mod structure;

pub use error::{HlsParseError, HlsParseErrorKind, HlsProfileError};
pub use limits::{HlsParserLimits, HlsParserLimitsError};
pub use model::{
    ByteRange, ClosedCaptionsReference, ExactReference, HlsDuration, HlsFrameRate,
    HlsKeyDeclaration, HlsKeyFormat, HlsKeyMethod, HlsLineNumber, HlsPlaylist, HlsPlaylistType,
    HlsVideoRange, InitializationMap, MasterPlaylist, MediaContainerIntent, MediaPlaylist,
    MediaRendition, MediaRenditionType, MediaSegment, VariantStream,
};
pub use parser::{HlsParseRequest, is_hls_candidate, parse_hls_playlist};
pub use profile::{
    validate_initial_profile, validate_live_profile, validate_live_refresh_profile,
    validate_vod_profile,
};
