//! Bounded deterministic DFS owner локальных playlist includes.

mod engine;
mod model;

pub use engine::{LocalPlaylistExpansionRequest, expand_local_playlist};
pub use model::{
    DEFAULT_MAX_LOCAL_EXPANSION_BYTES, DEFAULT_MAX_LOCAL_EXPANSION_DEPTH,
    DEFAULT_MAX_LOCAL_EXPANSION_DIAGNOSTICS, DEFAULT_MAX_LOCAL_EXPANSION_DOCUMENTS,
    DEFAULT_MAX_LOCAL_EXPANSION_ITEMS, DepthFirstExpandedEntries, ExpandedLocalPlaylistDocument,
    ExpandedLocalPlaylistEntry, LocalPlaylistDocumentFormat, LocalPlaylistExpansion,
    LocalPlaylistExpansionCancellation, LocalPlaylistExpansionIssue,
    LocalPlaylistExpansionIssueKind, LocalPlaylistExpansionLimits,
    LocalPlaylistExpansionLimitsError, LocalPlaylistExpansionStartError,
    LocalPlaylistExpansionSummary, UnexpandedLocalPlaylistInclude,
};
