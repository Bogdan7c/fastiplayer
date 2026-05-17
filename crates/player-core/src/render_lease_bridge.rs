use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvError, SendTimeoutError, Sender, TrySendError, bounded};
use tracing::warn;

use crate::session::{LeasedPresentFrame, PlayerSession};

/// Ёмкость release ack stream; защищает worker от бесконечного роста drop-ack очереди.
const RENDER_RELEASE_CHANNEL_CAPACITY: usize = 512;

/// Ёмкость неблокирующих render-acquire latency samples.
const RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Ёмкость неблокирующих GPU submit/present latency samples.
const RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Короткий backpressure budget для Drop path render lease-а.
const RENDER_RELEASE_SEND_TIMEOUT: Duration = Duration::from_millis(2);

/// WGPU texture views, полученные render thread-ом по opaque frame handle.
pub struct PresentFrameTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub uv_view: wgpu::TextureView,
}

/// Lease текущего кадра, который render thread может отдать `render-wgpu`.
#[derive(Clone)]
pub struct PresentFrameLease {
    /// Поколение render resources, которому принадлежит texture handle.
    pub render_generation: u64,

    /// Metadata decoded frame без прямого доступа к `PlayerSession`.
    pub frame: video_core::DecodedFrame,

    /// Признак, что кадр является stale fallback для текущего timeline состояния.
    pub stale: bool,

    /// Render-side provider для ленивого получения WGPU views по frame handle.
    texture_provider: Option<video_vaapi::VideoTextureViewProvider>,

    /// Shared lease отправляет drop-ack, когда последний clone кадра освобождён.
    _drop_ack: Arc<PresentFrameDropAck>,
}

/// Backward-compatible имя public frame lease для текущего app shell.
pub type PlayerPresentFrame = PresentFrameLease;

impl PresentFrameLease {
    /// Собирает render lease из worker-owned session lease без раскрытия pipeline наружу.
    #[must_use]
    fn from_leased_frame(
        leased_frame: LeasedPresentFrame,
        release_tx: Sender<RenderLeaseRelease>,
    ) -> Self {
        Self {
            render_generation: leased_frame.render_generation,
            frame: leased_frame.frame.clone(),
            stale: leased_frame.stale,
            texture_provider: Some(leased_frame.texture_provider.clone()),
            _drop_ack: Arc::new(PresentFrameDropAck {
                render_generation: leased_frame.render_generation,
                texture_handle: leased_frame.frame.texture_handle,
                texture_provider: Some(leased_frame.texture_provider),
                release_tx,
            }),
        }
    }

    /// Создаёт тестовый lease с тем же RAII drop-ack контрактом, что production path.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new_for_tests(
        render_generation: u64,
        frame: video_core::DecodedFrame,
        stale: bool,
        release_tx: Sender<RenderLeaseRelease>,
    ) -> Self {
        let texture_handle = frame.texture_handle;

        Self {
            render_generation,
            frame,
            stale,
            texture_provider: None,
            _drop_ack: Arc::new(PresentFrameDropAck {
                render_generation,
                texture_handle,
                texture_provider: None,
                release_tx,
            }),
        }
    }

    /// Возвращает opaque texture handle кадра без доступа к player pipeline.
    #[must_use]
    pub const fn texture_handle(&self) -> video_core::FrameTextureHandle {
        self.frame.texture_handle
    }

    /// Возвращает `true`, если lease устарел относительно актуального render generation.
    #[must_use]
    pub const fn stale_for_generation(&self, current_render_generation: u64) -> bool {
        self.stale || self.render_generation != current_render_generation
    }

    /// Получает WGPU views на render thread через render-side provider.
    #[must_use]
    pub fn texture_views(&self) -> Option<PresentFrameTextureViews> {
        self.texture_provider
            .as_ref()
            .and_then(|provider| provider.texture_views(self.texture_handle()))
            .map(|views| PresentFrameTextureViews {
                y_view: views.y_view,
                uv_view: views.uv_view,
            })
    }
}

/// Стабильная identity кадра для latest-slot без сравнения backend handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentFrameLeaseIdentity {
    /// Поколение render resources, где был создан texture handle.
    render_generation: u64,

    /// Opaque texture handle decoded frame-а.
    texture_handle: video_core::FrameTextureHandle,
}

