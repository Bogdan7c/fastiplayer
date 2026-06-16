//! `video-backend-api` factory scaffold for the future FFmpeg backend.

use thiserror::Error;
use video_backend_api::{StartedVideoBackend, VideoBackendFactory};

use crate::decoder_thread::{FfmpegDecoderThreadConfig, FfmpegDecoderThreadError};

/// Concrete factory type that keeps future startup wiring outside `player-core`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FfmpegVideoBackendFactory {
    /// Config remains crate-owned; callers should not assemble FFmpeg internals.
    decoder_config: FfmpegDecoderThreadConfig,
}

impl FfmpegVideoBackendFactory {
    /// Создаёт factory с default scaffold config.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            decoder_config: FfmpegDecoderThreadConfig,
        }
    }
}

impl VideoBackendFactory for FfmpegVideoBackendFactory {
    /// Возвращает typed unavailable error до реализации decoder thread-а.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        crate::decoder_thread::start_decoder_thread(self.decoder_config)
            .map_err(FfmpegBackendFactoryError::from)?;

        Err(FfmpegBackendFactoryError::DecodeThreadDidNotReturnHandle.into())
    }
}

/// Ошибки startup boundary, которые не раскрывают FFmpeg raw pointers.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegBackendFactoryError {
    /// Decoder thread не стартовал по typed причине.
    #[error(transparent)]
    DecoderThread(#[from] FfmpegDecoderThreadError),

    /// Защита от невозможного состояния в scaffold-е.
    #[error("FFmpeg decoder thread startup did not return a backend handle")]
    DecodeThreadDidNotReturnHandle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_factory_reports_unavailable_instead_of_creating_fake_backend() {
        let factory = FfmpegVideoBackendFactory::new();
        let result = factory.start_video_backend();

        assert!(
            result.is_err(),
            "scaffold must not start a fake FFmpeg backend"
        );

        let error = result.err().unwrap();

        assert!(error.downcast_ref::<FfmpegBackendFactoryError>().is_some());
    }
}
