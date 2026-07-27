use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
    SharedVideoBackendDecoderThreadHandle, StartedVideoBackend, VideoBackendLifetimeGuard,
    share_video_backend_decoder_thread,
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
) -> (
    StartedVideoBackend,
    PresentFrameResourceProviderHandle,
    WgpuSubmissionQueueBinding,
) {
    let backend_id = started_backend.backend_id().to_owned();
    let decoder_thread = started_backend.into_decoder_thread();
    let inner_provider = decoder_thread.resource_provider();
    let (decoder_thread, backend_lifetime) = share_video_backend_decoder_thread(decoder_thread);
    let submission_queue = WgpuSubmissionQueueBinding::new(queue);
    let renderer_provider =
        PresentFrameResourceProviderHandle::new(WgpuSubmittedResourceProvider {
            inner_provider,
            submission_queue: submission_queue.clone(),
            backend_lifetime,
        });
    let wrapped_thread = WgpuReleaseVideoDecoderThreadHandle {
        inner: decoder_thread,
        renderer_provider: renderer_provider.clone(),
    };

    let wrapped_backend = StartedVideoBackend::from_decoder_thread(backend_id, wrapped_thread);

    (wrapped_backend, renderer_provider, submission_queue)
}

/// Переключаемая queue-привязка provider-а к активному renderer lifecycle.
///
/// Один handle разделяется decoder wrapper-ом и app-owned materializer-ом. Queue можно
/// заменить только после остановки новых submissions, освобождения frame leases и
/// ожидания завершения старой GPU queue.
#[derive(Clone)]
pub struct WgpuSubmissionQueueBinding {
    /// Queue state и exactly-once submitted releases разделяются всеми provider handles.
    inner: Arc<WgpuSubmissionQueueBindingInner>,
}

/// Shared mutable state queue binding-а.
struct WgpuSubmissionQueueBindingInner {
    /// Active/lost queue и pending releases меняются одной критической секцией.
    state: Mutex<WgpuSubmissionQueueState>,

    /// Монотонный id не влияет на frame handle/generation semantics.
    next_release_id: AtomicU64,
}

/// State queue вместе с callbacks, которые ещё не подтвердили completion.
struct WgpuSubmissionQueueState {
    /// None означает доказанный device lost: future releases безопасно идут сразу.
    active_queue: Option<wgpu::Queue>,

    /// Exactly-once guards submitted releases старого или нового renderer-а.
    pending_releases: BTreeMap<u64, Arc<PendingSubmittedRelease>>,
}

/// Один decoder resource, который должен быть возвращён provider-у ровно один раз.
struct PendingSubmittedRelease {
    /// Backend provider остаётся владельцем фактического resource release-а.
    inner_provider: PresentFrameResourceProviderHandle,

    /// Opaque resource handle release boundary-а.
    handle: video_core::FrameResourceHandle,

    /// Renderer-neutral backend owner живёт до фактического resource release-а.
    _backend_lifetime: VideoBackendLifetimeGuard,

    /// Callback и device-lost recovery соревнуются через exactly-once CAS.
    released: AtomicBool,
}

impl PendingSubmittedRelease {
    /// Возвращает resource provider-у только победителем exactly-once CAS.
    fn release_once(&self) {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.inner_provider.release_frame(self.handle);
        }
    }
}

impl WgpuSubmissionQueueBinding {
    /// Создаёт привязку к queue первоначального renderer-а.
    fn new(queue: &wgpu::Queue) -> Self {
        Self {
            inner: Arc::new(WgpuSubmissionQueueBindingInner {
                state: Mutex::new(WgpuSubmissionQueueState {
                    active_queue: Some(queue.clone()),
                    pending_releases: BTreeMap::new(),
                }),
                next_release_id: AtomicU64::new(0),
            }),
        }
    }

    /// Атомарно переводит будущие release callbacks на queue нового renderer-а.
    ///
    /// Poison возвращается как typed commit failure: продолжать частичную замену
    /// renderer-а после потери доверия к binding state нельзя.
    pub fn rebind(&self, queue: &wgpu::Queue) -> Result<(), WgpuSubmissionQueueRebindError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| WgpuSubmissionQueueRebindError::Poisoned)?;
        state.active_queue = Some(queue.clone());
        Ok(())
    }

    /// Завершает pending releases после доказанного device lost ровно по одному разу.
    pub fn release_after_device_lost(&self) -> Result<usize, WgpuSubmissionQueueRebindError> {
        let pending_releases = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| WgpuSubmissionQueueRebindError::Poisoned)?;
            state.active_queue = None;
            std::mem::take(&mut state.pending_releases)
        };
        let release_count = pending_releases.len();
        for pending_release in pending_releases.into_values() {
            pending_release.release_once();
        }
        Ok(release_count)
    }

    /// Регистрирует release callback либо release-ит сразу после proven device lost.
    fn schedule_release(
        &self,
        inner_provider: PresentFrameResourceProviderHandle,
        handle: video_core::FrameResourceHandle,
        backend_lifetime: VideoBackendLifetimeGuard,
    ) {
        let release_id = self.inner.next_release_id.fetch_add(1, Ordering::Relaxed);
        let pending_release = Arc::new(PendingSubmittedRelease {
            inner_provider,
            handle,
            _backend_lifetime: backend_lifetime,
            released: AtomicBool::new(false),
        });

        let active_queue = match self.inner.state.lock() {
            Ok(mut state) => match &state.active_queue {
                Some(active_queue) => {
                    let active_queue = active_queue.clone();
                    state
                        .pending_releases
                        .insert(release_id, pending_release.clone());
                    Some(active_queue)
                }
                None => None,
            },
            Err(poisoned_state) => {
                tracing::error!(
                    "WGPU submission queue binding poisoned; используем последнее доступное состояние для safe release"
                );
                let mut state = poisoned_state.into_inner();
                match &state.active_queue {
                    Some(active_queue) => {
                        let active_queue = active_queue.clone();
                        state
                            .pending_releases
                            .insert(release_id, pending_release.clone());
                        Some(active_queue)
                    }
                    None => None,
                }
            }
        };

        let Some(active_queue) = active_queue else {
            pending_release.release_once();
            return;
        };

        let binding = self.clone();
        active_queue.on_submitted_work_done(move || {
            pending_release.release_once();
            binding.complete_release(release_id);
        });
    }

    /// Удаляет завершённый callback из bounded pending registry.
    fn complete_release(&self, release_id: u64) {
        match self.inner.state.lock() {
            Ok(mut state) => {
                state.pending_releases.remove(&release_id);
            }
            Err(poisoned_state) => {
                tracing::error!(
                    release_id,
                    "WGPU submission release registry poisoned при callback cleanup"
                );
                poisoned_state
                    .into_inner()
                    .pending_releases
                    .remove(&release_id);
            }
        }
    }
}

