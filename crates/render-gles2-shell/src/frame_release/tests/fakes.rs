//! Подставные fence, провайдер и декодер для тестов освобождения кадров.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gles2_egui::{
    ContextLoss, ContextLossDetection, ContextLossReason, FenceStatus, FenceWaitError,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup, StartedVideoBackend,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceHandle, HostUploadResourceSnapshot, HostUploadResourceSnapshotStatus,
    VideoDecoderActivityNotifier, VideoDecoderActivitySnapshot,
    VideoDecoderControlChannelPressureSnapshot, VideoDecoderDiagnosticEvent,
    VideoDecoderDropReason, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoDecoderThreadHandle, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult, VideoStreamConfigRejection, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};
use video_frame_contract::VideoFramePixelLayout;

use super::super::binding::{Gles2FrameReleaseBinding, wrap_video_backend_for_gles2_release};
use super::super::fence::PresentedFrameFence;

/// Предел ожидания события в тестах: достаточно большой для медленной CI, но не бесконечный.
pub(crate) const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Окно «ничего не должно произойти»: короче 100 мс, как требует план (поток-ожидатель
/// дополнительно синхронизируется через `wait_until_waited`, так что окно не единственное
/// доказательство).
pub(crate) const QUIET_WINDOW: Duration = Duration::from_millis(80);

/// Что сейчас отвечает fake-fence.
#[derive(Clone, Copy)]
enum FakeFenceOutcome {
    Pending,
    Signaled,
    Failed(FenceWaitError),
}

struct FakeFenceState {
    outcome: FakeFenceOutcome,
    /// Сколько раз кто-то зашёл в `wait` (поток-ожидатель реально ждёт).
    wait_calls: usize,
}

/// Fence, которым управляет тест: `signal()` / `fail()`.
pub(crate) struct FakeFence {
    state: Mutex<FakeFenceState>,
    changed: Condvar,
    /// В каком потоке fence был разрушен (имитация `eglDestroySync`, который обязан случиться в
    /// потоке рендера). Журнал общий, поэтому читается и после уничтожения fence.
    destroyed_in: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}

impl Drop for FakeFence {
    fn drop(&mut self) {
        if let Ok(mut log) = self.destroyed_in.lock() {
            log.push(std::thread::current().id());
        }
    }
}

impl FakeFence {
    pub(crate) fn pending() -> Arc<Self> {
        Self::with_outcome(FakeFenceOutcome::Pending)
    }

    pub(crate) fn already_signaled() -> Arc<Self> {
        Self::with_outcome(FakeFenceOutcome::Signaled)
    }

    fn with_outcome(outcome: FakeFenceOutcome) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeFenceState {
                outcome,
                wait_calls: 0,
            }),
            changed: Condvar::new(),
            destroyed_in: Arc::default(),
        })
    }

    /// Журнал потоков, в которых fence был разрушен (пуст, пока fence жив).
    pub(crate) fn destruction_log(&self) -> Arc<Mutex<Vec<std::thread::ThreadId>>> {
        Arc::clone(&self.destroyed_in)
    }

    /// GPU «прошёл» fence.
    pub(crate) fn signal(&self) {
        self.set_outcome(FakeFenceOutcome::Signaled);
    }

    /// Ожидание и проверка начинают отказывать потерей контекста.
    pub(crate) fn fail_with_context_loss(&self) {
        self.set_outcome(FakeFenceOutcome::Failed(FenceWaitError::ContextLost(
            ContextLoss {
                reason: ContextLossReason::EglContextLost,
                detection: ContextLossDetection::EglCall,
            },
        )));
    }

    fn set_outcome(&self, outcome: FakeFenceOutcome) {
        self.state.lock().expect("fake fence").outcome = outcome;
        self.changed.notify_all();
    }

    /// Ждёт, пока в `wait` зайдёт хотя бы один поток (поток-ожидатель реально ждёт fence).
    pub(crate) fn wait_until_waited(&self, timeout: Duration) -> bool {
        let state = self.state.lock().expect("fake fence");
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.wait_calls == 0)
            .expect("fake fence");
        state.wait_calls > 0
    }

    fn status(outcome: FakeFenceOutcome) -> Result<FenceStatus, FenceWaitError> {
        match outcome {
            FakeFenceOutcome::Pending => Ok(FenceStatus::Pending),
            FakeFenceOutcome::Signaled => Ok(FenceStatus::Signaled),
            FakeFenceOutcome::Failed(error) => Err(error),
        }
    }
}

