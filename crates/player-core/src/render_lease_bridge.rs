#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvError, SendTimeoutError, Sender, TrySendError, bounded};
use tracing::{debug, warn};
use video_present_core::{
    SharedVideoFrameLeaseDiagnosticsSink, SharedVideoFrameReleaseSink, VideoFrameLease,
    VideoFrameLeaseConfig, VideoFrameLeaseDiagnosticsSink, VideoFrameRelease,
    VideoFrameReleaseFallbackReason, VideoFrameReleaseOutcome, VideoFrameReleaseSink,
    VideoFrameResourceLookupSample,
};

use crate::PresentFrameResourceProviderHandle;
#[cfg(test)]
use crate::decoder_boundary::PresentFrameResourceProvider;
#[cfg(test)]
use crate::decoder_boundary::PresentFrameResourceProviderLookup;
use crate::session::{LeasedPresentFrame, PlayerSession, PresentFrameIdentity};

mod ordered_present_handoff;

pub(crate) use ordered_present_handoff::OrderedPresentFrameHandoff;
use ordered_present_handoff::OrderedPresentFramePublish;

/// Ёмкость release ack stream; защищает worker от бесконечного роста drop-ack очереди.
const RENDER_RELEASE_CHANNEL_CAPACITY: usize = 512;

/// Ёмкость неблокирующих render-acquire latency samples.
const RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Ёмкость неблокирующих GPU submit/present latency samples.
const RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Ёмкость samples ожидания backend resource lock-а на render hot path.
const RENDER_RESOURCE_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Ёмкость samples reuse предыдущего renderable frame-а при busy resource lock-е.
const RENDER_RESOURCE_REUSE_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Короткий backpressure budget для Drop path render lease-а.
const RENDER_RELEASE_SEND_TIMEOUT: Duration = Duration::from_millis(2);

/// Собирает stable identity из lease-а, который уже безопасно опубликован render-side.
fn present_frame_identity_from_lease(lease: &VideoFrameLease) -> PresentFrameIdentity {
    PresentFrameIdentity::from_decoded_frame(lease.render_generation(), lease.decoded_frame())
}

/// Результат неблокирующего чтения latest present frame.
#[expect(
    clippy::large_enum_variant,
    reason = "Render acquire hot path сразу матчится на результате; Box добавил бы heap allocation на каждый frame acquire без изменения ownership semantics."
)]
pub(crate) enum LatestPresentFrameAcquire {
    /// Latest-slot содержит lease, clone которого можно отдать renderer-у.
    Acquired(VideoFrameLease),

    /// Worker ещё не публиковал frame или уже очистил stale frame.
    Empty,

    /// Worker прямо сейчас обновляет slot; render thread должен reuse-нуть cache.
    Busy,
}

/// Что изменилось в слоте показа после публикации worker-ом (cpu-parity, сессия 04).
///
/// По этому итогу worker решает, будить ли UI-поток: `Unchanged` — нечего показывать нового,
/// `Published`/`Cleared` — картинка на экране должна измениться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "по итогу публикации worker будит UI; молча терять его нельзя"]
pub(crate) enum PresentFramePublication {
    /// Слот не менялся: тот же кадр, что и раньше (или по-прежнему пусто).
    Unchanged,
    /// В слот положен новый кадр.
    Published,
    /// Слот очищен: кадра для показа больше нет.
    Cleared,
}

impl PresentFramePublication {
    /// `true`, если UI-поток должен проснуться и перерисовать видео.
    #[must_use]
    pub(crate) const fn changes_presentation(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// Итог неблокирующего чтения identity слота показа (без clone lease-а).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentFrameIdentityProbe {
    /// Слот прочитан: `None` — кадра для показа нет.
    Known(Option<video_present_core::VideoPresentFrameIdentity>),
    /// Worker прямо сейчас обновляет слот; читающий обязан считать кадр изменившимся.
    Busy,
}

/// Latest-slot для передачи frame lease-а из worker thread в render thread без request/reply.
///
/// С сессии 06a служит только scrub visual override: там нужен именно последний кадр жеста.
/// Кадры воспроизведения идут через упорядоченную `OrderedPresentFrameHandoff`, которая не
/// перезаписывает незабранный UI кадр.
pub(crate) struct LatestPresentFrameHandoff {
    /// Единственный опубликованный lease; его clone-ы разделяют один RAII drop-ack.
    latest_frame: Mutex<Option<VideoFrameLease>>,
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
    ///
    /// Только тесты: в production scrub-override слот пока никто не заполняет (UI его лишь
    /// читает), а кадры воспроизведения с сессии 06a идут через `OrderedPresentFrameHandoff`.
    #[cfg(test)]
    pub(crate) fn publish(&self, frame: Option<VideoFrameLease>) {
        let previous_frame = {
            let mut guard = self.latest_frame_guard();
            std::mem::replace(&mut *guard, frame)
        };
        drop(previous_frame);
    }

    /// Очищает latest-slot и отдаёт release ack старого frame-а через RAII.
    #[cfg(test)]
    pub(crate) fn clear(&self) {
        self.publish(None);
    }

