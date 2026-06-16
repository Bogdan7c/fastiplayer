//! Decoder-thread scaffold for future FFmpeg send/receive bridge.

use thiserror::Error;

/// Config object exists now so future fields do not leak through `player-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FfmpegDecoderThreadConfig;

/// Startup/decode errors owned by the FFmpeg backend layer.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegDecoderThreadError {
    /// Build собран без feature `ffmpeg`, поэтому raw FFmpeg backend недоступен.
    #[error("FFmpeg decoder thread unavailable because feature `ffmpeg` is disabled")]
    FeatureDisabled,

    /// Crate scaffold уже существует, но send/receive decode ещё не реализован.
    #[error("FFmpeg decoder thread scaffold exists, but decode is not implemented yet")]
    DecodeNotImplemented,
}

/// Явная точка будущего запуска thread-а; сейчас она не создаёт fake decoder.
pub fn start_decoder_thread(
    _config: FfmpegDecoderThreadConfig,
) -> Result<(), FfmpegDecoderThreadError> {
    if cfg!(feature = "ffmpeg") {
        Err(FfmpegDecoderThreadError::DecodeNotImplemented)
    } else {
        Err(FfmpegDecoderThreadError::FeatureDisabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_thread_scaffold_returns_typed_unavailable_error() {
        let error = start_decoder_thread(FfmpegDecoderThreadConfig)
            .expect_err("scaffold must not pretend that decode is implemented");

        if cfg!(feature = "ffmpeg") {
            assert_eq!(error, FfmpegDecoderThreadError::DecodeNotImplemented);
        } else {
            assert_eq!(error, FfmpegDecoderThreadError::FeatureDisabled);
        }
    }
}