impl PresentFrameLeaseIdentity {
    /// Собирает identity из lease-а, который уже безопасно опубликован render-side.
    #[must_use]
    const fn from_lease(lease: &PresentFrameLease) -> Self {
        Self {
            render_generation: lease.render_generation,
            texture_handle: lease.frame.texture_handle,
        }
    }
}

/// Результат неблокирующего чтения latest present frame.
pub(crate) enum LatestPresentFrameAcquire {
    /// Latest-slot содержит lease, clone которого можно отдать renderer-у.
    Acquired(PlayerPresentFrame),

    /// Worker ещё не публиковал frame или уже очистил stale frame.
    Empty,

    /// Worker прямо сейчас обновляет slot; render thread должен reuse-нуть cache.
    Busy,
}

/// Latest-slot для передачи frame lease-а из worker thread в render thread без request/reply.
pub(crate) struct LatestPresentFrameHandoff {
    /// Единственный опубликованный lease; его clone-ы разделяют один RAII drop-ack.
    latest_frame: Mutex<Option<PlayerPresentFrame>>,
}

impl LatestPresentFrameHandoff {
    /// Создаёт пустой handoff slot.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            latest_frame: Mutex::new(None),
        }
    }

    /// Неблокирующе clone-ит latest frame для render hot path.
    pub(crate) fn try_clone_latest(&self) -> LatestPresentFrameAcquire {
        match self.latest_frame.try_lock() {
            Ok(guard) => Self::clone_from_guard(&guard),
            Err(TryLockError::WouldBlock) => LatestPresentFrameAcquire::Busy,
            Err(TryLockError::Poisoned(poisoned)) => {
                warn!("Latest present frame handoff mutex was poisoned; recovering slot");
                let guard = poisoned.into_inner();
                Self::clone_from_guard(&guard)
            }
        }
    }

    /// Публикует новый latest frame и dropping старого lease-а выполняет вне mutex guard-а.
    pub(crate) fn publish(&self, frame: Option<PlayerPresentFrame>) {
        let previous_frame = {
            let mut guard = self.latest_frame_guard();
            std::mem::replace(&mut *guard, frame)
        };
        drop(previous_frame);
    }

    /// Возвращает identity текущего slot-а, чтобы worker не создавал новый lease без причины.
    fn current_identity(&self) -> Option<PresentFrameLeaseIdentity> {
        self.latest_frame_guard()
            .as_ref()
            .map(PresentFrameLeaseIdentity::from_lease)
    }

    /// Очищает latest-slot и отдаёт release ack старого frame-а через RAII.
    pub(crate) fn clear(&self) {
        self.publish(None);
    }

    /// Clone-ит frame из уже полученного guard-а.
    fn clone_from_guard(guard: &Option<PlayerPresentFrame>) -> LatestPresentFrameAcquire {
        guard
            .as_ref()
            .cloned()
            .map(LatestPresentFrameAcquire::Acquired)
            .unwrap_or(LatestPresentFrameAcquire::Empty)
    }

    /// Берёт mutex для worker-side обновлений и восстанавливается после poison.
    fn latest_frame_guard(&self) -> MutexGuard<'_, Option<PlayerPresentFrame>> {
        match self.latest_frame.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("Latest present frame handoff mutex was poisoned; recovering slot");
                poisoned.into_inner()
            }
        }
    }
}

impl Default for LatestPresentFrameHandoff {
    /// Возвращает пустой latest-slot.
    fn default() -> Self {
        Self::new()
    }
}

/// Неблокирующий sample render acquisition latency.
pub(crate) struct RenderAcquireSample {
    /// Сколько заняла попытка получить latest frame на render thread.
    pub(crate) wait: Duration,
}

/// Неблокирующий sample renderer submit/present latency.
pub(crate) struct RenderTimingSample {
    /// Время от `queue.submit()` до возврата из `surface_texture.present()`.
    pub(crate) submit_present_elapsed: Duration,
}

/// Drop-ack от render-side frame lease.
pub(crate) struct RenderLeaseRelease {
    /// Поколение render resources, где lease был создан.
    pub(crate) render_generation: u64,

    /// Texture handle, который больше не удерживает render/UI side.
    pub(crate) texture_handle: video_core::FrameTextureHandle,

    /// Provider исходного decoder texture pool для release после смены поколения.
    pub(crate) texture_provider: Option<video_vaapi::VideoTextureViewProvider>,

