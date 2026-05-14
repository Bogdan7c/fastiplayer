use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TryRecvError, TrySendError, bounded};
use media_core::MediaTime;
use rustiplayer_config::PlayerDemuxConfig;
use tracing::{debug, warn};
use webm_demux::DemuxerOptions;

use crate::seek_controller::{PlaybackResumeIntent, SeekController};
use crate::{
    FrameCounters, MediaOpenRequest, MediaSource, PlayerCommand, PlayerError, PlayerErrorKind,
    PlayerEvent, PlayerResult, PlayerSession, PlayerSnapshot, PlayerTickConfig, PlayerTickContext,
    PlayerTickResult, ScrubCommitPolicy, SeekRequest, WgpuVideoBackendFactory,
};

/// Интервал worker tick по умолчанию: 60 Hz без привязки к UI redraw.
const DEFAULT_WORKER_TICK_INTERVAL: Duration = Duration::from_micros(16_667);

/// Ёмкость основной очереди команд без high-frequency scrub updates.
const COMMAND_CHANNEL_CAPACITY: usize = 128;

/// Ёмкость wake-очереди scrub updates; сами координаты лежат в latest-slot.
const SCRUB_WAKE_CHANNEL_CAPACITY: usize = 1;

/// Ёмкость latest snapshot stream; worker публикует только актуальное состояние.
const SNAPSHOT_CHANNEL_CAPACITY: usize = 1;

/// Ёмкость event stream; переполнение не должно блокировать playback thread.
const EVENT_CHANNEL_CAPACITY: usize = 512;

/// Ёмкость render request stream; render threadу нужен только один inflight request.
const RENDER_REQUEST_CHANNEL_CAPACITY: usize = 1;

/// Ёмкость release ack stream; защищает worker от бесконечного роста drop-ack очереди.
const RENDER_RELEASE_CHANNEL_CAPACITY: usize = 512;

/// Максимальное ожидание render-frame ответа, чтобы UI не зависал за worker-ом.
const RENDER_FRAME_REPLY_TIMEOUT: Duration = Duration::from_millis(2);

/// Короткий backpressure budget для Drop path render lease-а.
const RENDER_RELEASE_SEND_TIMEOUT: Duration = Duration::from_millis(2);

/// Интервал throttled diagnostics summary в debug logs.
const DIAGNOSTICS_SUMMARY_INTERVAL: Duration = Duration::from_secs(2);

/// Default throttle live preview seek-а, если worker создан без app config.
const DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL: Duration = Duration::from_millis(100);

/// Конфигурация playback worker.
#[derive(Debug, Clone, Copy)]
pub struct PlayerWorkerConfig {
    /// Период фонового playback tick.
    pub tick_interval: Duration,

    /// Scheduler/backpressure лимиты, передаваемые в `PlayerSession::tick`.
    pub tick_config: PlayerTickConfig,

    /// Минимальный интервал между live preview seek-ами во время scrub.
    pub live_scrub_preview_interval: Duration,

    /// Fail-safe настройки demuxer-а для media, которые worker открывает сам.
    pub demuxer_options: DemuxerOptions,

    /// Bounded queue/runtime limits decoder thread-а.
    pub decoder_thread_config: video_vaapi::VideoDecodeThreadConfig,
}

impl PlayerWorkerConfig {
    /// Создаёт worker config из runtime tick config приложения.
    #[must_use]
    pub fn new(tick_config: PlayerTickConfig) -> Self {
        Self {
            tick_interval: DEFAULT_WORKER_TICK_INTERVAL,
            tick_config,
            live_scrub_preview_interval: DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL,
            demuxer_options: DemuxerOptions::default(),
            decoder_thread_config: video_vaapi::VideoDecodeThreadConfig::default(),
        }
    }

    /// Создаёт worker config напрямую из validated app config.
    #[must_use]
    pub fn from_app_config(config: &rustiplayer_config::AppConfig) -> Self {
        Self {
            tick_interval: DEFAULT_WORKER_TICK_INTERVAL,
            tick_config: PlayerTickConfig::from(config),
            live_scrub_preview_interval: Duration::from_millis(config.player.seek.live_interval_ms),
            demuxer_options: demuxer_options_from_config(&config.player.demux),
            decoder_thread_config: decoder_thread_config_from_app_config(config),
        }
    }
}

impl Default for PlayerWorkerConfig {
    /// Возвращает production defaults без чтения внешней конфигурации.
    fn default() -> Self {
        Self::new(PlayerTickConfig::default())
    }
}

/// Конвертирует validated TOML config в runtime options demuxer-а.
fn demuxer_options_from_config(config: &PlayerDemuxConfig) -> DemuxerOptions {
    DemuxerOptions::from_max_consecutive_corrupted_packets(config.max_consecutive_corrupted_packets)
        .expect("validated AppConfig must provide positive demux corrupted packet limit")
}

/// Конвертирует validated TOML config в bounded decoder thread limits.
fn decoder_thread_config_from_app_config(
    config: &rustiplayer_config::AppConfig,
) -> video_vaapi::VideoDecodeThreadConfig {
    video_vaapi::VideoDecodeThreadConfig {
        packet_channel_frames: config.video.decoder_packet_channel_frames,
        frame_channel_frames: config.video.decoder_frame_channel_frames,
        decoder_ready_queue_frames: config.video.decoder_ready_queue_frames,
        decoder_surface_pool_frames: config.video.decoder_surface_pool_frames,
        zero_copy_surface_pool_slots: config.video.zero_copy_surface_pool_slots,
        ..video_vaapi::VideoDecodeThreadConfig::from_env()
    }
    .normalized()
}

/// Ошибка неблокирующей отправки команды в worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerWorkerSendError {
    /// Очередь заполнена, команда не была поставлена.
    Full,

    /// Worker уже завершился или receiver был закрыт.
    Disconnected,
}

impl fmt::Display for PlayerWorkerSendError {
    /// Печатает короткую причину для logs/UI diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("player worker command queue is full"),
            Self::Disconnected => formatter.write_str("player worker is disconnected"),
        }
    }
}

impl std::error::Error for PlayerWorkerSendError {}

impl<T> From<TrySendError<T>> for PlayerWorkerSendError {
    /// Теряет payload команды намеренно: вызывающему нужен только тип отказа.
    fn from(error: TrySendError<T>) -> Self {
        match error {
            TrySendError::Full(_) => Self::Full,
            TrySendError::Disconnected(_) => Self::Disconnected,
        }
    }
}

/// Ошибка join-а worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerWorkerJoinError;

impl fmt::Display for PlayerWorkerJoinError {
    /// Печатает стабильное сообщение для shutdown diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("player worker thread panicked during join")
    }
}

impl std::error::Error for PlayerWorkerJoinError {}

/// Событие worker boundary: core event или tick telemetry.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerWorkerEvent {
    /// Событие из `PlayerSession`.
    Player(PlayerEvent),

    /// Ошибка render bridge, полученная от shell/render thread.
    RenderError(PlayerRenderError),

    /// Итог одного фонового playback tick.
    Tick(PlayerTickResult),
}

