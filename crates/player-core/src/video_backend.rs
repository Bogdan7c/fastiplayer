use crate::{PlayerVideoDecoderThreadHandle, WgpuRenderTextureProviderHandle};
use video_core::VideoDecoderThreadHandle;

/// Фабрика video backend-а, которую session вызывает без знания деталей конкретного backend init.
pub trait VideoBackendFactory: Send {
    /// Стартует backend и возвращает уже готовый decoder thread wrapper.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend>;
}

/// Запущенный video backend, подготовленный фабрикой для установки в playback pipeline.
pub struct StartedVideoBackend {
    /// Decoder thread остаётся за neutral handle boundary.
    decoder_thread: Box<PlayerVideoDecoderThreadHandle>,
}

impl StartedVideoBackend {
    /// Создаёт backend wrapper вокруг decoder thread, который уже прошёл init handshake.
    pub fn from_decoder_thread(
        decoder_thread: impl VideoDecoderThreadHandle<
            TextureViewProvider = WgpuRenderTextureProviderHandle,
        > + 'static,
    ) -> Self {
        Self {
            decoder_thread: Box::new(decoder_thread),
        }
    }

    /// Передаёт decoder handle pipeline-у без раскрытия concrete backend type.
    pub(crate) fn into_decoder_thread(self) -> Box<PlayerVideoDecoderThreadHandle> {
        self.decoder_thread
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DecodeSendError, DecodeThreadError, DecoderResourceSnapshot, PlayerDecodePacket,
        WgpuRenderTextureProviderHandle,
    };

    /// Minimal fake decoder для проверки startup wrapper-а без production backend resources.
    struct StartupFakeDecoderThread;

    impl VideoDecoderThreadHandle for StartupFakeDecoderThread {
        type TextureViewProvider = WgpuRenderTextureProviderHandle;

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

        fn texture_view_provider(&self) -> WgpuRenderTextureProviderHandle {
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
