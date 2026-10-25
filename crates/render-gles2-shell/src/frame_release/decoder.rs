//! Обёртка декодера: подменяет только провайдер для render leases, остальное — как есть.

use video_backend_api::{
    PresentFrameResourceProviderHandle, SharedVideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceHandle, HostUploadResourceSnapshotStatus, VideoDecoderActivitySnapshot,
    VideoDecoderControlChannelPressureSnapshot, VideoDecoderDiagnosticEvent,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoDecoderThreadHandle, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult, VideoStreamConfigResult, VideoStreamDecodeConfig,
};

/// Декодер с провайдером, ждущим fence. Каждый метод трейта обязан быть здесь явно: у девяти
/// методов трейта есть дефолты, и забытый проброс молча подменяет ответ декодера заглушкой
/// (так уже ломались `decoder_activity_snapshot` и `host_upload_resource_snapshot`). Это
/// сторожит тест `forwarding_covers_every_trait_method`.
pub(super) struct Gles2ReleaseVideoDecoderThreadHandle {
    /// Настоящий декодер: владелец decode/flush и возврата кадра, не отданного рендеру.
    inner: SharedVideoBackendDecoderThreadHandle,
    /// Провайдер, который плеер сохранит в render leases.
    renderer_provider: PresentFrameResourceProviderHandle,
}

impl Gles2ReleaseVideoDecoderThreadHandle {
    pub(super) fn new(
        inner: SharedVideoBackendDecoderThreadHandle,
        renderer_provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        Self {
            inner,
            renderer_provider,
        }
    }
}

impl VideoDecoderThreadHandle for Gles2ReleaseVideoDecoderThreadHandle {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError> {
        self.inner.send_packet(packet)
    }

    fn configure_stream(&self, config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        self.inner.configure_stream(config)
    }

    fn clear_stream(&self) -> VideoStreamConfigResult {
        self.inner.clear_stream()
    }

    fn set_preroll_output_floor(
        &self,
        floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        self.inner.set_preroll_output_floor(floor)
    }

    fn clear_preroll_output_floor(
        &self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        self.inner.clear_preroll_output_floor(clear)
    }

    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        self.inner.begin_end_of_stream_drain(generation)
    }

    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        self.inner.end_of_stream_drain_state()
    }

    /// Кадр, не отданный рендеру (отброшен до показа): GPU его не трогал, fence не нужен.
    fn release_frame(&self, handle: FrameResourceHandle) {
        self.inner.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.inner.try_recv_frame()
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        self.inner.try_recv_diagnostic_event()
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.inner.try_recv_error()
    }

    fn flush(&self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    /// Единственный метод, который обёртка НЕ форвардит: плеер получает провайдер с fence.
    fn resource_provider(&self) -> Self::ResourceProvider {
        self.renderer_provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.inner.decoder_resource_snapshot()
    }

    fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        self.inner.host_upload_resource_snapshot()
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        self.inner.decoder_control_channel_pressure()
    }

    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        self.inner.decoder_activity_snapshot()
    }

    fn packet_queue_depth(&self) -> usize {
        self.inner.packet_queue_depth()
    }

    fn drain_completed_packet_count(&self) -> usize {
        self.inner.drain_completed_packet_count()
    }
}