/// Категория ошибки render bridge на границе shell -> worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerRenderErrorKind {
    /// Render thread не смог получить texture views по handle lease-а.
    MissingTextureViews,

    /// Renderer отказал decoded frame metadata или plane contract.
    UnsupportedFrameFormat,

    /// WGPU device/surface не смог завершить render frame.
    RenderDeviceLost,
}

/// Типизированная ошибка render bridge, которую shell отправляет в worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRenderError {
    /// Машиночитаемая категория render ошибки.
    pub kind: PlayerRenderErrorKind,

    /// Render generation кадра, если ошибка связана с конкретным lease.
    pub render_generation: Option<u64>,

    /// Opaque frame handle, если ошибка связана с конкретным decoded frame.
    pub frame_handle: Option<u64>,

    /// Сообщение для logs/UI без backend-specific Debug payload.
    pub message: String,
}

impl PlayerRenderError {
    /// Создаёт ошибку отсутствующих texture views для конкретного lease-а.
    #[must_use]
    pub fn missing_texture_views(lease: &PresentFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::MissingTextureViews,
            render_generation: Some(lease.render_generation),
            frame_handle: Some(lease.texture_handle().0),
            message: format!(
                "Render texture views are missing for {} frame handle {} in generation {}",
                lease.frame.format,
                lease.texture_handle().0,
                lease.render_generation
            ),
        }
    }

    /// Создаёт ошибку renderer boundary validation для конкретного lease-а.
    #[must_use]
    pub fn unsupported_frame_format(lease: &PresentFrameLease, message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(lease.render_generation),
            frame_handle: Some(lease.texture_handle().0),
            message: message.into(),
        }
    }

    /// Создаёт ошибку отказа render device/surface.
    #[must_use]
    pub fn render_device_lost(message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::RenderDeviceLost,
            render_generation: None,
            frame_handle: None,
            message: message.into(),
        }
    }

    /// Конвертирует typed render error в существующий player error snapshot contract.
    #[must_use]
    pub fn to_player_error(&self) -> PlayerError {
        let kind = match self.kind {
            PlayerRenderErrorKind::MissingTextureViews
            | PlayerRenderErrorKind::UnsupportedFrameFormat => {
                PlayerErrorKind::UnsupportedRenderFormat
            }
            PlayerRenderErrorKind::RenderDeviceLost => PlayerErrorKind::RenderDeviceLost,
        };

        PlayerError::new(
            kind,
            format!("Video render bridge failed: {}", self.message),
        )
    }
}

/// Cloneable sender для команд player worker.
#[derive(Clone)]
pub struct PlayerCommandSender {
    /// Основная очередь low-frequency команд.
    command_tx: Sender<WorkerCommand>,

    /// Явный latest-slot для high-frequency scrub updates.
    scrub_coalescer: Arc<ScrubUpdateCoalescer>,
}

impl PlayerCommandSender {
    /// Отправляет команду без блокировки render/UI thread.
    pub fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        match command {
            PlayerCommand::UpdateScrub(request) => self.try_send_scrub_update(request),
            other_command => self
                .command_tx
                .try_send(WorkerCommand::Player(other_command))
                .map_err(PlayerWorkerSendError::from),
        }
    }

    /// Отправляет latest scrub target без доступа sender-а к receiver side.
    fn try_send_scrub_update(&self, request: SeekRequest) -> Result<(), PlayerWorkerSendError> {
        self.scrub_coalescer.submit_latest(request)
    }
}

/// Явная latest-wins прослойка для частых scrub updates.
struct ScrubUpdateCoalescer {
    /// Последняя цель scrub-а; sender только заменяет значение, worker только забирает.
    latest_request: Mutex<Option<SeekRequest>>,

    /// Bounded wake-token: один pending token уже означает "latest_request надо проверить".
    wake_tx: Sender<()>,
}

impl ScrubUpdateCoalescer {
    /// Создаёт coalescer вокруг wake channel sender-а.
    fn new(wake_tx: Sender<()>) -> Self {
        Self {
            latest_request: Mutex::new(None),
            wake_tx,
        }
    }

    /// Записывает новую latest scrub-цель и будит worker одним bounded token-ом.
    fn submit_latest(&self, request: SeekRequest) -> Result<(), PlayerWorkerSendError> {
        *self.latest_request_guard() = Some(request);

        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => {
                *self.latest_request_guard() = None;
                Err(PlayerWorkerSendError::Disconnected)
            }
        }
    }

    /// Забирает последнюю scrub-цель; все промежуточные цели намеренно coalesced.
    fn take_latest(&self) -> Option<SeekRequest> {
        self.latest_request_guard().take()
    }

    /// Возвращает guard latest-slot-а и восстанавливается после poison без потери command path.
    fn latest_request_guard(&self) -> MutexGuard<'_, Option<SeekRequest>> {
        match self.latest_request.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("Scrub coalescer mutex was poisoned; recovering latest request slot");
                poisoned.into_inner()
            }
        }
    }
}

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

/// Drop-ack от render-side frame lease.
struct RenderLeaseRelease {
    /// Поколение render resources, где lease был создан.
    render_generation: u64,

    /// Texture handle, который больше не удерживает render/UI side.
    texture_handle: video_core::FrameTextureHandle,

    /// Provider исходного decoder texture pool для release после смены поколения.
    texture_provider: Option<video_vaapi::VideoTextureViewProvider>,

    /// Монотонный момент drop-а render lease на render/UI side.
    released_at: Instant,
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

/// Playback worker boundary, которым владеет app shell.
pub struct PlayerWorker {
    /// Cloneable command sender для UI и integration слоёв.
    command_sender: PlayerCommandSender,

    /// Канал latest snapshot от worker-а.
    snapshot_rx: Receiver<PlayerSnapshot>,

    /// Последний snapshot, прочитанный shell-ом.
    cached_snapshot: PlayerSnapshot,

    /// Канал событий и tick telemetry.
    event_rx: Receiver<PlayerWorkerEvent>,

    /// Канал запросов на текущий renderable frame.
    render_request_tx: Sender<RenderFrameRequest>,

    /// Аварийный shutdown signal, если command queue недоступна.
    shutdown_tx: Sender<()>,

    /// Join handle фонового потока.
    join_handle: Option<thread::JoinHandle<()>>,
}

impl PlayerWorker {
    /// Запускает worker thread и сразу публикует empty snapshot.
    pub fn spawn(config: PlayerWorkerConfig) -> PlayerResult<Self> {
        let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (scrub_wake_tx, scrub_wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (render_request_tx, render_request_rx) = bounded(RENDER_REQUEST_CHANNEL_CAPACITY);
        let (render_release_tx, render_release_rx) = bounded(RENDER_RELEASE_CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let scrub_coalescer = Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx));

        let command_sender = PlayerCommandSender {
            command_tx,
            scrub_coalescer: Arc::clone(&scrub_coalescer),
        };
        let snapshot_rx_for_worker = snapshot_rx.clone();

        let worker_started_at = Instant::now();
        let join_handle = thread::Builder::new()
            .name("player-worker".into())
            .spawn(move || {
                let runtime = PlayerWorkerRuntime {
                    session: PlayerSession::with_demuxer_options(config.demuxer_options),
                    seek_controller: SeekController::new(),
                    command_rx,
                    scrub_wake_rx,
                    scrub_coalescer,
                    snapshot_publisher: LatestSnapshotPublisher::new(
                        snapshot_tx,
                        snapshot_rx_for_worker,
                    ),
                    event_tx,
                    render_request_rx,
                    render_release_tx,
                    render_release_rx,
                    shutdown_rx,
                    config,
                    last_preview_scrub_seek_at: None,
                    last_preview_scrub_target: None,
                    deferred_preview_scrub_target: None,
                    last_tick_at: worker_started_at,
                    last_diagnostics_summary_at: worker_started_at,
                };
                runtime.run();
            })
            .map_err(|error| {
                PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("failed to spawn player worker: {error}"),
                )
            })?;