    /// Clone-ит frame из уже полученного guard-а.
    fn clone_from_guard(guard: &Option<VideoFrameLease>) -> LatestPresentFrameAcquire {
        guard
            .as_ref()
            .cloned()
            .map(LatestPresentFrameAcquire::Acquired)
            .unwrap_or(LatestPresentFrameAcquire::Empty)
    }

    /// Берёт mutex для worker-side обновлений и восстанавливается после poison.
    #[cfg(test)]
    fn latest_frame_guard(&self) -> MutexGuard<'_, Option<VideoFrameLease>> {
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
    /// Время от `queue.submit()` до возврата из `queue.present(surface_texture)`.
    pub(crate) submit_present_elapsed: Duration,
}

/// Неблокирующий sample ожидания backend resource pool lock-а.
pub(crate) struct RenderResourceLockSample {
    /// Сколько render thread ждал mutex backend resource pool-а.
    pub(crate) wait: Duration,

    /// Был ли lookup остановлен из-за занятого resource pool lock-а.
    pub(crate) lookup_was_busy: bool,

    /// PTS кадра, для которого renderer запросил resource materialization.
    pub(crate) pts: Option<Duration>,

    /// Memory path кадра для сопоставления zero-copy/upload path-ов.
    pub(crate) memory_path: Option<video_core::FrameMemoryPath>,
}

/// Неблокирующий sample reuse предыдущего renderable frame-а при busy resource lock-е.
pub(crate) struct RenderResourcePreviousFrameReuseSample;

/// Drop-ack от render-side frame lease.
pub(crate) struct RenderLeaseRelease {
    /// Поколение render resources, где lease был создан.
    pub(crate) render_generation: u64,

    /// Texture handle, который больше не удерживает render/UI side.
    pub(crate) resource_handle: video_core::FrameResourceHandle,

    /// Provider исходного decoder resource pool для release после смены поколения.
    pub(crate) resource_provider: Option<PresentFrameResourceProviderHandle>,

    /// Был ли lease реально включён в renderer submit.
    pub(crate) submitted_to_renderer: bool,

    /// Монотонный момент drop-а render lease на render/UI side.
    pub(crate) released_at: Instant,
}

impl RenderLeaseRelease {
    /// Переводит общий lease release в player-owned queue DTO.
    fn from_video_release(release: VideoFrameRelease) -> Self {
        Self {
            render_generation: release.render_generation(),
            resource_handle: release.resource_handle(),
            resource_provider: release.resource_provider().cloned(),
            submitted_to_renderer: release.submitted_to_renderer(),
            released_at: release.released_at(),
        }
    }
}

/// Player-owned release sink: сохраняет accounting path и fallback release policy.
pub(crate) struct RenderLeaseReleaseSink {
    /// Канал drop-ack обратно в playback worker.
    release_tx: Sender<RenderLeaseRelease>,
}

impl RenderLeaseReleaseSink {
    /// Создаёт sink для конкретной bounded release queue.
    pub(crate) fn new(release_tx: Sender<RenderLeaseRelease>) -> Self {
        Self { release_tx }
    }

    /// Отправляет release ack через bounded queue с коротким backpressure budget.
    fn send_release_ack(&self, release: RenderLeaseRelease) -> VideoFrameReleaseOutcome {
        match self.release_tx.try_send(release) {
            Ok(()) => VideoFrameReleaseOutcome::Accepted,
            Err(TrySendError::Full(release)) => self.send_release_ack_with_timeout(release),
            Err(TrySendError::Disconnected(release)) => {
                Self::release_without_worker(release, VideoFrameReleaseFallbackReason::Disconnected)
            }
        }
    }

    /// Даёт worker-у короткое окно на drain, но не блокирует render/UI thread навсегда.
    fn send_release_ack_with_timeout(
        &self,
        release: RenderLeaseRelease,
    ) -> VideoFrameReleaseOutcome {
        match self
            .release_tx
            .send_timeout(release, RENDER_RELEASE_SEND_TIMEOUT)
        {
            Ok(()) => VideoFrameReleaseOutcome::Accepted,
            Err(SendTimeoutError::Timeout(release)) => {
                warn!(
                    generation = release.render_generation,
                    resource_handle = release.resource_handle.0,
                    "Render release queue is full; releasing texture outside worker bookkeeping"
                );
                Self::release_without_worker(release, VideoFrameReleaseFallbackReason::Backpressure)
            }
            Err(SendTimeoutError::Disconnected(release)) => {
                Self::release_without_worker(release, VideoFrameReleaseFallbackReason::Disconnected)
            }
        }
    }

    /// Последний шанс освободить backend resource, когда worker уже недоступен или перегружен.
    fn release_without_worker(
        release: RenderLeaseRelease,
        reason: VideoFrameReleaseFallbackReason,
    ) -> VideoFrameReleaseOutcome {
        let Some(resource_provider) = release.resource_provider else {
            warn!(
                generation = release.render_generation,
                resource_handle = release.resource_handle.0,
                "Render release ack could not reach worker and has no resource provider fallback"
            );
            return VideoFrameReleaseOutcome::FallbackUnavailable { reason };
        };

        resource_provider.release_frame(release.resource_handle);
        VideoFrameReleaseOutcome::FallbackReleased { reason }
    }
}

impl VideoFrameReleaseSink for RenderLeaseReleaseSink {
    /// Сохраняет current playback accounting: сначала worker queue, затем typed fallback.
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        self.send_release_ack(RenderLeaseRelease::from_video_release(release))
    }
}

