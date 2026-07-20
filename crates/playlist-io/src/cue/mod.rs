//! CUE AUDIO subset §D11: bounded bytes → preview + ID-less domain drafts.

mod encoding;
mod limits;
mod model;
mod parser;

use std::fmt;
use std::path::{Path, PathBuf};

use playlist_core::{DurableReopenLocator, LocalLocator};

pub use limits::{
    CueParserLimits, CueParserLimitsError, DEFAULT_MAX_CUE_DOCUMENT_BYTES, DEFAULT_MAX_CUE_FILES,
    DEFAULT_MAX_CUE_LINE_BYTES, DEFAULT_MAX_CUE_RETAINED_TEXT_BYTES,
    DEFAULT_MAX_CUE_UNKNOWN_COMMANDS,
};
pub use model::{
    CUE_FRAMES_PER_SECOND, CueDocument, CueExportIneligibility, CueFile, CueFileType,
    CueFileTypeKind, CueIndex, CueLineNumber, CueParseError, CueParseErrorKind, CueTextEncoding,
    CueTimestamp, CueTrack, CueUnknownCommand,
};
pub use parser::parse_cue_document;

/// Exact local identity CUE document.
#[derive(Clone, PartialEq, Eq)]
pub struct CueDocumentSource {
    path: PathBuf,
}

impl CueDocumentSource {
    /// Создаёт source без filesystem access либо lossy normalization.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Возвращает exact native source path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Строит durable document root provenance.
    pub(crate) fn durable_root(&self) -> DurableReopenLocator {
        DurableReopenLocator::local(LocalLocator::Native(self.path.clone()))
    }
}

impl fmt::Debug for CueDocumentSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CueDocumentSource")
            .field("path", &"<redacted>")
            .finish()
    }
}

/// Self-documenting parse request без hidden I/O/config defaults.
pub struct CueParseRequest<'document> {
    document_bytes: &'document [u8],
    source: CueDocumentSource,
    limits: CueParserLimits,
}

impl<'document> CueParseRequest<'document> {
    /// Создаёт bounded request из caller-owned bytes и exact local identity.
    pub fn new(
        document_bytes: &'document [u8],
        source: CueDocumentSource,
        limits: CueParserLimits,
    ) -> Self {
        Self {
            document_bytes,
            source,
            limits,
        }
    }

    pub(crate) fn into_parts(self) -> (&'document [u8], CueDocumentSource, CueParserLimits) {
        (self.document_bytes, self.source, self.limits)
    }
}