        Ok(Self {
            command_sender,
            snapshot_rx,
            cached_snapshot: PlayerSnapshot::empty(),
            event_rx,
            render_request_tx,
            shutdown_tx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает cloneable sender для long-lived UI callbacks.
    #[must_use]
    pub fn command_sender(&self) -> PlayerCommandSender {
        self.command_sender.clone()
    }

    /// Отправляет обычную player command без блокировки.
    pub fn try_send_command(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.command_sender.try_send(command)
    }

    /// Загружает локальный файл на worker thread.
    pub fn load_file(&self, path: &Path, autoplay: bool) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::LoadFile {
                path: path.to_path_buf(),
                autoplay,
            })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт уже открытый streaming demuxer во владение worker thread.
    pub fn load_demuxer(
        &self,
        label: String,
        demuxer: Box<dyn webm_demux::Demuxer + Send>,
        autoplay: bool,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::LoadDemuxer {
                label,
                demuxer,
                autoplay,
            })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Инициализирует video decoder pipeline внутри worker-owned session.
    pub fn init_video_pipeline(
        &self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::InitVideoPipeline {
                instance: instance.clone(),
                adapter: adapter.clone(),
                device: device.clone(),
                queue: queue.clone(),
            })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт capability report из shell/backend layer в worker.
    pub fn set_system_capabilities(
        &self,
        capabilities: SystemCapabilities,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::SetSystemCapabilities(capabilities))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт fatal render error в player state machine.
    pub fn mark_fatal_error(&self, error: PlayerError) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::MarkFatalError(error))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт typed render bridge error в worker-owned player session.
    pub fn report_render_error(
        &self,
        error: PlayerRenderError,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::RenderError(error))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Возвращает последний snapshot, не блокируя UI.
    #[must_use]
    pub fn latest_snapshot(&mut self, frame_counters: FrameCounters) -> PlayerSnapshot {
        for snapshot in self.snapshot_rx.try_iter() {
            self.cached_snapshot = snapshot;
        }

        let mut snapshot = self.cached_snapshot.clone();
        snapshot.frame_counters = frame_counters;
        snapshot
    }

    /// Забирает накопленные worker events без блокировки.
    #[must_use]
    pub fn drain_events(&self) -> Vec<PlayerWorkerEvent> {
        self.event_rx.try_iter().collect()
    }

    /// Пытается получить текущий кадр для renderer-а без раскрытия `PlayerSession`.
    #[must_use]
    pub fn try_acquire_present_frame(&self) -> Option<PlayerPresentFrame> {
        let (reply_tx, reply_rx) = bounded(1);
        let requested_at = Instant::now();
        let request = RenderFrameRequest {
            reply_tx,
            requested_at,
        };

        if self.render_request_tx.try_send(request).is_err() {
            self.report_render_acquire_timeout(requested_at.elapsed());
            return None;
        }

        match reply_rx.recv_timeout(RENDER_FRAME_REPLY_TIMEOUT) {
            Ok(frame) => frame,
            Err(_) => {
                self.report_render_acquire_timeout(requested_at.elapsed());
                None
            }
        }
    }

    /// Сообщает worker-у о render acquire timeout без блокировки render thread.
    fn report_render_acquire_timeout(&self, wait: Duration) {
        let _ = self
            .command_sender
            .command_tx
            .try_send(WorkerCommand::RenderAcquireTimeout { wait });
    }

    /// Запрашивает shutdown и ждёт завершения worker thread.
    pub fn shutdown(&mut self) -> Result<(), PlayerWorkerJoinError> {
        let _ = self.try_send_command(PlayerCommand::Shutdown);
        let _ = self.shutdown_tx.try_send(());

        let Some(join_handle) = self.join_handle.take() else {
            return Ok(());
        };

        join_handle.join().map_err(|_| PlayerWorkerJoinError)
    }
}

impl Drop for PlayerWorker {
    /// Drop path не должен оставлять фоновые player threads.
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            warn!(error = %error, "Player worker shutdown failed during drop");
        }
    }
}

/// Внутренние команды worker boundary.
enum WorkerCommand {
    /// Обычная команда public player contract.
    Player(PlayerCommand),

    /// Открыть локальный файл внутри worker-owned session.
    LoadFile {
        /// Путь к media-файлу.
        path: PathBuf,

        /// Нужно ли начать playback после успешного открытия.
        autoplay: bool,
    },

    /// Подключить уже созданный demuxer к worker-owned session.
    LoadDemuxer {
        /// User-facing label stream-а.
        label: String,

        /// Demuxer, который worker забирает во владение.
        demuxer: Box<dyn webm_demux::Demuxer + Send>,

        /// Нужно ли начать playback после успешного открытия.
        autoplay: bool,
    },

    /// Инициализация video backend с GPU handles shell-а.
    InitVideoPipeline {
        /// WGPU instance для zero-copy import path.
        instance: wgpu::Instance,

        /// WGPU adapter для backend capability matching.
        adapter: wgpu::Adapter,

        /// WGPU device для texture allocation.
        device: wgpu::Device,

        /// WGPU queue для texture upload/release callbacks.
        queue: wgpu::Queue,
    },

    /// Capability report из shell/backend layer.
    SetSystemCapabilities(SystemCapabilities),

    /// Fatal render boundary error.
    MarkFatalError(PlayerError),

    /// Typed render bridge error.
    RenderError(PlayerRenderError),

    /// Diagnostic event: render thread не получил present frame в bounded budget.
    RenderAcquireTimeout {
        /// Сколько render thread ждал reply.
        wait: Duration,
    },
}

/// Запрос текущего present frame от render thread.
struct RenderFrameRequest {
    /// Одноразовый reply channel.
    reply_tx: Sender<Option<PlayerPresentFrame>>,

    /// Монотонный момент создания render request.
    requested_at: Instant,
}

/// Publisher latest snapshot поверх bounded channel.
struct LatestSnapshotPublisher {
    /// Sender snapshot'ов в app shell.
    snapshot_tx: Sender<PlayerSnapshot>,

    /// Receiver clone нужен worker-у для политики `latest wins`.
    snapshot_rx_for_drain_latest: Receiver<PlayerSnapshot>,
}

impl LatestSnapshotPublisher {
    /// Создаёт publisher с private drain receiver clone.
    fn new(
        snapshot_tx: Sender<PlayerSnapshot>,
        snapshot_rx_for_drain_latest: Receiver<PlayerSnapshot>,
    ) -> Self {
        Self {
            snapshot_tx,
            snapshot_rx_for_drain_latest,
        }
    }