/// Player-owned diagnostics sink для samples, которые производит общий lease API.
struct RenderLeaseDiagnosticsSink {
    /// Неблокирующий канал sample-ов ожидания backend resource pool lock-а.
    resource_lock_sample_tx: Sender<RenderResourceLockSample>,
}

impl RenderLeaseDiagnosticsSink {
    /// Создаёт sink для worker-owned diagnostics queue.
    fn new(resource_lock_sample_tx: Sender<RenderResourceLockSample>) -> Self {
        Self {
            resource_lock_sample_tx,
        }
    }
}

impl VideoFrameLeaseDiagnosticsSink for RenderLeaseDiagnosticsSink {
    /// Переводит общий diagnostics sample в player diagnostics queue.
    fn report_resource_lookup_sample(&self, sample: VideoFrameResourceLookupSample) {
        let _ = self
            .resource_lock_sample_tx
            .try_send(RenderResourceLockSample {
                wait: sample.wait(),
                pts: sample.pts(),
                memory_path: sample.memory_path(),
                lookup_was_busy: sample.lookup_was_busy(),
            });
    }
}

/// Собирает общий lease config из player-owned leased frame и local sinks.
fn build_video_frame_lease(
    leased_frame: LeasedPresentFrame,
    release_sink: SharedVideoFrameReleaseSink,
    diagnostics_sink: SharedVideoFrameLeaseDiagnosticsSink,
) -> VideoFrameLease {
    let mut config = VideoFrameLeaseConfig::new(
        leased_frame.render_generation,
        leased_frame.frame.clone(),
        release_sink,
    )
    .with_resource_provider(leased_frame.resource_provider)
    .with_diagnostics_sink(diagnostics_sink);

    if leased_frame.stale {
        config = config.with_timeline_stale();
    }

    VideoFrameLease::new(config)
}

/// Shell/render-side handle, через который public `PlayerWorker` API остаётся прежним.
pub(crate) struct RenderLeaseBridgeClient {
    /// Упорядоченная передача кадров показа: render thread неблокирующе забирает следующий
    /// кадр, незабранные не перезаписываются (сессия 06a).
    present_frame_handoff: Arc<OrderedPresentFrameHandoff>,

    /// Отдельный latest-slot для scrub visual override; playback acquisition его не читает.
    latest_scrub_visual_override_handoff: Arc<LatestPresentFrameHandoff>,

    /// Неблокирующий канал latency samples render acquisition.
    render_acquire_sample_tx: Sender<RenderAcquireSample>,

    /// Неблокирующий канал GPU submit/present timing samples.
    render_timing_sample_tx: Sender<RenderTimingSample>,

    /// Неблокирующий канал samples reuse предыдущего renderable frame-а.
    resource_previous_frame_reuse_sample_tx: Sender<RenderResourcePreviousFrameReuseSample>,
}

impl RenderLeaseBridgeClient {
    /// Создаёт render-side handle поверх уже созданных worker-side каналов.
    fn new(
        present_frame_handoff: Arc<OrderedPresentFrameHandoff>,
        latest_scrub_visual_override_handoff: Arc<LatestPresentFrameHandoff>,
        render_acquire_sample_tx: Sender<RenderAcquireSample>,
        render_timing_sample_tx: Sender<RenderTimingSample>,
        resource_previous_frame_reuse_sample_tx: Sender<RenderResourcePreviousFrameReuseSample>,
    ) -> Self {
        Self {
            present_frame_handoff,
            latest_scrub_visual_override_handoff,
            render_acquire_sample_tx,
            render_timing_sample_tx,
            resource_previous_frame_reuse_sample_tx,
        }
    }

    /// Забирает следующий кадр для renderer-а без раскрытия `PlayerSession`.
    ///
    /// Кадры одной последовательности приходят по порядку: если worker успел опубликовать
    /// два кадра между двумя вызовами, второй вызов вернёт следующий, а не пропустит его.
    /// Без нового кадра возвращается уже забранный (тот же identity).
    #[must_use]
    pub(crate) fn try_acquire_present_frame(&self) -> Option<VideoFrameLease> {
        let acquire_started_at = Instant::now();
        let acquire_result = self.present_frame_handoff.try_take_next();
        self.report_render_acquire_sample(acquire_started_at.elapsed());

        match acquire_result {
            LatestPresentFrameAcquire::Acquired(frame) => Some(frame),
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => None,
        }
    }

    /// Неблокирующе читает identity кадра, который вернёт следующий
    /// `try_acquire_present_frame`, без clone lease-а и без diagnostics sample.
    #[must_use]
    pub(crate) fn try_present_frame_identity(&self) -> PresentFrameIdentityProbe {
        self.present_frame_handoff.try_next_identity()
    }