impl PresentedFrameFence for FakeFence {
    fn poll(&self) -> Result<FenceStatus, FenceWaitError> {
        Self::status(self.state.lock().expect("fake fence").outcome)
    }

    fn wait(&self, timeout: Duration) -> Result<FenceStatus, FenceWaitError> {
        let mut state = self.state.lock().expect("fake fence");
        state.wait_calls += 1;
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state.outcome, FakeFenceOutcome::Pending)
            })
            .expect("fake fence");
        Self::status(state.outcome)
    }
}

/// Журнал внутреннего провайдера: какие кадры вернули и какие методы вызывали.
#[derive(Default)]
pub(crate) struct InnerProviderLog {
    released: Mutex<Vec<u64>>,
    released_changed: Condvar,
    /// Методы провайдера (`PresentFrameResourceProvider`), дошедшие до внутреннего провайдера.
    calls: Mutex<Vec<&'static str>>,
    /// Методы декодера (`VideoDecoderThreadHandle`), дошедшие до внутреннего декодера.
    decoder_calls: Mutex<Vec<&'static str>>,
}

impl InnerProviderLog {
    /// Номера вернувшихся кадров в порядке возврата (повтор — двойное освобождение).
    pub(crate) fn released_handles(&self) -> Vec<u64> {
        self.released.lock().expect("journal").clone()
    }

    /// Ждёт, пока вернётся не меньше `count` кадров.
    pub(crate) fn wait_for_release_count(&self, count: usize, timeout: Duration) -> bool {
        let released = self.released.lock().expect("journal");
        let (released, _) = self
            .released_changed
            .wait_timeout_while(released, timeout, |released| released.len() < count)
            .expect("journal");
        released.len() >= count
    }

    /// Имена вызванных методов внутреннего провайдера.
    pub(crate) fn called_provider_methods(&self) -> Vec<&'static str> {
        self.calls.lock().expect("journal").clone()
    }

    /// Имена вызванных методов внутреннего декодера.
    pub(crate) fn called_decoder_methods(&self) -> Vec<&'static str> {
        self.decoder_calls.lock().expect("journal").clone()
    }

    fn record_decoder_call(&self, method: &'static str) {
        self.decoder_calls.lock().expect("journal").push(method);
    }

    fn record_call(&self, method: &'static str) {
        self.calls.lock().expect("journal").push(method);
    }

    fn record_release(&self, handle: FrameResourceHandle) {
        self.record_call("release_frame");
        self.released.lock().expect("journal").push(handle.0);
        self.released_changed.notify_all();
    }
}

/// Внутренний провайдер: каждый lookup отвечает отличимым НЕ-дефолтным значением.
pub(crate) struct FakeInnerProvider {
    pub(crate) log: Arc<InnerProviderLog>,
}

pub(crate) fn lock_wait(micros: u64) -> Duration {
    Duration::from_micros(micros)
}

impl PresentFrameResourceProvider for FakeInnerProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        self.log.record_call("resource_lookup");
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: lock_wait(1),
        }
    }

    fn try_resource_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.log.record_call("try_resource_lookup");
        PresentFrameResourceProviderLookup::Busy {
            resource_pool_lock_wait: lock_wait(2),
        }
    }

    fn resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.log.record_call("resource_descriptor_lookup");
        PresentFrameResourceDescriptorLookup::Missing {
            resource_pool_lock_wait: lock_wait(3),
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.log.record_call("try_resource_descriptor_lookup");
        PresentFrameResourceDescriptorLookup::Fatal {
            resource_pool_lock_wait: lock_wait(4),
        }
    }

    fn try_resource_identity_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceIdentityLookup {
        self.log.record_call("try_resource_identity_lookup");
        PresentFrameResourceIdentityLookup::Missing {
            resource_pool_lock_wait: lock_wait(5),
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.log.record_release(handle);
    }
}

