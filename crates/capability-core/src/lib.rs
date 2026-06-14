//! Сводная модель системных возможностей и selection по аппаратному decode.
//!
//! `capability-core` не открывает VA-API сам. Backend crate'ы предоставляют
//! probe providers, а этот crate агрегирует результат и объясняет, почему stream
//! можно или нельзя декодировать.

#![forbid(unsafe_code)]

mod report;
mod selection;

pub use report::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    CapabilityScanner, CapabilitySchemaVersion, DriverQuirk, SystemCapabilities,
    VideoCapabilityProvider, VideoExportPath,
};
pub use selection::{
    SelectedVideoStream, StreamSelectionError, UnsupportedVideoRequirement,
    VideoCapabilityRejection, VideoStreamCandidate,
};
pub use video_frame_contract::DmaBufImageLayout;