    /// Пытается получить scrub visual override lease без смешивания с playback latest-slot.
    #[must_use]
    pub(crate) fn try_acquire_scrub_visual_override_frame(&self) -> Option<VideoFrameLease> {
        match self.latest_scrub_visual_override_handoff.try_clone_latest() {
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

    /// Сообщает worker-у, что render path переиспользовал previous valid frame из-за busy lock-а.
    pub(crate) fn report_resource_previous_frame_reuse(&self) {
        let _ = self
            .resource_previous_frame_reuse_sample_tx
            .try_send(RenderResourcePreviousFrameReuseSample);
    }

    /// Создаёт test client с внешним latest-slot-ом для проверки public worker API.
    #[cfg(test)]
    pub(crate) fn with_handoff_for_tests(
        present_frame_handoff: Arc<OrderedPresentFrameHandoff>,
        latest_scrub_visual_override_handoff: Arc<LatestPresentFrameHandoff>,
    ) -> (
        Self,
        Receiver<RenderAcquireSample>,
        Receiver<RenderTimingSample>,
        Receiver<RenderResourcePreviousFrameReuseSample>,
    ) {
        let (render_acquire_sample_tx, render_acquire_sample_rx) =
            bounded(RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (render_timing_sample_tx, render_timing_sample_rx) =
            bounded(RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (resource_previous_frame_reuse_sample_tx, resource_previous_frame_reuse_sample_rx) =
            bounded(RENDER_RESOURCE_REUSE_DIAGNOSTIC_CHANNEL_CAPACITY);

        (
            Self::new(
                present_frame_handoff,
                latest_scrub_visual_override_handoff,
                render_acquire_sample_tx,
                render_timing_sample_tx,
                resource_previous_frame_reuse_sample_tx,
            ),
            render_acquire_sample_rx,
            render_timing_sample_rx,
            resource_previous_frame_reuse_sample_rx,
        )
    }
}

/// Worker-side владелец render lease handoff, release ack и render diagnostics потоков.
pub(crate) struct RenderLeaseBridge {
    /// Упорядоченная передача кадров показа render thread-у (сессия 06a).
    present_frame_handoff: Arc<OrderedPresentFrameHandoff>,

    /// Sender render lease release ack-ов для новых present frames.
    render_release_tx: Sender<RenderLeaseRelease>,

    /// Receiver render lease release ack-ов от UI/render side.
    render_release_rx: Receiver<RenderLeaseRelease>,

    /// Receiver неблокирующих latency samples от render thread.
    render_acquire_sample_rx: Receiver<RenderAcquireSample>,

    /// Receiver неблокирующих GPU submit/present timing samples от render thread.
    render_timing_sample_rx: Receiver<RenderTimingSample>,

    /// Sender samples ожидания resource lock-а для leases, которые публикует worker.
    resource_lock_sample_tx: Sender<RenderResourceLockSample>,

    /// Receiver samples ожидания resource lock-а от render thread.
    resource_lock_sample_rx: Receiver<RenderResourceLockSample>,

    /// Receiver samples reuse previous frame из-за busy resource lock-а.
    resource_previous_frame_reuse_sample_rx: Receiver<RenderResourcePreviousFrameReuseSample>,
}

impl RenderLeaseBridge {
    /// Создаёт worker-side bridge и shell-facing client для прежнего public API.
    pub(crate) fn new() -> (Self, RenderLeaseBridgeClient) {
        let (render_release_tx, render_release_rx) = bounded(RENDER_RELEASE_CHANNEL_CAPACITY);
        let (render_acquire_sample_tx, render_acquire_sample_rx) =
            bounded(RENDER_ACQUIRE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (render_timing_sample_tx, render_timing_sample_rx) =
            bounded(RENDER_TIMING_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (resource_lock_sample_tx, resource_lock_sample_rx) =
            bounded(RENDER_RESOURCE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (resource_previous_frame_reuse_sample_tx, resource_previous_frame_reuse_sample_rx) =
            bounded(RENDER_RESOURCE_REUSE_DIAGNOSTIC_CHANNEL_CAPACITY);
        let present_frame_handoff = Arc::new(OrderedPresentFrameHandoff::new());
        let latest_scrub_visual_override_handoff = Arc::new(LatestPresentFrameHandoff::new());
        let client = RenderLeaseBridgeClient::new(
            Arc::clone(&present_frame_handoff),
            Arc::clone(&latest_scrub_visual_override_handoff),
            render_acquire_sample_tx,
            render_timing_sample_tx,
            resource_previous_frame_reuse_sample_tx,
        );

        (
            Self {
                present_frame_handoff,
                render_release_tx,
                render_release_rx,
                render_acquire_sample_rx,
                render_timing_sample_rx,
                resource_lock_sample_tx,
                resource_lock_sample_rx,
                resource_previous_frame_reuse_sample_rx,
            },
            client,
        )
    }

    /// Возвращает receiver clone для `select!`, не отдавая владение очередью runtime-у.
    #[must_use]
    pub(crate) fn render_release_receiver(&self) -> Receiver<RenderLeaseRelease> {
        self.render_release_rx.clone()
    }

    /// Обрабатывает wakeup от bounded release ack queue.
    ///
    /// Это единственный render-feedback, который будит worker: release возвращает поверхность
    /// декодеру. Диагностические замеры (acquire/timing/lock/reuse) идут по неблокирующим
    /// каналам без собственных пробуждений и снимаются `drain_diagnostics` (сессия 04).
    pub(crate) fn handle_release_wakeup(
        &mut self,
        session: &mut PlayerSession,
        release_result: Result<RenderLeaseRelease, RecvError>,
    ) {
        if let Ok(release) = release_result {
            Self::record_release(session, release);
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
        self.drain_resource_lock_samples(session);
        self.drain_resource_previous_frame_reuse_samples(session);
    }

    /// Публикует кадр показа, если worker-side frame identity изменилась.
    ///
    /// Возвращает, изменился ли слот показа: по этому итогу worker будит UI-поток
    /// (`PlayerWorkerPresentationWake`), а не оставляет его ждать vsync. Кадр встаёт в
    /// упорядоченную передачу и не перезаписывает незабранный UI кадр (сессия 06a); сброс
    /// незабранного при переполнении считается в диагностике сессии.
    pub(crate) fn publish_present_frame(
        &mut self,
        session: &mut PlayerSession,
    ) -> PresentFramePublication {
        self.drain_releases(session);
        let Some(current_identity) = Self::current_present_frame_identity(session) else {
            return self.clear_present_frames();
        };

        if self.present_frame_handoff.newest_identity() == Some(current_identity) {
            return PresentFramePublication::Unchanged;
        }

        let Some(present_frame) = self.build_present_frame(session) else {
            // Identity есть, а lease не выдан (кадр без ресурса): показывать нечего, как и
            // раньше, когда в слот публиковался `None`.
            return self.clear_present_frames();
        };
        let handoff_publication = self.present_frame_handoff.publish(present_frame);
        Self::record_handoff_publication(session, handoff_publication, current_identity);
        PresentFramePublication::Published
    }

    /// Очищает передачу кадров показа и сообщает, была ли в ней картинка.
    fn clear_present_frames(&self) -> PresentFramePublication {
        let had_frame = self.present_frame_handoff.newest_identity().is_some();
        self.present_frame_handoff.clear();
        if had_frame {
            PresentFramePublication::Cleared
        } else {
            PresentFramePublication::Unchanged
        }
    }

    /// Учитывает итог публикации: сброс незабранного кадра — потеря показа, которую раньше
    /// не видел ни один счётчик; новая последовательность (seek) — не потеря.
    fn record_handoff_publication(
        session: &mut PlayerSession,
        handoff_publication: OrderedPresentFramePublish,
        published_identity: PresentFrameIdentity,
    ) {
        match handoff_publication {
            OrderedPresentFramePublish::Queued
            | OrderedPresentFramePublish::RestartedSequence { .. } => {}
            OrderedPresentFramePublish::DroppedOldestUntaken => {
                debug!(
                    published_pts_ms = published_identity.pts().as_secs_f64() * 1000.0,
                    "Present frame handoff dropped the oldest frame the UI had not taken yet"
                );
                session.record_present_frame_dropped_untaken();
            }
        }
    }

    /// Возвращает identity текущего present frame без создания нового render lease-а.
    fn current_present_frame_identity(session: &PlayerSession) -> Option<PresentFrameIdentity> {
        session.current_present_frame_identity()
    }

    /// Собирает renderable frame без передачи `PlayerSession` наружу.
    fn build_present_frame(&mut self, session: &mut PlayerSession) -> Option<VideoFrameLease> {
        let leased_frame = session.lease_present_video_frame()?;
        Some(build_video_frame_lease(
            leased_frame,
            Arc::new(RenderLeaseReleaseSink::new(self.render_release_tx.clone())),
            Arc::new(RenderLeaseDiagnosticsSink::new(
                self.resource_lock_sample_tx.clone(),
            )),
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

    /// Снимает samples ожидания resource pool lock-а без блокировки worker-а.
    fn drain_resource_lock_samples(&mut self, session: &mut PlayerSession) {
        while let Ok(sample) = self.resource_lock_sample_rx.try_recv() {
            Self::record_resource_lock_sample(session, sample);
        }
    }

    /// Снимает samples previous-frame reuse без блокировки worker-а.
    fn drain_resource_previous_frame_reuse_samples(&mut self, session: &mut PlayerSession) {
        while self
            .resource_previous_frame_reuse_sample_rx
            .try_recv()
            .is_ok()
        {
            Self::record_resource_previous_frame_reuse_sample(session);
        }
    }

    /// Записывает release ack в session diagnostics и снимает render lease.
    fn record_release(session: &mut PlayerSession, release: RenderLeaseRelease) {
        session.record_release_ack_latency(release.released_at.elapsed());
        session.release_render_lease_with_provider(
            release.render_generation,
            release.resource_handle,
            release.resource_provider.as_ref(),
            release.submitted_to_renderer,
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

    /// Записывает resource lock wait sample в session diagnostics.
    fn record_resource_lock_sample(session: &mut PlayerSession, sample: RenderResourceLockSample) {
        session.record_render_resource_lock_wait(sample.wait, sample.pts, sample.memory_path);
        if sample.lookup_was_busy {
            session.record_render_resource_lock_busy();
        }
    }

    /// Записывает reuse previous frame из-за busy resource lock-а.
    fn record_resource_previous_frame_reuse_sample(session: &mut PlayerSession) {
        session.record_render_resource_previous_frame_reuse();
    }

    /// Возвращает release sender для unit-тестов bridge wakeup/drain поведения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn release_sender_for_tests(&self) -> Sender<RenderLeaseRelease> {
        self.render_release_tx.clone()
    }

    /// Забирает следующий кадр показа в unit-тестах без прохождения через public worker API.
    #[cfg(test)]
    pub(crate) fn try_take_present_frame_for_tests(&self) -> LatestPresentFrameAcquire {
        self.present_frame_handoff.try_take_next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::VideoColorMetadata;
    use video_core::{FrameMemoryPath, FrameResourceHandle};
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
    use video_present_core::{VideoPresentFrameResourceKind, VideoPresentFrameResourceLookup};

    /// Provider, который не создаёт GPU handles, но возвращает измеренный lock wait.
    struct MissingResourceProvider {
        /// Handle, который должен запросить render lease.
        expected_handle: FrameResourceHandle,

        /// Synthetic wait, имитирующий ожидание resource pool mutex-а.
        lock_wait: Duration,
    }

    impl PresentFrameResourceProvider for MissingResourceProvider {
        /// Возвращает missing resource, сохраняя lock wait diagnostics.
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: self.lock_wait,
            }
        }

        /// Release в этом тесте не должен доходить до backend provider-а.
        fn release_frame(&self, _handle: FrameResourceHandle) {}
    }

    /// Provider, который имитирует занятый backend resource pool.
    struct BusyResourceProvider {
        /// Handle, который должен запросить render lease.
        expected_handle: FrameResourceHandle,

        /// Synthetic wait, имитирующий короткую try_lock попытку.
        lock_wait: Duration,
    }

    impl PresentFrameResourceProvider for BusyResourceProvider {
        /// Blocking compatibility path в этом тесте не используется.
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.try_resource_lookup(handle)
        }

        /// Возвращает busy lookup, не смешивая его с missing resource.
        fn try_resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: self.lock_wait,
            }
        }

        /// Release в этом тесте не должен доходить до backend provider-а.
        fn release_frame(&self, _handle: FrameResourceHandle) {}
    }

    /// Provider, который имитирует fatal/poisoned lookup state.
    struct ErrorResourceProvider {
        /// Handle, который должен запросить render lease.
        expected_handle: FrameResourceHandle,

        /// Synthetic wait, имитирующий попытку lookup-а до ошибки.
        lock_wait: Duration,
    }

    impl PresentFrameResourceProvider for ErrorResourceProvider {
        /// Возвращает error lookup для проверки typed boundary.
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: self.lock_wait,
            }
        }

        /// Non-blocking path сохраняет тот же fatal outcome.
        fn try_resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.resource_lookup(handle)
        }

        /// Release в этом тесте не должен доходить до backend provider-а.
        fn release_frame(&self, _handle: FrameResourceHandle) {}
    }

    /// Provider, который записывает fallback release-и для проверки аварийного пути bridge-а.
    struct RecordingFallbackProvider {
        /// Handle, который должен быть освобождён через provider fallback.
        expected_handle: FrameResourceHandle,

        /// Общий журнал release-вызовов, потому что provider хранится за Arc boundary.
        released_handles: Arc<Mutex<Vec<FrameResourceHandle>>>,
    }

    impl PresentFrameResourceProvider for RecordingFallbackProvider {
        /// Lookup в этих тестах не используется: проверяется только release fallback.
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: Duration::ZERO,
            }
        }

        /// Записывает backend release, если worker queue недоступна или переполнена.
        fn release_frame(&self, handle: FrameResourceHandle) {
            assert_eq!(handle, self.expected_handle);
            self.released_handles
                .lock()
                .expect("fallback release log mutex should not be poisoned")
                .push(handle);
        }
    }

    /// Создаёт decoded frame без GPU handles для проверки render lease boundary.
    fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            generation: 0,
            pts: Duration::from_millis(42),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Собирает lease с fake resource provider-ом, сохраняя production RAII drop-ack contract.
    fn lease_with_resource_provider_for_tests(
        render_generation: u64,
        resource_handle: FrameResourceHandle,
        release_tx: Sender<RenderLeaseRelease>,
        sample_tx: Sender<RenderResourceLockSample>,
        resource_provider: impl PresentFrameResourceProvider + 'static,
    ) -> VideoFrameLease {
        let resource_provider = PresentFrameResourceProviderHandle::new(resource_provider);
        VideoFrameLease::new(
            VideoFrameLeaseConfig::new(
                render_generation,
                decoded_frame_for_tests(resource_handle),
                Arc::new(RenderLeaseReleaseSink::new(release_tx)),
            )
            .with_resource_provider(resource_provider)
            .with_diagnostics_sink(Arc::new(RenderLeaseDiagnosticsSink::new(sample_tx))),
        )
    }

    /// Собирает lease без provider-а для тестов absent-resource path-а.
    fn lease_without_resource_provider_for_tests(
        render_generation: u64,
        frame: video_core::DecodedFrame,
        release_tx: Sender<RenderLeaseRelease>,
    ) -> VideoFrameLease {
        VideoFrameLease::new(VideoFrameLeaseConfig::new(
            render_generation,
            frame,
            Arc::new(RenderLeaseReleaseSink::new(release_tx)),
        ))
    }

    /// Собирает release ack-заглушку, которая занимает bounded queue в backpressure тесте.
    fn queued_release_for_tests(resource_handle: FrameResourceHandle) -> RenderLeaseRelease {
        RenderLeaseRelease {
            render_generation: 0,
            resource_handle,
            resource_provider: None,
            submitted_to_renderer: false,
            released_at: Instant::now(),
        }
    }

    /// Собирает общий release DTO для прямой проверки bridge sink-а.
    fn video_release_for_tests(
        render_generation: u64,
        resource_handle: FrameResourceHandle,
        resource_provider: Option<PresentFrameResourceProviderHandle>,
        renderer_submission: video_present_core::VideoFrameRendererSubmission,
    ) -> VideoFrameRelease {
        VideoFrameRelease::new(
            render_generation,
            resource_handle,
            resource_provider,
            renderer_submission,
            Instant::now(),
        )
    }

    #[test]
    fn resource_descriptor_exposes_renderer_neutral_dma_buf_handle() {
        let resource_handle = FrameResourceHandle(76);
        let (release_tx, _release_rx) = bounded(1);
        let frame = decoded_frame_for_tests(resource_handle);
        let lease = lease_without_resource_provider_for_tests(8, frame, release_tx);

        let descriptor = lease.resource_descriptor();

        assert_eq!(
            descriptor.kind(),
            VideoPresentFrameResourceKind::DmaBufZeroCopy
        );
        assert_eq!(descriptor.render_generation(), 8);
        assert_eq!(descriptor.resource_handle(), resource_handle);
        assert_eq!(descriptor.memory_path(), FrameMemoryPath::DmaBufZeroCopy);
    }

    #[test]
    fn try_resource_lookup_reports_missing_without_provider() {
        let resource_handle = FrameResourceHandle(77);
        let (release_tx, _release_rx) = bounded(1);
        let frame = decoded_frame_for_tests(resource_handle);
        let lease = lease_without_resource_provider_for_tests(9, frame, release_tx);

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Missing
        ));
    }

    #[test]
    fn try_resource_lookup_reports_busy_without_ready_resource() {
        let resource_handle = FrameResourceHandle(78);
        let lock_wait = Duration::from_micros(70);
        let (release_tx, _release_rx) = bounded(1);
        let (sample_tx, sample_rx) = bounded(1);
        let lease = lease_with_resource_provider_for_tests(
            9,
            resource_handle,
            release_tx,
            sample_tx,
            BusyResourceProvider {
                expected_handle: resource_handle,
                lock_wait,
            },
        );

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Busy
        ));

