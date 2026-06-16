//! FFmpeg software video backend scaffold.
//!
//! Crate изолирует raw FFmpeg FFI и будущие unsafe blocks от остального
//! workspace. `player-core` не зависит от этого crate-а: будущая интеграция
//! должна идти через `video-backend-api` и neutral `video-core` contracts.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod backend_factory;
pub mod codec_adapter;
pub mod decoder_thread;
pub mod ffi;
pub mod probe;
pub mod software_frame;

pub use backend_factory::{FfmpegBackendFactoryError, FfmpegVideoBackendFactory};
pub use codec_adapter::{
    FfmpegCodecAdapterError, SoftwareDecodeContractPlan, validate_software_frame_contract,
};
pub use decoder_thread::{FfmpegDecoderThreadConfig, FfmpegDecoderThreadError};
pub use probe::{FfmpegBuildStatus, FfmpegProbeReport, compile_time_probe};
pub use software_frame::{SoftwareFramePlan, SoftwareFramePlanError};

/// Canonical backend id для будущего software FFmpeg backend-а.
pub const FFMPEG_SOFTWARE_BACKEND_ID: &str = "ffmpeg-sw";

/// Возвращает typed backend id без раскрытия storage формата `codec-core`.
#[must_use]
pub fn ffmpeg_software_backend_id() -> codec_core::DecodeBackendId {
    codec_core::DecodeBackendId::new(FFMPEG_SOFTWARE_BACKEND_ID)
        .expect("ffmpeg-sw is a valid lowercase backend id")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_id_matches_planned_public_diagnostic_name() {
        let backend_id = ffmpeg_software_backend_id();

        assert_eq!(backend_id.as_str(), FFMPEG_SOFTWARE_BACKEND_ID);
    }
}
