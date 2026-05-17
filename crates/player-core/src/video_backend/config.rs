use crate::PlayerVideoDecoderThreadConfig;

/// Backend-neutral config, который selector передаёт выбранному production adapter-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoBackendStartupConfig {
    /// Runtime limits decoder thread-а без знания конкретного backend-а.
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
}

impl VideoBackendStartupConfig {
    /// Создаёт startup config из уже validated/normalized app-level decoder config.
    #[must_use]
    pub(super) fn new(decoder_thread_config: impl Into<PlayerVideoDecoderThreadConfig>) -> Self {
        Self {
            decoder_thread_config: decoder_thread_config.into(),
        }
    }

    /// Возвращает decoder-thread limits для adapter conversion.
    #[must_use]
    pub(super) fn decoder_thread_config(self) -> PlayerVideoDecoderThreadConfig {
        self.decoder_thread_config
    }
}

impl Default for VideoBackendStartupConfig {
    /// Сохраняет прежнее default поведение factory без чтения app config.
    fn default() -> Self {
        Self::new(PlayerVideoDecoderThreadConfig::default())
    }
}

/// Shared WGPU resources, нужные production video backend-ам для zero-copy path.
pub(super) struct VideoBackendWgpuContext<'a> {
    /// WGPU instance для импортов external memory.
    instance: &'a wgpu::Instance,

    /// WGPU adapter для проверки совместимости импортов.
    adapter: &'a wgpu::Adapter,

    /// WGPU device, которым владеет render shell.
    device: &'a wgpu::Device,

    /// WGPU queue для release callbacks и GPU-side синхронизации.
    queue: &'a wgpu::Queue,
}

impl<'a> VideoBackendWgpuContext<'a> {
    /// Собирает borrowed GPU handles без передачи ownership из render shell.
    #[must_use]
    fn new(
        instance: &'a wgpu::Instance,
        adapter: &'a wgpu::Adapter,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Self {
        Self {
            instance,
            adapter,
            device,
            queue,
        }
    }

    /// Возвращает WGPU instance для concrete adapter startup.
    #[must_use]
    pub(super) fn instance(&self) -> &'a wgpu::Instance {
        self.instance
    }

    /// Возвращает WGPU adapter для concrete adapter startup.
    #[must_use]
    pub(super) fn adapter(&self) -> &'a wgpu::Adapter {
        self.adapter
    }

    /// Возвращает WGPU device для concrete adapter startup.
    #[must_use]
    pub(super) fn device(&self) -> &'a wgpu::Device {
        self.device
    }

    /// Возвращает WGPU queue для concrete adapter startup.
    #[must_use]
    pub(super) fn queue(&self) -> &'a wgpu::Queue {
        self.queue
    }
}

/// Backend-neutral startup request: GPU context плюс config, но без выбора backend-а.
pub(super) struct VideoBackendStartupRequest<'a> {
    /// Borrowed GPU resources, которыми продолжает владеть render shell.
    wgpu_context: VideoBackendWgpuContext<'a>,

    /// Runtime/startup limits для выбранного production adapter-а.
    startup_config: VideoBackendStartupConfig,
}

impl<'a> VideoBackendStartupRequest<'a> {
    /// Создаёт request, который factory передаёт selector-у без concrete backend знания.
    #[must_use]
    pub(super) fn new(
        instance: &'a wgpu::Instance,
        adapter: &'a wgpu::Adapter,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        startup_config: VideoBackendStartupConfig,
    ) -> Self {
        Self {
            wgpu_context: VideoBackendWgpuContext::new(instance, adapter, device, queue),
            startup_config,
        }
    }

    /// Возвращает borrowed GPU context для adapter startup.
    #[must_use]
    pub(super) fn wgpu_context(&self) -> &VideoBackendWgpuContext<'a> {
        &self.wgpu_context
    }

    /// Возвращает decoder-thread limits без раскрытия структуры storage.
    #[must_use]
    pub(super) fn decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.startup_config.decoder_thread_config()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Проверяет, что startup config не меняет validated decoder-thread limits.
    #[test]
    fn startup_config_preserves_decoder_thread_limits() {
        let decoder_thread_config = PlayerVideoDecoderThreadConfig {
            packet_channel_frames: 2,
            frame_channel_frames: 3,
            control_channel_frames: 4,
            decoder_ready_queue_frames: 5,
            decoder_surface_pool_frames: 6,
            zero_copy_surface_pool_slots: 7,
            flush_timeout: Duration::from_millis(75),
        };

        let startup_config = VideoBackendStartupConfig::new(decoder_thread_config);

        assert_eq!(
            startup_config.decoder_thread_config(),
            decoder_thread_config
        );
    }

    /// Проверяет, что neutral startup config сохраняет прежние production defaults.
    #[test]
    fn startup_config_default_matches_decoder_thread_default() {
        assert_eq!(
            VideoBackendStartupConfig::default().decoder_thread_config(),
            PlayerVideoDecoderThreadConfig::default()
        );
    }
}