/// Ошибка commit-а переключаемой WGPU queue-привязки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgpuSubmissionQueueRebindError {
    /// Mutex был poisoned паникой внутри критической секции binding-а.
    Poisoned,
}

impl std::fmt::Display for WgpuSubmissionQueueRebindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Poisoned => formatter.write_str("WGPU submission queue binding poisoned"),
        }
    }
}

impl std::error::Error for WgpuSubmissionQueueRebindError {}

/// Provider wrapper, который добавляет WGPU completion wait к submitted release path.
struct WgpuSubmittedResourceProvider {
    /// Concrete backend provider с нейтральными descriptors и VA release path.
    inner_provider: PresentFrameResourceProviderHandle,

    /// Renderer-owned queue binding, переключаемый только controlled recreation-ом.
    submission_queue: WgpuSubmissionQueueBinding,

    /// Neutral backend owner для callbacks, переживающих playback decoder handle.
    backend_lifetime: VideoBackendLifetimeGuard,
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
        // Контракт submitted release: player-core уже передал frame в render submission,
        // поэтому decoder resource можно вернуть inner provider-у только после того,
        // как WGPU подтвердит завершение ранее отправленной работы очереди.
        self.submission_queue.schedule_release(
            self.inner_provider.clone(),
            handle,
            self.backend_lifetime.clone(),
        );
    }
}

/// Decoder-thread wrapper, который заменяет provider на renderer-aware wrapper.
struct WgpuReleaseVideoDecoderThreadHandle {
    /// Concrete decoder thread остаётся владельцем decode/flush/unsubmitted release.
    inner: SharedVideoBackendDecoderThreadHandle,

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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Provider fake считает фактические releases без GPU queue.
    struct CountingResourceProvider {
        release_count: Arc<AtomicUsize>,
    }

    impl PresentFrameResourceProvider for CountingResourceProvider {
        fn resource_lookup(
            &self,
            _handle: video_core::FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: std::time::Duration::ZERO,
            }
        }

        fn release_frame(&self, _handle: video_core::FrameResourceHandle) {
            self.release_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct LifetimeFakeDecoderThread {
        provider: PresentFrameResourceProviderHandle,
        drop_count: Arc<AtomicUsize>,
    }

    impl Drop for LifetimeFakeDecoderThread {
        fn drop(&mut self) {
            self.drop_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl VideoDecoderThreadHandle for LifetimeFakeDecoderThread {
        type ResourceProvider = PresentFrameResourceProviderHandle;

        fn backend_name(&self) -> &'static str {
            "lifetime fake decoder"
        }

        fn send_packet(&self, _packet: video_core::DecodePacket) -> Result<(), DecodeSendError> {
            Err(DecodeSendError::Fatal(DecodeThreadError::new(
                "lifetime fake does not decode",
            )))
        }

        fn release_frame(&self, handle: video_core::FrameResourceHandle) {
            self.provider.release_frame(handle);
        }

        fn try_recv_frame(&self) -> Option<DecodedFrame> {
            None
        }

        fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn resource_provider(&self) -> Self::ResourceProvider {
            self.provider.clone()
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

    #[test]
    fn submitted_release_guard_prevents_callback_and_device_lost_double_release() {
        let release_count = Arc::new(AtomicUsize::new(0));
        let provider = PresentFrameResourceProviderHandle::new(CountingResourceProvider {
            release_count: release_count.clone(),
        });
        let decoder_drop_count = Arc::new(AtomicUsize::new(0));
        let decoder_thread: Box<video_backend_api::VideoBackendDecoderThreadHandle> =
            Box::new(LifetimeFakeDecoderThread {
                provider: provider.clone(),
                drop_count: decoder_drop_count.clone(),
            });
        let (shared_decoder, backend_lifetime) = share_video_backend_decoder_thread(decoder_thread);
        drop(shared_decoder);
        let pending_release = PendingSubmittedRelease {
            inner_provider: provider,
            handle: video_core::FrameResourceHandle(41),
            _backend_lifetime: backend_lifetime,
            released: AtomicBool::new(false),
        };

        pending_release.release_once();
        pending_release.release_once();

        assert_eq!(release_count.load(Ordering::Relaxed), 1);
        assert_eq!(decoder_drop_count.load(Ordering::Relaxed), 0);
        drop(pending_release);
        assert_eq!(decoder_drop_count.load(Ordering::Relaxed), 1);
    }
}