    /// Публикует latest snapshot, удаляя устаревший pending snapshot.
    fn publish(&self, snapshot: PlayerSnapshot) {
        drain_receiver_without_blocking(&self.snapshot_rx_for_drain_latest);
        if let Err(error) = self.snapshot_tx.try_send(snapshot) {
            match error {
                TrySendError::Full(_) => debug!("Latest snapshot channel is full"),
                TrySendError::Disconnected(_) => debug!("Snapshot receiver disconnected"),
            }
        }
    }
}

/// Runtime state, который живёт только на worker thread.
struct PlayerWorkerRuntime {
    /// Worker-owned player session и весь playback pipeline.
    session: PlayerSession,

    /// Seek/scrub skeleton controller.
    seek_controller: SeekController,

    /// Receiver основной очереди команд.
    command_rx: Receiver<WorkerCommand>,

    /// Receiver bounded wake-token-ов для latest scrub slot-а.
    scrub_wake_rx: Receiver<()>,

    /// Shared latest-slot, в котором sender явно заменяет scrub target.
    scrub_coalescer: Arc<ScrubUpdateCoalescer>,

    /// Latest snapshot publisher.
    snapshot_publisher: LatestSnapshotPublisher,

    /// Event stream sender.
    event_tx: Sender<PlayerWorkerEvent>,

    /// Receiver запросов текущего render frame.
    render_request_rx: Receiver<RenderFrameRequest>,

    /// Sender render lease release ack-ов для новых present frames.
    render_release_tx: Sender<RenderLeaseRelease>,

    /// Receiver render lease release ack-ов от UI/render side.
    render_release_rx: Receiver<RenderLeaseRelease>,

    /// Аварийный shutdown receiver.
    shutdown_rx: Receiver<()>,

    /// Runtime config worker-а.
    config: PlayerWorkerConfig,

    /// Последний момент, когда worker запускал live preview seek.
    last_preview_scrub_seek_at: Option<Instant>,

    /// Последняя scrub-цель, для которой preview seek уже был отправлен.
    last_preview_scrub_target: Option<SeekRequest>,

    /// Latest scrub-цель, ожидающая preview throttle window.
    deferred_preview_scrub_target: Option<SeekRequest>,

    /// Момент последнего playback tick.
    last_tick_at: Instant,

    /// Последний debug diagnostics summary.
    last_diagnostics_summary_at: Instant,
}

impl PlayerWorkerRuntime {
    /// Главный цикл worker thread.
    fn run(mut self) {
        self.publish_session_outputs();

        loop {
            self.drain_render_releases();

            if self.shutdown_rx.try_recv().is_ok() {
                self.handle_shutdown_request();
                break;
            }

            if let Some(command) = self.receive_next_command() {
                self.handle_worker_command(command);
                self.publish_session_outputs();
                if self.session.is_shutdown_requested() {
                    break;
                }
                continue;
            }

            self.handle_pending_render_requests();
            self.apply_latest_scrub_update();
            self.dispatch_due_preview_scrub_seek();
            self.run_tick_if_due();

            if self.session.is_shutdown_requested() {
                break;
            }

            if self.wait_for_worker_wakeup() {
                break;
            }
        }

        self.publish_session_outputs();
    }

    /// Ждёт ближайший command/render/shutdown wakeup вместо fixed idle polling.
    fn wait_for_worker_wakeup(&mut self) -> bool {
        match self.next_worker_wakeup_timeout() {
            Some(timeout) => self.wait_for_worker_wakeup_with_timeout(timeout),
            None => self.wait_for_worker_wakeup_until_event(),
        }
    }

    /// Блокируется до события или ближайшего tick/preview deadline-а.
    fn wait_for_worker_wakeup_with_timeout(&mut self, timeout: Duration) -> bool {
        crossbeam_channel::select! {
            recv(self.command_rx) -> command_result => {
                self.handle_command_wakeup(command_result)
            }
            recv(self.render_request_rx) -> request_result => {
                self.handle_render_request_wakeup(request_result)
            }
            recv(self.render_release_rx) -> release_result => {
                self.handle_render_release_wakeup(release_result)
            }
            recv(self.scrub_wake_rx) -> wake_result => {
                self.handle_scrub_wakeup(wake_result)
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                true
            }
            default(timeout) => false,
        }
    }

    /// Блокируется без timeout, когда playback idle и нет preview deadline-а.
    fn wait_for_worker_wakeup_until_event(&mut self) -> bool {
        crossbeam_channel::select! {
            recv(self.command_rx) -> command_result => {
                self.handle_command_wakeup(command_result)
            }
            recv(self.render_request_rx) -> request_result => {
                self.handle_render_request_wakeup(request_result)
            }
            recv(self.render_release_rx) -> release_result => {
                self.handle_render_release_wakeup(release_result)
            }
            recv(self.scrub_wake_rx) -> wake_result => {
                self.handle_scrub_wakeup(wake_result)
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                true
            }
        }
    }

    /// Вычисляет ближайший deadline, который требует самостоятельного wakeup-а worker-а.
    fn next_worker_wakeup_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut next_timeout = self.next_tick_timeout(now);

        if let Some(preview_timeout) = self.next_preview_scrub_timeout(now) {
            next_timeout = Some(match next_timeout {
                Some(tick_timeout) => tick_timeout.min(preview_timeout),
                None => preview_timeout,
            });
        }