    /// Монотонный момент drop-а render lease на render/UI side.
    pub(crate) released_at: Instant,
}

/// Shared guard, который отправляет release ack ровно один раз на группу clone-ов.
struct PresentFrameDropAck {
    /// Поколение render resources, где lease был создан.
    render_generation: u64,

    /// Texture handle, защищённый от premature release.
    texture_handle: video_core::FrameTextureHandle,

    /// Provider исходного frame-а; нужен, если ack пришёл после смены поколения.
    texture_provider: Option<video_vaapi::VideoTextureViewProvider>,

    /// Канал drop-ack обратно в playback worker.
    release_tx: Sender<RenderLeaseRelease>,
}

impl Drop for PresentFrameDropAck {
    /// Освобождает render lease без участия UI-кода.
    fn drop(&mut self) {
        let release = RenderLeaseRelease {
            render_generation: self.render_generation,
            texture_handle: self.texture_handle,
            texture_provider: self.texture_provider.clone(),
            released_at: Instant::now(),
        };

        self.send_release_ack(release);
    }
}

impl PresentFrameDropAck {
    /// Отправляет release ack через bounded queue с коротким backpressure budget.
    fn send_release_ack(&self, release: RenderLeaseRelease) {
        match self.release_tx.try_send(release) {
            Ok(()) => {}
            Err(TrySendError::Full(release)) => {
                self.send_release_ack_with_timeout(release);
            }
            Err(TrySendError::Disconnected(release)) => {
                Self::release_without_worker(release);
            }
        }
    }

    /// Даёт worker-у короткое окно на drain, но не блокирует render/UI thread навсегда.
    fn send_release_ack_with_timeout(&self, release: RenderLeaseRelease) {
        match self
            .release_tx
            .send_timeout(release, RENDER_RELEASE_SEND_TIMEOUT)
        {
            Ok(()) => {}
            Err(SendTimeoutError::Timeout(release)) => {
                warn!(
                    generation = release.render_generation,
                    texture_handle = release.texture_handle.0,
                    "Render release queue is full; releasing texture outside worker bookkeeping"
                );
                Self::release_without_worker(release);
            }
            Err(SendTimeoutError::Disconnected(release)) => {
                Self::release_without_worker(release);
            }
        }
    }

    /// Последний шанс освободить backend resource, когда worker уже недоступен или перегружен.
    fn release_without_worker(release: RenderLeaseRelease) {
        let Some(texture_provider) = release.texture_provider else {
            warn!(
                generation = release.render_generation,
                texture_handle = release.texture_handle.0,
                "Render release ack could not reach worker and has no texture provider fallback"
            );
            return;
        };

        texture_provider.release_frame(release.texture_handle);
    }
}

/// Shell/render-side handle, через который public `PlayerWorker` API остаётся прежним.
pub(crate) struct RenderLeaseBridgeClient {
    /// Shared latest-slot, из которого render thread неблокирующе clone-ит frame lease.
    latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,

    /// Неблокирующий канал latency samples render acquisition.
    render_acquire_sample_tx: Sender<RenderAcquireSample>,

    /// Неблокирующий канал GPU submit/present timing samples.
    render_timing_sample_tx: Sender<RenderTimingSample>,
}

impl RenderLeaseBridgeClient {
    /// Создаёт render-side handle поверх уже созданных worker-side каналов.
    fn new(
        latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,
        render_acquire_sample_tx: Sender<RenderAcquireSample>,
        render_timing_sample_tx: Sender<RenderTimingSample>,
    ) -> Self {
        Self {
            latest_present_frame_handoff,
            render_acquire_sample_tx,
            render_timing_sample_tx,
        }
    }

    /// Пытается получить текущий кадр для renderer-а без раскрытия `PlayerSession`.
    #[must_use]
    pub(crate) fn try_acquire_present_frame(&self) -> Option<PlayerPresentFrame> {
        let acquire_started_at = Instant::now();
        let acquire_result = self.latest_present_frame_handoff.try_clone_latest();
        self.report_render_acquire_sample(acquire_started_at.elapsed());

        match acquire_result {
            LatestPresentFrameAcquire::Acquired(frame) => Some(frame),
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => None,
        }
    }

