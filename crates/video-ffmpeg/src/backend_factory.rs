//! `video-backend-api` factory scaffold for the future FFmpeg backend.

use thiserror::Error;
use video_backend_api::{StartedVideoBackend, VideoBackendFactory};

use crate::decoder_thread::{FfmpegDecoderThreadConfig, FfmpegDecoderThreadError};

/// Concrete software factory type that keeps startup wiring outside `player-core`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FfmpegSoftwareVideoBackendFactory {
    /// Config остаётся внутри crate-а; caller не собирает FFmpeg internals руками.
    decoder_config: FfmpegDecoderThreadConfig,
}

impl FfmpegSoftwareVideoBackendFactory {
    /// Создаёт software factory с default decoder-thread config.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder_config: FfmpegDecoderThreadConfig::default(),
        }
    }
}

impl VideoBackendFactory for FfmpegSoftwareVideoBackendFactory {
    /// Стартует playback-facing FFmpeg decoder thread без раскрытия FFmpeg internals.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        crate::decoder_thread::start_decoder_thread(self.decoder_config)
            .map_err(FfmpegBackendFactoryError::from)
            .map_err(Into::into)
    }
}

/// Ошибки startup boundary, которые не раскрывают FFmpeg raw pointers.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegBackendFactoryError {
    /// Decoder thread не стартовал по typed причине.
    #[error(transparent)]
    DecoderThread(#[from] FfmpegDecoderThreadError),
}

/// Backward-compatible имя старого scaffold-а.
pub type FfmpegVideoBackendFactory = FfmpegSoftwareVideoBackendFactory;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_factory_reports_unavailable_instead_of_creating_fake_backend() {
        let factory = FfmpegSoftwareVideoBackendFactory::new();
        let result = factory.start_video_backend();

        if cfg!(feature = "ffmpeg") {
            assert!(
                result.is_ok(),
                "FFmpeg build should start a real decoder-thread handle"
            );
            return;
        }

        let error = result.err().expect("default build has no FFmpeg FFI");

        assert!(error.downcast_ref::<FfmpegBackendFactoryError>().is_some());
    }
}