/// Fake-декодер: каждый метод отвечает отличимым не-дефолтным значением и пишет своё имя.
pub(crate) struct FakeDecoder {
    provider: PresentFrameResourceProviderHandle,
    log: Arc<InnerProviderLog>,
    activity: VideoDecoderActivitySnapshot,
    /// Сообщает тесту, что декодер уничтожен (последний владелец отпущен).
    dropped: Sender<()>,
}

impl Drop for FakeDecoder {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

/// Значение, которое fake-декодер отдаёт из `decoder_resource_snapshot`.
pub(crate) fn fake_resource_snapshot() -> DecoderResourceSnapshot {
    DecoderResourceSnapshot {
        capacity: 11,
        slots: 7,
        in_use: 3,
        free_surfaces: 4,
        waiting_gpu_completion: 2,
        waiting_decoder_reuse: 1,
        import_failures: 5,
        imports_created: 6,
        imports_reused: 8,
        imports_replaced: 9,
    }
}

/// Значение `host_upload_resource_snapshot` fake-декодера.
pub(crate) fn fake_host_upload_snapshot() -> HostUploadResourceSnapshot {
    HostUploadResourceSnapshot {
        host_frames_ready: 6,
        host_frames_in_flight: 5,
        upload_slots_capacity: 8,
        upload_slots_free: 1,
        upload_failures: 2,
    }
}

/// Значение `decoder_control_channel_pressure` fake-декодера.
pub(crate) fn fake_control_pressure() -> VideoDecoderControlChannelPressureSnapshot {
    VideoDecoderControlChannelPressureSnapshot {
        control_channel_len: 3,
        control_channel_capacity: 16,
        control_channel_full_count: 4,
        release_control_send_fail_count: 5,
        flush_control_send_fail_count: 6,
    }
}

impl VideoDecoderThreadHandle for FakeDecoder {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        self.log.record_decoder_call("backend_name");
        "fake-gles2-decoder"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        self.log.record_decoder_call("send_packet");
        Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "fake send_packet",
        )))
    }

    fn configure_stream(&self, _config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        self.log.record_decoder_call("configure_stream");
        VideoStreamConfigResult::Unsupported(VideoStreamConfigRejection::BackendUnsupported {
            reason: "fake configure_stream".to_owned(),
        })
    }

    fn clear_stream(&self) -> VideoStreamConfigResult {
        self.log.record_decoder_call("clear_stream");
        VideoStreamConfigResult::Cleared
    }

    fn set_preroll_output_floor(
        &self,
        _floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        self.log.record_decoder_call("set_preroll_output_floor");
        VideoPrerollOutputFloorResult::Applied
    }

    fn clear_preroll_output_floor(
        &self,
        _clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        self.log.record_decoder_call("clear_preroll_output_floor");
        VideoPrerollOutputFloorResult::Cleared
    }

    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        self.log.record_decoder_call("begin_end_of_stream_drain");
        VideoDecoderEndOfStreamDrainResult::Unchanged(VideoDecoderEndOfStreamDrainState::Draining {
            generation,
        })
    }

    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        self.log.record_decoder_call("end_of_stream_drain_state");
        VideoDecoderEndOfStreamDrainState::Draining { generation: 77 }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.log.record_decoder_call("release_frame");
        self.provider.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.log.record_decoder_call("try_recv_frame");
        let mut frame = crate::test_support::host_frame(
            VideoFramePixelLayout::Yuv420Planar8,
            [2, 2],
            0,
            [1, 2, 3],
        )
        .frame;
        frame.pts = Duration::from_millis(123);
        Some(frame)
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        self.log.record_decoder_call("try_recv_diagnostic_event");
        Some(VideoDecoderDiagnosticEvent::FrameDropped {
            pts: Duration::from_millis(5),
            reason: VideoDecoderDropReason::ReadyQueueOverflow,
        })
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.log.record_decoder_call("try_recv_error");
        Some(DecodeThreadError::new("fake try_recv_error"))
    }

    fn flush(&self) -> anyhow::Result<()> {
        self.log.record_decoder_call("flush");
        Err(anyhow::anyhow!("fake flush"))
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.log.record_decoder_call("resource_provider");
        self.provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.log.record_decoder_call("decoder_resource_snapshot");
        Some(fake_resource_snapshot())
    }

    fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        self.log
            .record_decoder_call("host_upload_resource_snapshot");
        HostUploadResourceSnapshotStatus::Available(fake_host_upload_snapshot())
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        self.log
            .record_decoder_call("decoder_control_channel_pressure");
        Some(fake_control_pressure())
    }

    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        self.log.record_decoder_call("decoder_activity_snapshot");
        self.activity.clone()
    }

    fn packet_queue_depth(&self) -> usize {
        self.log.record_decoder_call("packet_queue_depth");
        17
    }

    fn drain_completed_packet_count(&self) -> usize {
        self.log.record_decoder_call("drain_completed_packet_count");
        23
    }
}