    /// Сообщает worker-у latency render acquisition без блокировки render thread.
    fn report_render_acquire_sample(&self, wait: Duration) {
        let _ = self
            .render_acquire_sample_tx
            .try_send(RenderAcquireSample { wait });
    }

    /// Сообщает worker-у renderer submit/present timing без блокировки render thread.
    pub(crate) fn report_gpu_submit_present_latency(&self, submit_present_elapsed: Duration) {
        let _ = self.render_timing_sample_tx.try_send(RenderTimingSample {
            submit_present_elapsed,
        });
    }

    /// Создаёт test client с внешним latest-slot-ом для проверки public worker API.
    #[cfg(test)]
    pub(crate) fn with_handoff_for_tests(
        latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,
    ) -> (
        Self,
        Receiver<RenderAcquireSample>,
        Receiver<RenderTimingSample>,
    ) {
        let (render_acquire_sample_tx, render_acquire_sample_rx) =
            bounded(RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (render_timing_sample_tx, render_timing_sample_rx) =
            bounded(RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY);

        (
            Self::new(
                latest_present_frame_handoff,
                render_acquire_sample_tx,
                render_timing_sample_tx,
            ),
            render_acquire_sample_rx,
            render_timing_sample_rx,
        )
    }
}

/// Worker-side владелец render lease handoff, release ack и render diagnostics потоков.
pub(crate) struct RenderLeaseBridge {
    /// Shared latest-slot для публикации frame lease-а render thread-у.
    latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,

    /// Sender render lease release ack-ов для новых present frames.
    render_release_tx: Sender<RenderLeaseRelease>,

    /// Receiver render lease release ack-ов от UI/render side.
    render_release_rx: Receiver<RenderLeaseRelease>,

    /// Receiver неблокирующих latency samples от render thread.
    render_acquire_sample_rx: Receiver<RenderAcquireSample>,