        next_timeout
    }

    /// Возвращает время до следующего playback tick-а только для активной state machine.
    fn next_tick_timeout(&self, now: Instant) -> Option<Duration> {
        if !self.session.playback_state().is_playback_active() {
            return None;
        }

        let elapsed_since_tick = now.saturating_duration_since(self.last_tick_at);
        Some(self.config.tick_interval.saturating_sub(elapsed_since_tick))
    }

    /// Возвращает время до throttled preview seek-а во время interactive scrub.
    fn next_preview_scrub_timeout(&self, now: Instant) -> Option<Duration> {
        if !self.seek_controller.is_scrubbing() || self.deferred_preview_scrub_target.is_none() {
            return None;
        }

        let Some(last_seek_at) = self.last_preview_scrub_seek_at else {
            return Some(Duration::ZERO);
        };
        let elapsed_since_preview = now.saturating_duration_since(last_seek_at);
        Some(
            self.config
                .live_scrub_preview_interval
                .saturating_sub(elapsed_since_preview),
        )
    }

    /// Обрабатывает wakeup от основной очереди команд.
    fn handle_command_wakeup(
        &mut self,
        command_result: Result<WorkerCommand, crossbeam_channel::RecvError>,
    ) -> bool {
        match command_result {
            Ok(command) => {
                self.handle_worker_command(command);
                self.publish_session_outputs();
                self.session.is_shutdown_requested()
            }
            Err(_) => {
                self.handle_shutdown_request();
                true
            }
        }
    }

    /// Обрабатывает wakeup от render request queue.
    fn handle_render_request_wakeup(
        &mut self,
        request_result: Result<RenderFrameRequest, crossbeam_channel::RecvError>,
    ) -> bool {
        if let Ok(request) = request_result {
            self.handle_render_request(request);
        }
        false
    }

    /// Обрабатывает wakeup от bounded release ack queue.
    fn handle_render_release_wakeup(
        &mut self,
        release_result: Result<RenderLeaseRelease, crossbeam_channel::RecvError>,
    ) -> bool {
        if let Ok(release) = release_result {
            self.session
                .record_release_ack_latency(release.released_at.elapsed());
            self.session.release_render_lease_with_provider(
                release.render_generation,
                release.texture_handle,
                release.texture_provider.as_ref(),
            );
        }
        false
    }

    /// Обрабатывает wake-token scrub coalescer-а.
    fn handle_scrub_wakeup(
        &mut self,
        wake_result: Result<(), crossbeam_channel::RecvError>,
    ) -> bool {
        if wake_result.is_ok() {
            self.apply_latest_scrub_update();
            self.publish_session_outputs();
        }

        self.session.is_shutdown_requested()
    }

    /// Забирает команду без блокировки, чтобы render/scrub/tick не starvation-ились.
    fn receive_next_command(&self) -> Option<WorkerCommand> {
        match self.command_rx.try_recv() {
            Ok(command) => Some(command),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(WorkerCommand::Player(PlayerCommand::Shutdown)),
        }
    }

    /// Обрабатывает одну worker command.
    fn handle_worker_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Player(player_command) => self.handle_player_command(player_command),
            WorkerCommand::LoadFile { path, autoplay } => {
                self.interrupt_scrub_for_external_boundary();
                self.session.load_file_with_autoplay(&path, autoplay);
            }
            WorkerCommand::LoadDemuxer {
                label,
                demuxer,
                autoplay,
            } => {
                self.interrupt_scrub_for_external_boundary();
                self.session
                    .load_demuxer_with_autoplay(label, demuxer, autoplay);
            }
            WorkerCommand::InitVideoPipeline {
                instance,
                adapter,
                device,
                queue,
            } => {
                let backend_factory = WgpuVideoBackendFactory::new_with_decoder_config(
                    &instance,
                    &adapter,
                    &device,
                    &queue,
                    self.config.decoder_thread_config,
                );
                self.session.init_video_pipeline(&backend_factory);
            }
            WorkerCommand::SetSystemCapabilities(capabilities) => {
                self.session.set_system_capabilities(capabilities);
            }
            WorkerCommand::MarkFatalError(error) => {
                self.session.mark_fatal_error(error);
            }
            WorkerCommand::RenderError(error) => {
                self.handle_render_error(error);
            }
            WorkerCommand::RenderAcquireTimeout { wait } => {
                self.session.record_render_acquire_timeout(wait);
            }
        }
    }

    /// Сохраняет typed render error в snapshot и публикует worker event.
    fn handle_render_error(&mut self, error: PlayerRenderError) {
        self.publish_worker_event(PlayerWorkerEvent::RenderError(error.clone()));
        self.session.mark_fatal_error(error.to_player_error());
    }

    /// Применяет public command с worker-level seek/scrub priority policy.
    fn handle_player_command(&mut self, command: PlayerCommand) {
        if self.seek_controller.consume_resume_intent_command(&command) {
            return;
        }

        match command {
            PlayerCommand::OpenMedia(request) => self.handle_open_media_request(request),
            PlayerCommand::Stop => self.handle_stop_command(),
            PlayerCommand::Shutdown => self.handle_shutdown_request(),
            PlayerCommand::BeginScrub => self.handle_begin_scrub_command(),
            PlayerCommand::UpdateScrub(request) => self.apply_scrub_update(request),
            PlayerCommand::PreviewScrub(request) => {
                self.dispatch_player_command(PlayerCommand::PreviewScrub(request));
            }
            PlayerCommand::EndScrub { policy } => self.handle_end_scrub_command(policy),
            PlayerCommand::Seek(request) => self.handle_seek_command(request),
            other_command => self.dispatch_player_command(other_command),
        }
    }

    /// Открывает media request; local file грузится полностью внутри worker thread.
    fn handle_open_media_request(&mut self, request: MediaOpenRequest) {
        self.interrupt_scrub_for_external_boundary();

        match request.source.clone() {
            MediaSource::LocalFile(path) => {
                self.session
                    .load_file_with_autoplay(&path, request.autoplay);
            }
            MediaSource::Url(_) | MediaSource::ExternalLabel(_) => {
                self.dispatch_player_command(PlayerCommand::OpenMedia(request));
            }
        }
    }

    /// Stop во время scrub сначала просит pause + seek zero, затем сбрасывает media.
    fn handle_stop_command(&mut self) {
        if self.seek_controller.is_scrubbing() {
            self.seek_controller.interrupt_scrub();
            self.dispatch_player_command(PlayerCommand::Pause);
            self.dispatch_player_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::ZERO,
            )));
        }

        self.dispatch_player_command(PlayerCommand::Stop);
    }

    /// Shutdown прерывает scrub и закрывает session.
    fn handle_shutdown_request(&mut self) {
        self.interrupt_scrub_for_external_boundary();
        self.dispatch_player_command(PlayerCommand::Shutdown);
    }

    /// BeginScrub сохраняет resume intent и временно ставит playback на паузу.
    fn handle_begin_scrub_command(&mut self) {
        self.reset_preview_scrub_state();
        self.seek_controller
            .begin_scrub(self.session.playback_state());
        self.dispatch_player_command(PlayerCommand::BeginScrub);
        self.dispatch_player_command(PlayerCommand::Pause);
    }

    /// EndScrub коммитит latest target и применяет сохранённый resume intent.
    fn handle_end_scrub_command(&mut self, policy: ScrubCommitPolicy) {
        self.apply_latest_scrub_update();
        self.deferred_preview_scrub_target = None;

        if !self.seek_controller.is_scrubbing() {
            self.dispatch_player_command(PlayerCommand::EndScrub { policy });
            self.reset_preview_scrub_state();
            return;
        }

        let resume_intent = self.seek_controller.finish_scrub();
        self.dispatch_player_command(PlayerCommand::EndScrub { policy });
        self.apply_end_scrub_resume_intent(resume_intent);
        self.reset_preview_scrub_state();
    }

    /// Применяет сохранённый worker resume intent к финальному seek transaction-у.
    fn apply_end_scrub_resume_intent(&mut self, resume_intent: PlaybackResumeIntent) {
        if self
            .session
            .override_active_seek_resume_intent(resume_intent)
        {
            return;
        }

        match resume_intent {
            PlaybackResumeIntent::Pause => self.dispatch_player_command(PlayerCommand::Pause),
            PlaybackResumeIntent::Play => self.dispatch_player_command(PlayerCommand::Play),
        }
    }

    /// Внешний Seek игнорируется, пока активен scrub.
    fn handle_seek_command(&mut self, request: SeekRequest) {
        if self.seek_controller.should_ignore_external_seek() {
            debug!("External seek ignored during active scrub");
            return;
        }

        self.dispatch_player_command(PlayerCommand::Seek(request));
    }

    /// Применяет один scrub update после coalescing.
    fn apply_scrub_update(&mut self, request: SeekRequest) {
        if !self.seek_controller.accept_scrub_update(request) {
            return;
        }

        self.dispatch_player_command(PlayerCommand::UpdateScrub(request));
        self.queue_preview_scrub_seek(request);
        self.dispatch_due_preview_scrub_seek();
    }

    /// Забирает latest scrub update из bounded канала и применяет только последнюю цель.
    fn apply_latest_scrub_update(&mut self) {
        drain_receiver_without_blocking(&self.scrub_wake_rx);

        if let Some(request) = self.scrub_coalescer.take_latest() {
            self.apply_scrub_update(request);
            self.publish_session_outputs();
        }
    }

    /// Прерывает scrub для Open/Shutdown/load boundary команд.
    fn interrupt_scrub_for_external_boundary(&mut self) {
        if self.seek_controller.is_scrubbing() {
            self.seek_controller.interrupt_scrub();
        }
        self.reset_preview_scrub_state();
    }

    /// Сбрасывает worker-only состояние preview seek-а для новой scrub операции.
    fn reset_preview_scrub_state(&mut self) {
        self.last_preview_scrub_seek_at = None;
        self.last_preview_scrub_target = None;
        self.deferred_preview_scrub_target = None;
    }

    /// Запоминает latest target, для которого нужен live preview seek.
    fn queue_preview_scrub_seek(&mut self, request: SeekRequest) {
        if self.last_preview_scrub_target == Some(request) {
            return;
        }

        self.deferred_preview_scrub_target = Some(request);
    }

    /// Отправляет preview seek, когда прошёл throttle interval live scrub-а.
    fn dispatch_due_preview_scrub_seek(&mut self) {
        if !self.seek_controller.is_scrubbing() {
            self.deferred_preview_scrub_target = None;
            return;
        }

        let Some(request) = self.deferred_preview_scrub_target else {
            return;
        };

        let now = Instant::now();
        if !self.preview_scrub_interval_elapsed(now) {
            return;
        }

        self.deferred_preview_scrub_target = None;
        self.last_preview_scrub_target = Some(request);
        self.last_preview_scrub_seek_at = Some(now);
        self.dispatch_player_command(PlayerCommand::PreviewScrub(request));
    }

    /// Проверяет throttle window для live preview seek-а.
    fn preview_scrub_interval_elapsed(&self, now: Instant) -> bool {
        match self.last_preview_scrub_seek_at {
            Some(last_seek_at) => {
                now.saturating_duration_since(last_seek_at)
                    >= self.config.live_scrub_preview_interval
            }
            None => true,
        }
    }

    /// Безопасно вызывает `PlayerSession::dispatch_command` и сохраняет ошибку в session.
    fn dispatch_player_command(&mut self, command: PlayerCommand) {
        if let Err(error) = self.session.dispatch_command(command) {
            warn!(error = %error, "Player worker command failed");
            self.session.mark_fatal_error(error);
        }
    }

    /// Обслуживает все pending render-frame requests без блокировки.
    fn handle_pending_render_requests(&mut self) {
        while let Ok(request) = self.render_request_rx.try_recv() {
            self.handle_render_request(request);
        }
    }

    /// Возвращает текущий present frame и ставит минимальный lease до Drop на UI side.
    fn handle_render_request(&mut self, request: RenderFrameRequest) {
        self.drain_render_releases();
        self.session
            .record_render_acquire_wait(request.requested_at.elapsed());

        let Some(present_frame) = self.build_present_frame() else {
            let _ = request.reply_tx.try_send(None);
            return;
        };

        if let Err(error) = request.reply_tx.try_send(Some(present_frame)) {
            match error {
                TrySendError::Full(_) => debug!("Render frame reply channel is full"),
                TrySendError::Disconnected(_) => debug!("Render frame request receiver dropped"),
            }
        }
    }

    /// Собирает renderable frame без передачи `PlayerSession` наружу.
    fn build_present_frame(&mut self) -> Option<PlayerPresentFrame> {
        let leased_frame = self.session.lease_present_video_frame()?;

        Some(PlayerPresentFrame {
            render_generation: leased_frame.render_generation,
            frame: leased_frame.frame.clone(),
            stale: leased_frame.stale,
            texture_provider: Some(leased_frame.texture_provider.clone()),
            _drop_ack: Arc::new(PresentFrameDropAck {
                render_generation: leased_frame.render_generation,
                texture_handle: leased_frame.frame.texture_handle,
                texture_provider: Some(leased_frame.texture_provider),
                release_tx: self.render_release_tx.clone(),
            }),
        })
    }

    /// Снимает все render leases, которые UI/render side уже dropped.
    fn drain_render_releases(&mut self) {
        while let Ok(release) = self.render_release_rx.try_recv() {
            self.session
                .record_release_ack_latency(release.released_at.elapsed());
            self.session.release_render_lease_with_provider(
                release.render_generation,
                release.texture_handle,
                release.texture_provider.as_ref(),
            );
        }
    }

    /// Выполняет playback tick, если пришло время.
    fn run_tick_if_due(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_tick_at) < self.config.tick_interval {
            return;
        }

        let tick_result = self
            .session
            .tick(PlayerTickContext::with_config(now, self.config.tick_config));
        self.last_tick_at = now;
        self.publish_tick_result(tick_result);
        self.log_diagnostics_summary_if_due(now);
        self.publish_session_outputs();
    }

    /// Пишет короткую diagnostics summary только при включённом debug tracing.
    fn log_diagnostics_summary_if_due(&mut self, now: Instant) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }

        if now.saturating_duration_since(self.last_diagnostics_summary_at)
            < DIAGNOSTICS_SUMMARY_INTERVAL
        {
            return;
        }

        let summary = self.session.diagnostics_log_summary();
        if !summary.has_activity() {
            return;
        }

        self.last_diagnostics_summary_at = now;
        let worst_stage = summary
            .worst_stage
            .map(|stage| stage.metric_name())
            .unwrap_or("none");
        let worst_latency_ms = summary
            .worst_latency
            .map(|latency| latency.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        debug!(
            drops = summary.drops_total,
            pauses = summary.pauses_total,
            memory_path = ?summary.zero_copy_memory_path,
            worst_stage,
            worst_latency_ms,
            pending_video_packets = summary.queues.pending_video_packets,
            present_queue_depth = summary.queues.present_queue_depth,
            active_render_leases = summary.queues.active_render_leases,
            "Playback diagnostics summary"
        );
    }

    /// Публикует latest snapshot и накопленные session events.
    fn publish_session_outputs(&mut self) {
        let snapshot = self
            .session
            .snapshot_with_frame_counters(FrameCounters::default());
        self.snapshot_publisher.publish(snapshot);

        for event in self.session.take_events() {
            self.publish_worker_event(PlayerWorkerEvent::Player(event));
        }
    }

    /// Публикует tick telemetry без блокировки worker-а.
    fn publish_tick_result(&self, tick_result: PlayerTickResult) {
        self.publish_worker_event(PlayerWorkerEvent::Tick(tick_result));
    }

    /// Публикует worker event, сбрасывая событие при переполнении receiver-а.
    fn publish_worker_event(&self, event: PlayerWorkerEvent) {
        if let Err(error) = self.event_tx.try_send(event) {
            match error {
                TrySendError::Full(_) => debug!("Player worker event channel is full"),
                TrySendError::Disconnected(_) => debug!("Player worker event receiver dropped"),
            }
        }
    }
}

