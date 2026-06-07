use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup, StartedVideoBackend,
    VideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    VideoDecoderControlChannelPressureSnapshot, VideoDecoderDiagnosticEvent,
    VideoDecoderThreadHandle,
};

/// Оборачивает started backend так, чтобы submitted releases ждали WGPU queue completion.
///
/// VAAPI provider остаётся владельцем decoder resource lifecycle, но WGPU queue
/// принадлежит renderer layer-у. Поэтому player-core получает provider wrapper,
/// который делегирует lookup/descriptor lookup, а release отправляет во внутренний
/// provider только после `on_submitted_work_done`.
#[must_use]
pub fn wrap_video_backend_for_wgpu_submission(
    started_backend: StartedVideoBackend,
    queue: &wgpu::Queue,
) -> (StartedVideoBackend, PresentFrameResourceProviderHandle) {
    let decoder_thread = started_backend.into_decoder_thread();
    let inner_provider = decoder_thread.resource_provider();
    let renderer_provider =
        PresentFrameResourceProviderHandle::new(WgpuSubmittedResourceProvider {
            inner_provider,
            queue: queue.clone(),
        });
    let wrapped_thread = WgpuReleaseVideoDecoderThreadHandle {
        inner: decoder_thread,
        renderer_provider: renderer_provider.clone(),
    };

    (
        StartedVideoBackend::from_decoder_thread(wrapped_thread),
        renderer_provider,
    )
}

/// Provider wrapper, который добавляет WGPU completion wait к submitted release path.
struct WgpuSubmittedResourceProvider {
    /// Concrete backend provider с нейтральными descriptors и VA release path.
    inner_provider: PresentFrameResourceProviderHandle,

    /// Renderer-owned queue, на которой уже отправлен video render work.
    queue: wgpu::Queue,
}

impl PresentFrameResourceProvider for WgpuSubmittedResourceProvider {
    fn resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.inner_provider.resource_lookup(handle)
    }

    fn try_resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.inner_provider.try_resource_lookup(handle)
    }

    fn resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner_provider.resource_descriptor_lookup(handle)
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner_provider.try_resource_descriptor_lookup(handle)
    }

    fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        let inner_provider = self.inner_provider.clone();
        self.queue.on_submitted_work_done(move || {
            inner_provider.release_frame(handle);
        });
    }
}

/// Decoder-thread wrapper, который заменяет provider на renderer-aware wrapper.
struct WgpuReleaseVideoDecoderThreadHandle {
    /// Concrete decoder thread остаётся владельцем decode/flush/unsubmitted release.
    inner: Box<VideoBackendDecoderThreadHandle>,

    /// Provider, который player-core сохранит в render leases.
    renderer_provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for WgpuReleaseVideoDecoderThreadHandle {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn send_packet(&self, packet: video_core::DecodePacket) -> Result<(), DecodeSendError> {
        self.inner.send_packet(packet)
    }

    fn configure_stream(
        &self,
        config: video_core::VideoStreamDecodeConfig,
    ) -> video_core::VideoStreamConfigResult {
        self.inner.configure_stream(config)
    }

    fn clear_stream(&self) -> video_core::VideoStreamConfigResult {
        self.inner.clear_stream()
    }

    fn set_preroll_output_floor(
        &self,
        floor: video_core::VideoPrerollOutputFloor,
    ) -> video_core::VideoPrerollOutputFloorResult {
        self.inner.set_preroll_output_floor(floor)
    }

    fn clear_preroll_output_floor(
        &self,
        clear: video_core::VideoPrerollOutputFloorClear,
    ) -> video_core::VideoPrerollOutputFloorResult {
        self.inner.clear_preroll_output_floor(clear)
    }

    fn begin_end_of_stream_drain(
        &self,
        generation: u64,
    ) -> video_core::VideoDecoderEndOfStreamDrainResult {
        self.inner.begin_end_of_stream_drain(generation)
    }

    fn end_of_stream_drain_state(&self) -> video_core::VideoDecoderEndOfStreamDrainState {
        self.inner.end_of_stream_drain_state()
    }

    fn release_frame(&self, handle: video_core::FrameResourceHandle) {
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

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.renderer_provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.inner.decoder_resource_snapshot()
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        self.inner.decoder_control_channel_pressure()
    }

    fn packet_queue_depth(&self) -> usize {
        self.inner.packet_queue_depth()
    }

    fn drain_completed_packet_count(&self) -> usize {
        self.inner.drain_completed_packet_count()
    }
}
