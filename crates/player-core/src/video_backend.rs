use crate::PlayerVideoDecoderThreadConfig;
use crate::pipeline::VideoDecoderThreadHandle;

mod vaapi;

mod private {
    /// Sealed marker: внешние crates не должны создавать backend wrapper напрямую.
    pub trait Sealed {}
}

/// Фабрика video backend-а, которую session вызывает без знания деталей конкретного backend init.
pub trait VideoBackendFactory: private::Sealed {
    /// Стартует backend и возвращает уже готовый decoder thread wrapper.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend>;
}

/// Запущенный video backend, подготовленный фабрикой для установки в playback pipeline.
pub struct StartedVideoBackend {
    /// Decoder thread остаётся за neutral handle boundary.
    decoder_thread: Box<dyn VideoDecoderThreadHandle>,
}

impl StartedVideoBackend {
    /// Создаёт backend wrapper вокруг decoder thread, который уже прошёл init handshake.
    fn from_decoder_thread(decoder_thread: impl VideoDecoderThreadHandle + 'static) -> Self {
        Self {
            decoder_thread: Box::new(decoder_thread),
        }
    }

    /// Передаёт decoder handle pipeline-у без раскрытия concrete backend type.
    pub(crate) fn into_decoder_thread(self) -> Box<dyn VideoDecoderThreadHandle> {
        self.decoder_thread
    }
}

/// WGPU-backed factory для текущего hardware decode backend-а.
///
/// Название намеренно не содержит VA-API: public caller передаёт GPU handles,
/// а конкретный backend остаётся внутренним выбором player-core/video layer.
pub struct WgpuVideoBackendFactory<'a> {
    /// WGPU instance для zero-copy import path.
    instance: &'a wgpu::Instance,

    /// WGPU adapter для backend capability matching.
    adapter: &'a wgpu::Adapter,

    /// WGPU device для texture allocation.
    device: &'a wgpu::Device,

    /// WGPU queue для texture upload/release callbacks.
    queue: &'a wgpu::Queue,

    /// Bounded queue/runtime limits decoder thread-а.
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
}

impl<'a> WgpuVideoBackendFactory<'a> {
    /// Создаёт factory из GPU handles, которыми владеет shell/render layer.
    #[must_use]
    pub fn new(
        instance: &'a wgpu::Instance,
        adapter: &'a wgpu::Adapter,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Self {
        Self::new_with_decoder_config(
            instance,
            adapter,
            device,
            queue,
            PlayerVideoDecoderThreadConfig::default(),
        )
    }

    /// Создаёт factory с явным decoder-thread config из validated app config.
    #[must_use]
    pub fn new_with_decoder_config(
        instance: &'a wgpu::Instance,
        adapter: &'a wgpu::Adapter,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        decoder_thread_config: impl Into<PlayerVideoDecoderThreadConfig>,
    ) -> Self {
        Self {
            instance,
            adapter,
            device,
            queue,
            decoder_thread_config: decoder_thread_config.into(),
        }
    }
}

impl private::Sealed for WgpuVideoBackendFactory<'_> {}

impl VideoBackendFactory for WgpuVideoBackendFactory<'_> {
    /// Запускает текущий hardware decoder backend за factory boundary.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        let decoder_thread = vaapi::start_video_decoder_thread(
            self.instance,
            self.adapter,
            self.device,
            self.queue,
            self.decoder_thread_config,
        )?;

        Ok(StartedVideoBackend::from_decoder_thread(decoder_thread))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DecodeSendError, DecodeThreadError, DecoderResourceSnapshot, PlayerDecodePacket,
        RenderTextureProviderHandle,
    };

    /// Minimal fake decoder для проверки startup wrapper-а без VA-API resources.
    struct StartupFakeDecoderThread;

    impl VideoDecoderThreadHandle for StartupFakeDecoderThread {
        fn backend_name(&self) -> &'static str {
            "startup fake decoder"
        }

        fn send_packet(&self, _packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
            Err(DecodeSendError::Fatal(DecodeThreadError::new(
                "startup fake decoder does not accept packets",
            )))
        }

        fn release_frame(&self, _handle: video_core::FrameTextureHandle) {}

        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            None
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn texture_view_provider(&self) -> RenderTextureProviderHandle {
            panic!("startup fake decoder does not provide renderer texture views")
        }

        fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Проверяет, что StartedVideoBackend отдаёт только neutral decoder handle.
    #[test]
    fn started_video_backend_returns_neutral_decoder_handle() {
        let started_backend = StartedVideoBackend::from_decoder_thread(StartupFakeDecoderThread);
        let decoder_thread = started_backend.into_decoder_thread();

        assert_eq!(decoder_thread.backend_name(), "startup fake decoder");
    }
}
