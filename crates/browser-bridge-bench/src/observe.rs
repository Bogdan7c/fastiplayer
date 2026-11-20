//! Наблюдение за результатом: журнал плеера («кадр дошёл до показа») и свидетельства
//! посредника в stderr браузера.

mod error;
mod extension_notes;
mod frame_growth;
mod host_evidence;
mod log_time;
mod media_switch_log;
mod page_subtitle_log;
mod playback_end;
mod player_log;
mod quality_log;
mod stream_generation_log;
mod summary_cadence;
mod summary_digest;

pub use error::ObserveError;
pub use extension_notes::{EXTENSION_NOTE_MARKER, parse_extension_notes, read_extension_notes};
pub use frame_growth::{FrameGrowth, FrameGrowthRule, PLAYBACK_GROWTH_WINDOW, find_frame_growth};
pub use host_evidence::{HostLogEvidence, parse_host_evidence, read_host_evidence};
pub use log_time::{PlayerLogTime, parse_log_time};
pub use media_switch_log::{
    MEDIA_SWITCH_MARKER, MediaSwitch, SwitchEnd, SwitchOutcome, SwitchRelease, parse_media_switch,
};
pub use page_subtitle_log::{
    PAGE_SUBTITLE_DOCUMENT_MARKER, PAGE_SUBTITLE_TRACK_READY_MARKER, PageSubtitleDocumentLine,
    PageSubtitleTrackLine, max_subtitle_overlay_frames, parse_page_subtitle_document,
    parse_page_subtitle_track_ready,
};
pub use playback_end::{PLAYBACK_ENDED_MARKER, parse_playback_ended_frames};
pub use player_log::{
    EndNotice, FrameEvidence, PlayerLog, parse_end_notice, parse_loads, parse_summary,
};
pub use quality_log::{
    BROWSER_QUALITY_SHOWN_MARKER, BrowserQualityLine, QualityBitrate, parse_browser_quality_shown,
};
pub use stream_generation_log::{
    STREAM_GENERATION_LOADED_MARKER, STREAM_GENERATION_SHOWN_MARKER,
    STREAM_SURFACE_RECONFIGURED_MARKER, ShownGeneration, parse_stream_generation_loaded,
    parse_stream_generation_shown, parse_stream_surface_width, parse_target_wait_ms,
};
pub use summary_cadence::{
    PLAYER_SUMMARY_INTERVAL, PLAYER_SUMMARY_INTERVAL_VARIABLE, player_summary_interval_value,
};
pub use summary_digest::SummaryDigest;