        let sample = sample_rx
            .try_recv()
            .expect("busy resource lookup sample should be queued");
        assert_eq!(sample.wait, lock_wait);
        assert!(sample.lookup_was_busy);
    }

    #[test]
    fn try_resource_lookup_reports_error_without_collapsing_to_missing() {
        let resource_handle = FrameResourceHandle(79);
        let lock_wait = Duration::from_micros(120);
        let (release_tx, _release_rx) = bounded(1);
        let (sample_tx, sample_rx) = bounded(1);
        let lease = lease_with_resource_provider_for_tests(
            9,
            resource_handle,
            release_tx,
            sample_tx,
            ErrorResourceProvider {
                expected_handle: resource_handle,
                lock_wait,
            },
        );

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Fatal
        ));

        let sample = sample_rx
            .try_recv()
            .expect("error resource lookup sample should be queued");
        assert_eq!(sample.wait, lock_wait);
        assert!(!sample.lookup_was_busy);
    }

    #[test]
    fn try_resource_lookup_reports_missing_with_lock_wait_sample() {
        let resource_handle = FrameResourceHandle(80);
        let lock_wait = Duration::from_micros(250);
        let (release_tx, _release_rx) = bounded(1);
        let (sample_tx, sample_rx) = bounded(1);
        let lease = lease_with_resource_provider_for_tests(
            9,
            resource_handle,
            release_tx,
            sample_tx,
            MissingResourceProvider {
                expected_handle: resource_handle,
                lock_wait,
            },
        );

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Missing
        ));

        let sample = sample_rx
            .try_recv()
            .expect("resource lock wait sample should be queued");
        assert_eq!(sample.wait, lock_wait);
        assert!(!sample.lookup_was_busy);
        assert_eq!(sample.pts, Some(Duration::from_millis(42)));
        assert_eq!(sample.memory_path, Some(FrameMemoryPath::DmaBufZeroCopy));
    }

    #[test]
    fn drop_ack_keeps_renderer_owned_release_path_renderer_neutral() {
        let resource_handle = FrameResourceHandle(81);
        let lock_wait = Duration::from_micros(75);
        let (release_tx, release_rx) = bounded(1);
        let (sample_tx, sample_rx) = bounded(1);
        let lease = lease_with_resource_provider_for_tests(
            9,
            resource_handle,
            release_tx,
            sample_tx,
            BusyResourceProvider {
                expected_handle: resource_handle,
                lock_wait,
            },
        );

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Busy
        ));

        let sample = sample_rx
            .try_recv()
            .expect("busy resource lookup sample should be queued");
        assert_eq!(sample.wait, lock_wait);
        assert!(sample.lookup_was_busy);

        drop(lease);

        let release = release_rx
            .try_recv()
            .expect("dropping resource lease should queue release ack");
        assert_eq!(release.render_generation, 9);
        assert_eq!(release.resource_handle, resource_handle);
        assert!(release.resource_provider.is_some());
        assert!(!release.submitted_to_renderer);
    }

    #[test]
    fn full_release_queue_reports_backpressure_fallback_and_releases_provider() {
        let queued_handle = FrameResourceHandle(82);
        let fallback_handle = FrameResourceHandle(83);
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let (release_tx, release_rx) = bounded(1);
        release_tx
            .try_send(queued_release_for_tests(queued_handle))
            .expect("test release queue should accept pre-filled ack");
        let resource_provider =
            PresentFrameResourceProviderHandle::new(RecordingFallbackProvider {
                expected_handle: fallback_handle,
                released_handles: Arc::clone(&released_handles),
            });
        let release_sink = RenderLeaseReleaseSink::new(release_tx);
        let release = video_release_for_tests(
            9,
            fallback_handle,
            Some(resource_provider),
            video_present_core::VideoFrameRendererSubmission::Submitted,
        );

        let outcome = release_sink.release_frame(release);

        assert!(matches!(
            outcome,
            VideoFrameReleaseOutcome::FallbackReleased {
                reason: VideoFrameReleaseFallbackReason::Backpressure
            }
        ));
        let still_queued_release = release_rx
            .try_recv()
            .expect("pre-filled release should stay in the full queue");
        assert_eq!(still_queued_release.resource_handle, queued_handle);
        assert!(release_rx.try_recv().is_err());
        assert_eq!(
            *released_handles
                .lock()
                .expect("fallback release log mutex should not be poisoned"),
            vec![fallback_handle]
        );
    }

    #[test]
    fn disconnected_release_queue_reports_disconnected_fallback_and_releases_provider() {
        let resource_handle = FrameResourceHandle(84);
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let (release_tx, release_rx) = bounded(1);
        let resource_provider =
            PresentFrameResourceProviderHandle::new(RecordingFallbackProvider {
                expected_handle: resource_handle,
                released_handles: Arc::clone(&released_handles),
            });
        let release_sink = RenderLeaseReleaseSink::new(release_tx);
        let release = video_release_for_tests(
            9,
            resource_handle,
            Some(resource_provider),
            video_present_core::VideoFrameRendererSubmission::NotSubmitted,
        );
        drop(release_rx);

        let outcome = release_sink.release_frame(release);

        assert!(matches!(
            outcome,
            VideoFrameReleaseOutcome::FallbackReleased {
                reason: VideoFrameReleaseFallbackReason::Disconnected
            }
        ));
        assert_eq!(
            *released_handles
                .lock()
                .expect("fallback release log mutex should not be poisoned"),
            vec![resource_handle]
        );
    }

    #[test]
    fn submitted_lease_drop_ack_records_renderer_usage() {
        let resource_handle = FrameResourceHandle(85);
        let lock_wait = Duration::from_micros(80);
        let (release_tx, release_rx) = bounded(1);
        let (sample_tx, _sample_rx) = bounded(1);
        let lease = lease_with_resource_provider_for_tests(
            9,
            resource_handle,
            release_tx,
            sample_tx,
            BusyResourceProvider {
                expected_handle: resource_handle,
                lock_wait,
            },
        );

        lease.mark_submitted_to_renderer();
        drop(lease);

        let release = release_rx
            .try_recv()
            .expect("dropping submitted lease should queue release ack");
        assert_eq!(release.resource_handle, resource_handle);
        assert!(release.submitted_to_renderer);
    }

    #[test]
    fn renderer_materializer_can_report_resource_lookup_diagnostics() {
        let resource_handle = FrameResourceHandle(82);
        let lock_wait = Duration::from_micros(125);
        let (release_tx, _release_rx) = bounded(1);
        let (sample_tx, sample_rx) = bounded(1);
        let lease = VideoFrameLease::new(
            VideoFrameLeaseConfig::new(
                9,
                decoded_frame_for_tests(resource_handle),
                Arc::new(RenderLeaseReleaseSink::new(release_tx)),
            )
            .with_diagnostics_sink(Arc::new(RenderLeaseDiagnosticsSink::new(sample_tx))),
        );
        lease.report_resource_lookup_sample(lock_wait, false);

        let sample = sample_rx
            .try_recv()
            .expect("renderer materializer lookup sample should be queued");
        assert_eq!(sample.wait, lock_wait);
        assert!(!sample.lookup_was_busy);
    }
}