/// Собранный стенд: привязка, обёрнутый backend и всё, что нужно для наблюдения.
pub(crate) struct Harness {
    pub(crate) binding: Gles2FrameReleaseBinding,
    /// Что видит плеер: провайдер для render leases.
    pub(crate) renderer_provider: PresentFrameResourceProviderHandle,
    /// Обёрнутый backend (`None` после `drop_backend`).
    pub(crate) backend: Option<StartedVideoBackend>,
    pub(crate) inner_log: Arc<InnerProviderLog>,
    /// Приходит сообщение, когда fake-декодер уничтожен.
    pub(crate) decoder_dropped: Receiver<()>,
    /// Уведомитель активности: держим, чтобы подписка fake-декодера оставалась живой.
    pub(crate) activity_notifier: VideoDecoderActivityNotifier,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let inner_log = Arc::new(InnerProviderLog::default());
        let inner_provider = PresentFrameResourceProviderHandle::new(FakeInnerProvider {
            log: Arc::clone(&inner_log),
        });
        let (activity_notifier, subscription) = VideoDecoderActivityNotifier::new();
        let (dropped, decoder_dropped) = channel();
        let decoder = FakeDecoder {
            provider: inner_provider,
            log: Arc::clone(&inner_log),
            activity: subscription.snapshot(),
            dropped,
        };
        let binding = Gles2FrameReleaseBinding::new();
        let (backend, renderer_provider) = wrap_video_backend_for_gles2_release(
            StartedVideoBackend::from_decoder_thread("fake-gles2", decoder),
            &binding,
        );
        Self {
            binding,
            renderer_provider,
            backend: Some(backend),
            inner_log,
            decoder_dropped,
            activity_notifier,
        }
    }

    /// Публикует fake-fence как fence последнего кадра показа.
    pub(crate) fn publish(&self, fence: &Arc<FakeFence>) {
        let fence: Arc<dyn PresentedFrameFence> = fence.clone();
        self.binding.publish_fence(fence);
    }

    /// Плеер просит вернуть кадр `raw_handle` через провайдер render leases.
    pub(crate) fn request_release(&self, raw_handle: u64) {
        self.renderer_provider
            .release_frame(FrameResourceHandle(raw_handle));
    }

    /// Ждёт уничтожения fake-декодера.
    pub(crate) fn wait_decoder_dropped(&self) -> bool {
        self.decoder_dropped.recv_timeout(EVENT_TIMEOUT).is_ok()
    }
}