/// Опустошает receiver без ожидания; используется для latest/coalescing каналов.
fn drain_receiver_without_blocking<T>(receiver: &Receiver<T>) {
    while receiver.try_recv().is_ok() {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use crossbeam_channel::unbounded;
    use video_core::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle};

    use super::*;
    use crate::{MediaSource, PlaybackState, SeekTarget};

    fn worker_config_for_tests() -> PlayerWorkerConfig {
        PlayerWorkerConfig {
            tick_interval: Duration::from_millis(2),
            tick_config: PlayerTickConfig::default(),
            live_scrub_preview_interval: DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL,
            demuxer_options: DemuxerOptions::default(),
            decoder_thread_config: video_vaapi::VideoDecodeThreadConfig::default(),
        }
    }

    fn seek_to_millis(milliseconds: u64) -> SeekRequest {
        SeekRequest::absolute(MediaTime::from_millis(milliseconds))
    }

    fn wait_for_snapshot(
        worker: &mut PlayerWorker,
        predicate: impl Fn(&PlayerSnapshot) -> bool,
    ) -> PlayerSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);

        while Instant::now() < deadline {
            let snapshot = worker.latest_snapshot(FrameCounters::default());
            if predicate(&snapshot) {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        }

        panic!("timed out waiting for worker snapshot");
    }

    fn drain_events_until(
        worker: &PlayerWorker,
        predicate: impl Fn(&[PlayerWorkerEvent]) -> bool,
    ) -> Vec<PlayerWorkerEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();

        while Instant::now() < deadline {
            events.extend(worker.drain_events());
            if predicate(&events) {
                return events;
            }
            thread::sleep(Duration::from_millis(2));
        }

        events
    }

    fn runtime_for_tests(last_tick_at: Instant) -> PlayerWorkerRuntime {
        let (_command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (scrub_wake_tx, scrub_wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (_render_request_tx, render_request_rx) = bounded(RENDER_REQUEST_CHANNEL_CAPACITY);
        let (render_release_tx, render_release_rx) = bounded(RENDER_RELEASE_CHANNEL_CAPACITY);
        let (_shutdown_tx, shutdown_rx) = bounded(1);
        let config = worker_config_for_tests();
        let scrub_coalescer = Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx));

        PlayerWorkerRuntime {
            session: PlayerSession::with_demuxer_options(config.demuxer_options),
            seek_controller: SeekController::new(),
            command_rx,
            scrub_wake_rx,
            scrub_coalescer,
            snapshot_publisher: LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx),
            event_tx,
            render_request_rx,
            render_release_tx,
            render_release_rx,
            shutdown_rx,
            config,
            last_preview_scrub_seek_at: None,
            last_preview_scrub_target: None,
            deferred_preview_scrub_target: None,
            last_tick_at,
            last_diagnostics_summary_at: last_tick_at,
        }
    }

    fn decoded_frame_for_tests(texture_handle: FrameTextureHandle) -> DecodedFrame {
        DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    fn present_frame_lease_for_tests(
        render_generation: u64,
        texture_handle: FrameTextureHandle,
        stale: bool,
        release_tx: Sender<RenderLeaseRelease>,
    ) -> PresentFrameLease {
        PresentFrameLease {
            render_generation,
            frame: decoded_frame_for_tests(texture_handle),
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

    #[test]
    fn worker_starts_accepts_commands_publishes_snapshot_and_shutdowns() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

        worker.try_send_command(PlayerCommand::Play).unwrap();
        let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
            snapshot.playback_state == PlaybackState::Playing
        });

        assert_eq!(snapshot.playback_state, PlaybackState::Playing);
        worker.shutdown().unwrap();
    }

    #[test]
    fn command_ordering_for_play_pause_stop_open_shutdown_is_preserved() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
        let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);

        worker.try_send_command(PlayerCommand::Play).unwrap();
        worker.try_send_command(PlayerCommand::Pause).unwrap();
        worker.try_send_command(PlayerCommand::Stop).unwrap();
        worker
            .try_send_command(PlayerCommand::OpenMedia(request.clone()))
            .unwrap();
        worker.try_send_command(PlayerCommand::Shutdown).unwrap();

        let events = drain_events_until(&worker, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    PlayerWorkerEvent::Player(PlayerEvent::ShutdownRequested)
                )
            })
        });
        let player_events = events
            .iter()
            .filter_map(|event| match event {
                PlayerWorkerEvent::Player(event) => Some(event),
                PlayerWorkerEvent::RenderError(_) => None,
                PlayerWorkerEvent::Tick(_) => None,
            })
            .collect::<Vec<_>>();

        let playing_index = player_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
                )
            })
            .expect("missing Playing event");
        let paused_index = player_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)
                )
            })
            .expect("missing Paused event");
        let open_index = player_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    PlayerEvent::MediaOpenRequested(open_request) if *open_request == request
                )
            })
            .expect("missing OpenMedia event");
        let shutdown_index = player_events
            .iter()
            .position(|event| matches!(event, PlayerEvent::ShutdownRequested))
            .expect("missing Shutdown event");

        assert!(playing_index < paused_index);
        assert!(paused_index < open_index);
        assert!(open_index < shutdown_index);
        worker.shutdown().unwrap();
    }

    #[test]
    fn update_scrub_coalesces_to_latest_target() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

        worker.try_send_command(PlayerCommand::BeginScrub).unwrap();
        for milliseconds in [100, 250, 500, 750] {
            worker
                .try_send_command(PlayerCommand::UpdateScrub(seek_to_millis(milliseconds)))
                .unwrap();
        }
        worker
            .try_send_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
            matches!(
                snapshot.last_error.as_ref().map(|error| &error.kind),
                Some(PlayerErrorKind::SeekUnavailable)
            )
        });
        let events = drain_events_until(&worker, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                        if request.target == SeekTarget::Absolute(MediaTime::from_millis(750))
                )
            })
        });

        assert_eq!(snapshot.current_position, Duration::ZERO);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                    if request.target == SeekTarget::Absolute(MediaTime::from_millis(750))
            )
        }));
        worker.shutdown().unwrap();
    }

    #[test]
    fn external_seek_is_ignored_during_active_scrub() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

        worker.try_send_command(PlayerCommand::BeginScrub).unwrap();
        worker
            .try_send_command(PlayerCommand::UpdateScrub(seek_to_millis(500)))
            .unwrap();
        worker
            .try_send_command(PlayerCommand::Seek(seek_to_millis(900)))
            .unwrap();
        worker
            .try_send_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
            matches!(
                snapshot.last_error.as_ref().map(|error| &error.kind),
                Some(PlayerErrorKind::SeekUnavailable)
            )
        });
        let events = drain_events_until(&worker, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                        if request.target == SeekTarget::Absolute(MediaTime::from_millis(500))
                )
            })
        });

        assert_eq!(snapshot.current_position, Duration::ZERO);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                    if request.target == SeekTarget::Absolute(MediaTime::from_millis(500))
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                    if request.target == SeekTarget::Absolute(MediaTime::from_millis(900))
            )
        }));
        worker.shutdown().unwrap();
    }

    #[test]
    fn stop_interrupts_scrub_and_requests_pause_then_seek_zero() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

        worker.try_send_command(PlayerCommand::Play).unwrap();
        worker.try_send_command(PlayerCommand::BeginScrub).unwrap();
        worker
            .try_send_command(PlayerCommand::UpdateScrub(seek_to_millis(900)))
            .unwrap();
        worker.try_send_command(PlayerCommand::Stop).unwrap();

        let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
            snapshot.playback_state == PlaybackState::Stopped
        });
        let events = drain_events_until(&worker, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                        if request.target == SeekTarget::Absolute(MediaTime::ZERO)
                )
            })
        });

        assert_eq!(snapshot.playback_state, PlaybackState::Stopped);
        assert_eq!(snapshot.current_position, Duration::ZERO);
        assert!(!snapshot.timeline.scrubbing);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(PlayerEvent::SeekRequested(request))
                    if request.target == SeekTarget::Absolute(MediaTime::ZERO)
            )
        }));
        worker.shutdown().unwrap();
    }

    #[test]
    fn command_sender_returns_disconnected_after_worker_shutdown() {
        let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
        let command_sender = worker.command_sender();

        worker.shutdown().unwrap();
        let result = command_sender.try_send(PlayerCommand::Play);

        assert_eq!(result, Err(PlayerWorkerSendError::Disconnected));
    }

    #[test]
    fn scrub_coalescer_keeps_latest_target_without_sender_receiver_drain() {
        let (wake_tx, wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);

        coalescer.submit_latest(seek_to_millis(100)).unwrap();
        coalescer.submit_latest(seek_to_millis(250)).unwrap();
        coalescer.submit_latest(seek_to_millis(900)).unwrap();

        assert_eq!(wake_rx.len(), 1);
        assert_eq!(coalescer.take_latest(), Some(seek_to_millis(900)));
        assert_eq!(coalescer.take_latest(), None);
    }

    #[test]
    fn scrub_coalescer_reports_disconnected_without_keeping_stale_target() {
        let (wake_tx, wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);
        drop(wake_rx);

        let send_result = coalescer.submit_latest(seek_to_millis(100));

        assert_eq!(send_result, Err(PlayerWorkerSendError::Disconnected));
        assert_eq!(coalescer.take_latest(), None);
    }

    #[test]
    fn idle_worker_has_no_periodic_wakeup_timeout() {
        let runtime = runtime_for_tests(Instant::now());

        assert_eq!(runtime.next_worker_wakeup_timeout(), None);
    }

    #[test]
    fn active_worker_uses_tick_interval_as_wakeup_timeout() {
        let mut runtime = runtime_for_tests(Instant::now());

        runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));

        assert!(runtime.next_worker_wakeup_timeout().is_some());
    }

    #[test]
    fn render_release_ack_is_drained_before_reply() {
        let mut runtime = runtime_for_tests(Instant::now());
        runtime
            .session
            .register_render_lease(0, video_core::FrameTextureHandle(7));
        runtime
            .render_release_tx
            .try_send(RenderLeaseRelease {
                render_generation: 0,
                texture_handle: video_core::FrameTextureHandle(7),
                texture_provider: None,
                released_at: Instant::now(),
            })
            .unwrap();
        let (reply_tx, reply_rx) = bounded(1);
        let requested_at = Instant::now();

        runtime.handle_render_request(RenderFrameRequest {
            reply_tx,
            requested_at,
        });

        assert_eq!(runtime.session.render_lease_count(), 0);
        assert!(reply_rx.try_recv().unwrap().is_none());
    }

    #[test]
    fn tick_runs_while_render_lease_is_active() {
        let mut runtime = runtime_for_tests(Instant::now() - Duration::from_millis(10));
        runtime
            .session
            .register_render_lease(0, video_core::FrameTextureHandle(11));
        let previous_tick_at = runtime.last_tick_at;

        runtime.run_tick_if_due();

        assert!(runtime.last_tick_at > previous_tick_at);
    }

    #[test]
    fn present_frame_lease_drop_releases_frame_exactly_once() {
        let (release_tx, release_rx) = unbounded();
        let lease =
            present_frame_lease_for_tests(2, FrameTextureHandle(12), false, release_tx.clone());
        let lease_clone = lease.clone();

        drop(lease);
        assert!(release_rx.try_recv().is_err());

        drop(lease_clone);
        let release = release_rx.try_recv().unwrap();

        assert_eq!(release.render_generation, 2);
        assert_eq!(release.texture_handle, FrameTextureHandle(12));
        assert!(release_rx.try_recv().is_err());
    }

    #[test]
    fn present_frame_lease_drop_times_out_when_release_queue_stays_full() {
        let (release_tx, release_rx) = bounded(1);
        release_tx
            .try_send(RenderLeaseRelease {
                render_generation: 1,
                texture_handle: FrameTextureHandle(1),
                texture_provider: None,
                released_at: Instant::now(),
            })
            .unwrap();
        let lease = present_frame_lease_for_tests(2, FrameTextureHandle(12), false, release_tx);
        let drop_started_at = Instant::now();

        drop(lease);

        assert!(drop_started_at.elapsed() < Duration::from_secs(1));
        assert_eq!(release_rx.len(), 1);
        let queued_release = release_rx.try_recv().unwrap();
        assert_eq!(queued_release.render_generation, 1);
        assert_eq!(queued_release.texture_handle, FrameTextureHandle(1));
    }

    #[test]
    fn leased_frame_release_is_deferred_until_renderer_drops_lease() {
        let mut runtime = runtime_for_tests(Instant::now());
        let texture_handle = FrameTextureHandle(21);

        assert!(runtime.session.register_render_lease(0, texture_handle));
        runtime.session.release_video_texture(texture_handle);

        assert_eq!(runtime.session.render_lease_count(), 1);
        assert!(
            runtime
                .session
                .has_deferred_video_texture_release(texture_handle)
        );

        runtime.session.release_render_lease(0, texture_handle);

        assert_eq!(runtime.session.render_lease_count(), 0);
        assert_eq!(runtime.session.deferred_video_texture_release_count(), 0);
    }

    #[test]
    fn new_generation_makes_old_lease_stale_without_dropping_it() {
        let (release_tx, release_rx) = unbounded();
        let lease = present_frame_lease_for_tests(4, FrameTextureHandle(31), false, release_tx);

        assert!(lease.stale_for_generation(5));
        assert!(release_rx.try_recv().is_err());

        drop(lease);

        let release = release_rx.try_recv().unwrap();
        assert_eq!(release.render_generation, 4);
        assert_eq!(release.texture_handle, FrameTextureHandle(31));
    }

    #[test]
    fn render_error_command_updates_player_error_snapshot() {
        let mut runtime = runtime_for_tests(Instant::now());
        let render_error = PlayerRenderError {
            kind: PlayerRenderErrorKind::MissingTextureViews,
            render_generation: Some(6),
            frame_handle: Some(42),
            message: "missing Y/UV views for test frame".into(),
        };

        runtime.handle_worker_command(WorkerCommand::RenderError(render_error));

        let snapshot_error = runtime.session.snapshot().last_error.as_ref().unwrap();
        assert_eq!(
            snapshot_error.kind,
            PlayerErrorKind::UnsupportedRenderFormat
        );
        assert!(
            snapshot_error
                .message
                .contains("missing Y/UV views for test frame")
        );
        assert_eq!(runtime.session.playback_state(), PlaybackState::Failed);
        assert!(
            runtime
                .session
                .take_events()
                .iter()
                .any(|event| matches!(event, PlayerEvent::FatalError(error)
                    if error.kind == PlayerErrorKind::UnsupportedRenderFormat))
        );
    }
}