    /// Receiver неблокирующих GPU submit/present timing samples от render thread.
    render_timing_sample_rx: Receiver<RenderTimingSample>,
}

impl RenderLeaseBridge {
    /// Создаёт worker-side bridge и shell-facing client для прежнего public API.
    pub(crate) fn new() -> (Self, RenderLeaseBridgeClient) {
        let (render_release_tx, render_release_rx) = bounded(RENDER_RELEASE_CHANNEL_CAPACITY);
        let (render_acquire_sample_tx, render_acquire_sample_rx) =
            bounded(RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (render_timing_sample_tx, render_timing_sample_rx) =
            bounded(RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY);
        let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
        let client = RenderLeaseBridgeClient::new(
            Arc::clone(&latest_present_frame_handoff),
            render_acquire_sample_tx,
            render_timing_sample_tx,
        );

        (
            Self {
                latest_present_frame_handoff,
                render_release_tx,
                render_release_rx,
                render_acquire_sample_rx,
                render_timing_sample_rx,
            },
            client,
        )
    }

    /// Возвращает receiver clone для `select!`, не отдавая владение очередью runtime-у.
    #[must_use]
    pub(crate) fn render_release_receiver(&self) -> Receiver<RenderLeaseRelease> {
        self.render_release_rx.clone()
    }

    /// Возвращает receiver clone для `select!`, не отдавая владение diagnostics очередью.
    #[must_use]
    pub(crate) fn render_acquire_sample_receiver(&self) -> Receiver<RenderAcquireSample> {
        self.render_acquire_sample_rx.clone()
    }

    /// Возвращает receiver clone для `select!`, не отдавая владение timing очередью.
    #[must_use]
    pub(crate) fn render_timing_sample_receiver(&self) -> Receiver<RenderTimingSample> {
        self.render_timing_sample_rx.clone()
    }

    /// Обрабатывает wakeup от bounded release ack queue.
    pub(crate) fn handle_release_wakeup(
        &mut self,
        session: &mut PlayerSession,
        release_result: Result<RenderLeaseRelease, RecvError>,
    ) {
        if let Ok(release) = release_result {
            Self::record_release(session, release);
        }
    }

    /// Обрабатывает wakeup от render-acquire diagnostics stream.
    pub(crate) fn handle_acquire_sample_wakeup(
        &mut self,
        session: &mut PlayerSession,
        sample_result: Result<RenderAcquireSample, RecvError>,
    ) {
        if let Ok(sample) = sample_result {
            Self::record_acquire_sample(session, sample);
            self.drain_render_acquire_samples(session);
        }
    }

    /// Обрабатывает wakeup от renderer submit/present diagnostics stream.
    pub(crate) fn handle_timing_sample_wakeup(
        &mut self,
        session: &mut PlayerSession,
        sample_result: Result<RenderTimingSample, RecvError>,
    ) {
        if let Ok(sample) = sample_result {
            Self::record_timing_sample(session, sample);
            self.drain_render_timing_samples(session);
        }
    }

    /// Снимает все render leases, которые UI/render side уже dropped.
    pub(crate) fn drain_releases(&mut self, session: &mut PlayerSession) {
        while let Ok(release) = self.render_release_rx.try_recv() {
            Self::record_release(session, release);
        }
    }

    /// Снимает render diagnostics samples, которые render thread отправил без ожидания worker-а.
    pub(crate) fn drain_diagnostics(&mut self, session: &mut PlayerSession) {
        self.drain_render_acquire_samples(session);
        self.drain_render_timing_samples(session);
    }

    /// Публикует latest present frame lease, если worker-side frame identity изменилась.
    pub(crate) fn publish_latest_present_frame(&mut self, session: &mut PlayerSession) {
        self.drain_releases(session);
        let Some(current_identity) = Self::current_present_frame_identity(session) else {
            self.latest_present_frame_handoff.clear();
            return;
        };

        if self.latest_present_frame_handoff.current_identity() == Some(current_identity) {
            return;
        }

        let present_frame = self.build_present_frame(session);
        self.latest_present_frame_handoff.publish(present_frame);
    }

    /// Возвращает identity текущего present frame без создания нового render lease-а.
    fn current_present_frame_identity(
        session: &PlayerSession,
    ) -> Option<PresentFrameLeaseIdentity> {
        session.pipeline.video_decoder_thread.as_ref()?;
        session
            .pipeline
            .present_video_frame
            .as_ref()
            .map(|frame| PresentFrameLeaseIdentity {
                render_generation: session.pipeline.render_generation,
                texture_handle: frame.texture_handle,
            })
    }

    /// Собирает renderable frame без передачи `PlayerSession` наружу.
    fn build_present_frame(&mut self, session: &mut PlayerSession) -> Option<PlayerPresentFrame> {
        let leased_frame = session.lease_present_video_frame()?;
        Some(PlayerPresentFrame::from_leased_frame(
            leased_frame,
            self.render_release_tx.clone(),
        ))
    }

    /// Снимает render acquire latency samples, которые render thread отправил без ожидания worker-а.
    fn drain_render_acquire_samples(&mut self, session: &mut PlayerSession) {
        while let Ok(sample) = self.render_acquire_sample_rx.try_recv() {
            Self::record_acquire_sample(session, sample);
        }
    }

    /// Снимает renderer submit/present timing samples, которые render thread отправил без ожидания worker-а.
    fn drain_render_timing_samples(&mut self, session: &mut PlayerSession) {
        while let Ok(sample) = self.render_timing_sample_rx.try_recv() {
            Self::record_timing_sample(session, sample);
        }
    }

    /// Записывает release ack в session diagnostics и снимает render lease.
    fn record_release(session: &mut PlayerSession, release: RenderLeaseRelease) {
        session.record_release_ack_latency(release.released_at.elapsed());
        session.release_render_lease_with_provider(
            release.render_generation,
            release.texture_handle,
            release.texture_provider.as_ref(),
        );
    }

    /// Записывает render acquire latency sample в session diagnostics.
    fn record_acquire_sample(session: &mut PlayerSession, sample: RenderAcquireSample) {
        session.record_render_acquire_wait(sample.wait);
    }

    /// Записывает renderer submit/present latency sample в session diagnostics.
    fn record_timing_sample(session: &mut PlayerSession, sample: RenderTimingSample) {
        session.record_gpu_submit_present_latency(sample.submit_present_elapsed);
    }

    /// Возвращает release sender для unit-тестов bridge wakeup/drain поведения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn release_sender_for_tests(&self) -> Sender<RenderLeaseRelease> {
        self.render_release_tx.clone()
    }

    /// Читает latest-slot в unit-тестах без прохождения через public worker API.
    #[cfg(test)]
    pub(crate) fn try_clone_latest_for_tests(&self) -> LatestPresentFrameAcquire {
        self.latest_present_frame_handoff.try_clone_latest()
    }
}
