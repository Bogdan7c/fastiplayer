//! Конфиг потока декодера FFmpeg.

use video_core::{HostFrameMemoryKind, VideoDecoderThreadConfig};

/// Config object exists now so future fields do not leak through `player-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegDecoderThreadConfig {
    /// Backend-neutral channel limits and flush timeout.
    thread_config: VideoDecoderThreadConfig,

    /// Какой памятью декодер выделяет блоки кадров. Выбирает приложение по семейству
    /// рендера (GLES2 — memfd для dma-buf, wgpu — анонимная); по умолчанию анонимная.
    host_frame_memory: HostFrameMemoryKind,
}

impl FfmpegDecoderThreadConfig {
    /// Создаёт FFmpeg config из уже выбранных neutral decoder-thread limits.
    #[must_use]
    pub fn from_thread_config(thread_config: VideoDecoderThreadConfig) -> Self {
        Self {
            thread_config: thread_config.normalized(),
            host_frame_memory: HostFrameMemoryKind::default(),
        }
    }

    /// Возвращает config с другим видом памяти блоков кадров; остальное не меняется.
    #[must_use]
    pub const fn with_host_frame_memory(mut self, host_frame_memory: HostFrameMemoryKind) -> Self {
        self.host_frame_memory = host_frame_memory;
        self
    }

    /// Возвращает normalized neutral runtime config.
    #[must_use]
    pub const fn thread_config(self) -> VideoDecoderThreadConfig {
        self.thread_config
    }

    /// Вид памяти блоков кадров.
    #[must_use]
    pub const fn host_frame_memory(self) -> HostFrameMemoryKind {
        self.host_frame_memory
    }
}

impl Default for FfmpegDecoderThreadConfig {
    fn default() -> Self {
        Self::from_thread_config(VideoDecoderThreadConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_keeps_anonymous_memory() {
        assert_eq!(
            FfmpegDecoderThreadConfig::default().host_frame_memory(),
            HostFrameMemoryKind::Anonymous
        );
    }

    #[test]
    fn with_host_frame_memory_changes_only_memory_kind() {
        let base = FfmpegDecoderThreadConfig::default();

        let memfd = base.with_host_frame_memory(HostFrameMemoryKind::SealedMemfd);

        assert_eq!(memfd.host_frame_memory(), HostFrameMemoryKind::SealedMemfd);
        assert_eq!(memfd.thread_config(), base.thread_config());
        // Возврат к анонимной памяти восстанавливает исходный config.
        assert_eq!(
            memfd.with_host_frame_memory(HostFrameMemoryKind::Anonymous),
            base
        );
    }
}
