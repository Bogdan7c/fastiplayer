//! Наблюдение за результатом: журнал плеера («кадр дошёл до показа») и свидетельства
//! посредника в stderr браузера.

mod error;
mod host_evidence;
mod page_subtitle_log;
mod player_log;
mod summary_digest;

pub use error::ObserveError;
pub use host_evidence::{HostLogEvidence, parse_host_evidence, read_host_evidence};
pub use page_subtitle_log::{
    PAGE_SUBTITLE_DOCUMENT_MARKER, PAGE_SUBTITLE_TRACK_READY_MARKER, PageSubtitleDocumentLine,
    PageSubtitleTrackLine, max_subtitle_overlay_frames, parse_page_subtitle_document,
    parse_page_subtitle_track_ready,
};
pub use player_log::{
    EndNotice, FrameEvidence, FrameGrowth, PlayerLog, parse_end_notice, parse_loads, parse_summary,
};
pub use summary_digest::SummaryDigest;
