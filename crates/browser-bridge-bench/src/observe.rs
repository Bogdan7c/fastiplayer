//! Наблюдение за результатом: журнал плеера («кадр дошёл до показа») и свидетельства
//! посредника в stderr браузера.

mod error;
mod host_evidence;
mod player_log;

pub use error::ObserveError;
pub use host_evidence::{HostLogEvidence, parse_host_evidence, read_host_evidence};
pub use player_log::{
    EndNotice, FrameEvidence, FrameGrowth, PlayerLog, parse_end_notice, parse_summary,
};
