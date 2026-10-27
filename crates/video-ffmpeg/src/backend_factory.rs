//! `video-backend-api` factory scaffold for the future FFmpeg backend.

use thiserror::Error;
use video_backend_api::{StartedVideoBackend, VideoBackendFactory};
use video_core::{HostFrameMemoryKind, VideoDecoderThreadConfig};

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

    /// Создаёт software factory с теми же neutral runtime limits, что выбрал app config.
    #[must_use]
    pub fn new_with_decoder_config(
        decoder_thread_config: impl Into<VideoDecoderThreadConfig>,
    ) -> Self {
        Self {
            decoder_config: FfmpegDecoderThreadConfig::from_thread_config(
                decoder_thread_config.into(),
            ),
        }
    }

    /// Возвращает factory, чей декодер выделяет блоки кадров памятью `host_frame_memory`.
    ///
    /// Вид памяти выбирает приложение по семейству рендера; без вызова — анонимная.
    #[must_use]
    pub fn with_host_frame_memory(mut self, host_frame_memory: HostFrameMemoryKind) -> Self {
        self.decoder_config = self
            .decoder_config
            .with_host_frame_memory(host_frame_memory);
        self
    }

    /// Вид памяти блоков кадров, который получит запущенный декодер.
    #[must_use]
    pub const fn host_frame_memory(&self) -> HostFrameMemoryKind {
        self.decoder_config.host_frame_memory()
    }

    /// Стартует FFmpeg backend и возвращает playback-facing artifact для composition root-а.
    pub fn start_for_composition(&self) -> anyhow::Result<StartedVideoBackend> {
        <Self as VideoBackendFactory>::start_video_backend(self)
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
    fn factory_host_frame_memory_reaches_decoder_config_and_defaults_to_anonymous() {
        let default_factory = FfmpegSoftwareVideoBackendFactory::new();
        let memfd_factory =
            default_factory.with_host_frame_memory(HostFrameMemoryKind::SealedMemfd);

        assert_eq!(
            default_factory.host_frame_memory(),
            HostFrameMemoryKind::Anonymous
        );
        assert_eq!(
            memfd_factory.host_frame_memory(),
            HostFrameMemoryKind::SealedMemfd
        );
    }

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
