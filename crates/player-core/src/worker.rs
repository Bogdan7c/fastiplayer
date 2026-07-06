use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use frame_server_core::{
    FrameServerConfig as RuntimeFrameServerConfig, LiveScrubDecodeMode, ValidatedFrameServerConfig,
};
use rustiplayer_config::{
    FrameServerConfig as PersistedFrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
};
use tracing::{debug, warn};
use video_core::{
    VideoDecoderActivityEpoch, VideoDecoderActivitySnapshot, VideoDecoderActivityUnavailableReason,
    VideoDecoderActivityWaitOutcome,
};
use video_present_core::VideoFrameLease;

use crate::audio_boundary::{missing_audio_decoder_factory, missing_audio_output_factory};
use crate::pipeline::VideoDecoderActivityStatus;
#[cfg(test)]
use crate::render_lease_bridge::{
    LatestPresentFrameAcquire, LatestPresentFrameHandoff, RenderAcquireSample, RenderLeaseRelease,
    RenderLeaseReleaseSink, RenderResourcePreviousFrameReuseSample, RenderTimingSample,
};
use crate::render_lease_bridge::{RenderLeaseBridge, RenderLeaseBridgeClient};
use crate::runtime_settings::{validate_runtime_default_volume, validate_runtime_tick_config};
use crate::worker_scheduler::{PlannedWorkerWakeup, WorkerScheduler, WorkerWakeupDeadline};
use crate::{
    ActiveSeekDiagnosticsSnapshot, AudioDecoderFactory, AudioOutputFactory, FrameCounters,
    LatencyCounterSnapshot, MediaOpenRequest, MediaSource, PlayerCommand, PlayerCommandOutcome,
    PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, PlayerRuntimeAcceptedChange,
    PlayerRuntimeApplyError, PlayerRuntimeApplyGroup, PlayerRuntimeApplyGroupReport,
    PlayerRuntimeApplyReport, PlayerRuntimeApplyResult, PlayerRuntimeDecoderThreadConfigUpdate,
    PlayerRuntimeDefaultVolumeUpdate, PlayerRuntimeFrameServerPolicyUpdate,
    PlayerRuntimeSettingsUpdate, PlayerRuntimeTickConfigUpdate, PlayerRuntimeVideoBackendUpdate,
    PlayerSession, PlayerSnapshot, PlayerTickConfig, PlayerTickContext, PlayerTickResult,
    PlayerVideoDecoderThreadConfig, PlayerWorkerWakeupPlan, PreparedMedia,
    SchedulerTimingDiagnosticsSnapshot, StartedVideoBackend, scheduler_timing_diagnostics,
};

mod handle;
mod runtime_commands;
mod runtime_publish;
mod runtime_wait;
mod sender;
#[cfg(test)]
mod tests;

/// Редкий fallback wakeup активного pipeline, когда нет точного media deadline-а.
const DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL: Duration = Duration::from_millis(250);

/// Короткий poll готовности decoder thread-а, пока его frame channel не участвует в `select!`.
const DEFAULT_DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Ёмкость основной очереди команд без high-frequency scrub updates.
const COMMAND_CHANNEL_CAPACITY: usize = 128;

/// Максимум ordinary worker command-ов перед обязательной проверкой render/scrub/tick.
const MAX_COMMANDS_PER_LOOP: usize = 8;

/// Ёмкость latest snapshot stream; worker публикует только актуальное состояние.
const SNAPSHOT_CHANNEL_CAPACITY: usize = 1;

/// Ёмкость event stream; переполнение не должно блокировать playback thread.
const EVENT_CHANNEL_CAPACITY: usize = 512;

/// Интервал throttled diagnostics summary в debug logs.
const DIAGNOSTICS_SUMMARY_INTERVAL: Duration = Duration::from_secs(2);

/// Минимальный возраст final seek-а перед первым stall log-ом.
const FINAL_SEEK_STALL_LOG_MIN_AFTER: Duration = Duration::from_millis(250);

/// Максимальный возраст final seek-а перед первым stall log-ом.
const FINAL_SEEK_STALL_LOG_MAX_AFTER: Duration = Duration::from_millis(1_000);

/// Минимальный интервал между повторными active seek stall logs.
const SEEK_STALL_LOG_INTERVAL: Duration = Duration::from_millis(500);

/// Конфигурация playback worker.
#[derive(Clone)]
pub struct PlayerWorkerConfig {
    /// Редкий progress wakeup для активного pipeline без точного media deadline-а.
    pub coarse_wakeup_interval: Duration,

    /// Poll interval готовности decoder thread-а без привязки к video FPS.
    pub decoder_readiness_poll_interval: Duration,

    /// Scheduler/backpressure лимиты, передаваемые в `PlayerSession::tick`.
    pub tick_config: PlayerTickConfig,

    /// Bounded queue/runtime limits decoder thread-а.
    pub decoder_thread_config: PlayerVideoDecoderThreadConfig,

    /// Default startup/future-media volume policy, не текущая runtime громкость.
    pub default_volume: f32,

    /// Factory audio decoder-а, которую composition layer устанавливает без backend deps в core.
    pub audio_decoder_factory: Arc<dyn AudioDecoderFactory>,

    /// Factory audio output-а, которую composition layer устанавливает без CPAL deps в core.
    pub audio_output_factory: Arc<dyn AudioOutputFactory>,

    /// Validated S19 scrub/scheduler policy snapshot для session-owned live scrub route.
    pub frame_server_config: ValidatedFrameServerConfig,
}

impl fmt::Debug for PlayerWorkerConfig {
    /// Не раскрывает trait object, но показывает остальную runtime конфигурацию.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerWorkerConfig")
            .field("coarse_wakeup_interval", &self.coarse_wakeup_interval)
            .field(
                "decoder_readiness_poll_interval",
                &self.decoder_readiness_poll_interval,
            )
            .field("tick_config", &self.tick_config)
            .field("decoder_thread_config", &self.decoder_thread_config)
            .field("default_volume", &self.default_volume)
            .field("audio_decoder_factory", &"<dyn AudioDecoderFactory>")
            .field("audio_output_factory", &"<dyn AudioOutputFactory>")
            .field("frame_server_config", &self.frame_server_config)
            .finish()
    }
}

impl PlayerWorkerConfig {
    /// Создаёт worker config из runtime tick config приложения.
    #[must_use]
    pub fn new(tick_config: PlayerTickConfig) -> Self {
        Self {
            coarse_wakeup_interval: DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL,
            decoder_readiness_poll_interval: DEFAULT_DECODER_READINESS_POLL_INTERVAL,
            tick_config,
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
            default_volume: 1.0,
            audio_decoder_factory: missing_audio_decoder_factory(),
            audio_output_factory: missing_audio_output_factory(),
            frame_server_config: RuntimeFrameServerConfig::default()
                .validate()
                .expect("default frame-server config must validate"),
        }
    }

    /// Создаёт worker config напрямую из validated app config.
    #[must_use]
    pub fn from_app_config(config: &rustiplayer_config::AppConfig) -> Self {
        Self {
            coarse_wakeup_interval: DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL,
            decoder_readiness_poll_interval: DEFAULT_DECODER_READINESS_POLL_INTERVAL,
            tick_config: PlayerTickConfig::from(config),
            decoder_thread_config: decoder_thread_config_from_app_config(config),
            default_volume: config.audio.volume as f32,
            audio_decoder_factory: missing_audio_decoder_factory(),
            audio_output_factory: missing_audio_output_factory(),
            frame_server_config: Self::frame_server_config_from_app_config(config),
        }
    }

    /// Подставляет audio decoder factory, которой владеет composition layer.
    #[must_use]
    pub fn with_audio_decoder_factory(
        mut self,
        audio_decoder_factory: Arc<dyn AudioDecoderFactory>,
    ) -> Self {
        self.audio_decoder_factory = audio_decoder_factory;
        self
    }

    /// Подставляет audio output factory, которой владеет composition layer.
    #[must_use]
    pub fn with_audio_output_factory(
        mut self,
        audio_output_factory: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        self.audio_output_factory = audio_output_factory;
        self
    }

    /// Возвращает decoder-thread limits из validated app config для shell-owned backend factory.
    #[must_use]
    pub fn decoder_thread_config_from_app_config(
        config: &rustiplayer_config::AppConfig,
    ) -> PlayerVideoDecoderThreadConfig {
        decoder_thread_config_from_app_config(config)
    }

    /// Возвращает validated frame-server policy тем же маппингом, что startup worker config.
    #[must_use]
    pub fn frame_server_config_from_app_config(
        config: &rustiplayer_config::AppConfig,
    ) -> ValidatedFrameServerConfig {
        runtime_frame_server_config_from_persisted(&config.frame_server)
            .validate()
            .expect("validated app frame_server config must map to frame-server-core config")
    }
}

impl Default for PlayerWorkerConfig {
    /// Возвращает production defaults без чтения внешней конфигурации.
    fn default() -> Self {
        Self::new(PlayerTickConfig::default())
    }
}

/// Конвертирует validated TOML config в bounded decoder thread limits.
fn decoder_thread_config_from_app_config(
    config: &rustiplayer_config::AppConfig,
) -> PlayerVideoDecoderThreadConfig {
    PlayerVideoDecoderThreadConfig {
        packet_channel_frames: config.video.decoder_packet_channel_frames,
        frame_channel_frames: config.video.decoder_frame_channel_frames,
        decoder_ready_queue_frames: config.video.decoder_ready_queue_frames,
        decoder_surface_pool_frames: config.video.decoder_surface_pool_frames,
        software_frame_pool_frames: config.video.sw_decoder_surface_pool_frames,
        software_decode_thread_budget: software_decode_thread_budget_from_config(
            config.video.sw_decode_threads,
        ),
        zero_copy_surface_pool_slots: config.video.zero_copy_surface_pool_slots,
        ..PlayerVideoDecoderThreadConfig::from_env()
    }
    .normalized()
}

/// Конвертирует `video.sw_decode_threads` в neutral thread budget.
///
/// `0` = auto (резолв «ядра − 2» живёт в `SoftwareDecodeThreadBudget`),
/// положительное значение = уже разрешённый пользователем лимит.
fn software_decode_thread_budget_from_config(
    sw_decode_threads: usize,
) -> video_core::SoftwareDecodeThreadBudget {
    match std::num::NonZeroUsize::new(sw_decode_threads) {
        Some(thread_count) => video_core::SoftwareDecodeThreadBudget::fixed(thread_count),
        None => video_core::SoftwareDecodeThreadBudget::auto(),
    }
}

/// Конвертирует persisted `[frame_server]` в neutral runtime policy для player-owned scrub.
fn runtime_frame_server_config_from_persisted(
    config: &PersistedFrameServerConfig,
) -> RuntimeFrameServerConfig {
    RuntimeFrameServerConfig {
        live_scrub_max_hz: config.live_scrub_max_hz,
        live_scrub_decode_mode: runtime_live_scrub_decode_mode(config.live_scrub_decode_mode),
        ..RuntimeFrameServerConfig::default()
    }
}

/// Переводит persisted enum в frame-server-core enum без строковых сравнений.
const fn runtime_live_scrub_decode_mode(
    mode: FrameServerLiveScrubDecodeModeConfig,
) -> LiveScrubDecodeMode {
    match mode {
        FrameServerLiveScrubDecodeModeConfig::ThrottledLatest => {
            LiveScrubDecodeMode::ThrottledLatest
        }
        FrameServerLiveScrubDecodeModeConfig::EveryDragEvent => LiveScrubDecodeMode::EveryDragEvent,
    }
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

    /// Normalized scrub state-machine event для app-owned visual override/diagnostics.
    Scrub(frame_server_core::ScrubEvent),

    /// Ошибка render bridge, полученная от shell/render thread.
    RenderError(PlayerRenderError),

    /// Итог одного фонового playback tick.
    Tick(PlayerTickResult),
}

/// Категория ошибки render bridge на границе shell -> worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerRenderErrorKind {
    /// Render thread не смог получить renderer resource по handle lease-а.
    MissingRenderResources,

    /// Backend resource lookup завершился poisoned/fatal состоянием.
    RenderResourceLookupFailed,

    /// Renderer отказал decoded frame metadata или plane contract.
    UnsupportedFrameFormat,

    /// Renderer device/surface не смог завершить render frame.
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
    /// Создаёт ошибку отсутствующего renderer resource для конкретного lease-а.
    #[must_use]
    pub fn missing_render_resources(lease: &VideoFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::MissingRenderResources,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
            message: format!(
                "Render resources are missing for {} frame handle {} in generation {}",
                lease.decoded_frame().format(),
                lease.resource_handle().0,
                lease.render_generation()
            ),
        }
    }

    /// Создаёт ошибку fatal renderer resource lookup для конкретного lease-а.
    #[must_use]
    pub fn render_resource_lookup_failed(lease: &VideoFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::RenderResourceLookupFailed,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
            message: format!(
                "Render resource lookup failed for {} frame handle {} in generation {}",
                lease.decoded_frame().format(),
                lease.resource_handle().0,
                lease.render_generation()
            ),
        }
    }

    /// Создаёт ошибку renderer boundary validation для конкретного lease-а.
    #[must_use]
    pub fn unsupported_frame_format(lease: &VideoFrameLease, message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
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
            PlayerRenderErrorKind::MissingRenderResources
            | PlayerRenderErrorKind::RenderResourceLookupFailed
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
    /// Единственная bounded очередь команд worker-а.
    command_tx: Sender<WorkerCommand>,
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

    /// Render-side handle для прежнего public API acquire/timing без доступа к runtime loop.
    render_bridge_client: RenderLeaseBridgeClient,

    /// Decoder-thread limits, которые shell должен передать concrete backend factory.
    decoder_thread_config: PlayerVideoDecoderThreadConfig,

    /// Аварийный shutdown signal, если command queue недоступна.
    shutdown_tx: Sender<()>,

    /// Join handle фонового потока.
    join_handle: Option<thread::JoinHandle<()>>,
}

/// Внутренние команды worker boundary.
enum WorkerCommand {
    /// Обычная public-команда, которую worker передаст в `PlayerSession`.
    Player(PlayerCommand),

    /// Подключить уже подготовленный media к worker-owned session.
    LoadPreparedMedia {
        /// Prepared-media contract, открытый container adapter-ом вне `player-core`.
        prepared_media: PreparedMedia,

        /// Нужно ли начать playback после успешного открытия.
        autoplay: bool,
    },

    /// Зафиксировать ошибку подготовки media, не скрывая open-request transition.
    MediaOpenFailed {
        /// Исходный запрос, для которого adapter не смог создать demuxer.
        request: MediaOpenRequest,

        /// Уже смэпленная player error с сохранённой категорией ошибки.
        error: PlayerError,
    },

    /// Установка video backend-а, который уже прошёл concrete startup в shell layer.
    SetVideoBackend {
        /// Started backend содержит только playback-facing decoder handle.
        started_backend: StartedVideoBackend,
    },

    /// Shell не смог подобрать совместимый backend под отложенный стрим (например
    /// `hardware`/`software` preference запрещает нужный класс backend-а).
    RejectPendingVideoBackend {
        /// Человекочитаемая причина для typed unsupported error.
        reason: String,
    },

    /// Capability report из shell/backend layer.
    SetSystemCapabilities(SystemCapabilities),

    /// Fatal render boundary error.
    MarkFatalError(PlayerError),

    /// Typed render bridge error.
    RenderError(PlayerRenderError),

    /// Settings-specific command с обязательным response/report.
    ApplyRuntimeSettings {
        /// Typed update, собранный settings binding layer без чтения TOML в player-core.
        update: Box<PlayerRuntimeSettingsUpdate>,

        /// One-shot response channel для реального apply report-а.
        response_tx: Sender<PlayerRuntimeApplyReport>,
    },
}

/// Publisher latest snapshot поверх bounded channel.
struct LatestSnapshotPublisher {
    /// Sender snapshot'ов в app shell.
    snapshot_tx: Sender<PlayerSnapshot>,

    /// Receiver clone нужен worker-у для политики `latest wins`.
    snapshot_rx_for_drain_latest: Receiver<PlayerSnapshot>,
}

/// Stable id одного activity receiver-а внутри worker runtime.
type DecoderActivitySourceId = u64;

/// Receiver identity, по которому worker отличает старый backend от нового.
#[derive(Debug, Clone)]
struct DecoderActivitySource {
    /// Monotonic worker-local id, не видимый за пределами worker-а.
    source_id: DecoderActivitySourceId,

    /// Receiver clone нужен только для `same_channel`, а не для чтения pulse-ов.
    pulse_receiver: Receiver<()>,
}

/// Activity wait, привязанный к одному уже выбранному playback deadline-у.
#[derive(Debug, Clone)]
struct DecoderActivityWaitSource {
    /// Id source-а, который был активен при planning.
    source_id: DecoderActivitySourceId,

    /// Epoch, уже учтённый worker-ом до входа в этот wait.
    observed_epoch: VideoDecoderActivityEpoch,

    /// Snapshot содержит neutral subscription без backend-specific каналов.
    snapshot: VideoDecoderActivitySnapshot,

    /// Receiver clone участвует в `select_biased!` только для этого wait cycle-а.
    pulse_receiver: Receiver<()>,
}

/// Decoder activity state, которым владеет только worker thread.
#[derive(Debug, Default)]
struct WorkerDecoderActivityState {
    /// Последний распознанный activity source.
    active_source: Option<DecoderActivitySource>,

    /// Последний epoch, после которого worker уже запускал tick или baseline-нул новый source.
    last_observed_epoch: VideoDecoderActivityEpoch,

    /// Source, отключённый после fatal/disconnected notifier до замены backend-а.
    disabled_source_id: Option<DecoderActivitySourceId>,

    /// Следующий worker-local source id.
    next_source_id: DecoderActivitySourceId,
}

/// Полный worker wait plan: playback deadline плюс optional decoder activity source.
#[derive(Debug, Clone)]
struct PlannedWorkerWait {
    /// Playback wakeup, уже выбранный scheduler-ом.
    wakeup: PlannedWorkerWakeup,

    /// Decoder activity source используется только если plan явно попросил wait.
    decoder_activity: Option<DecoderActivityWaitSource>,
}

/// Результат одной итерации timed `select!`.
enum WorkerTimedWaitOutcome {
    /// Render/stale activity обработаны; нужно продолжить ждать старый absolute deadline.
    ContinueWaiting,

    /// Wait завершён command/shutdown/activity/timeout outcome-ом.
    Finished { shutdown_requested: bool },
}

/// Что делать после typed decoder activity outcome-а.
enum DecoderActivityWaitAction {
    /// Новая activity должна немедленно запустить playback tick.
    RunPlaybackTick,

    /// Новой activity нет или source отключён; продолжаем ждать fallback deadline.
    ContinueWaiting,
}

/// Runtime state, который живёт только на worker thread.
struct PlayerWorkerRuntime {
    /// Worker-owned player session и весь playback pipeline.
    session: PlayerSession,

    /// Чистый planner ближайшего worker wakeup-а без владения session/scrub state.
    worker_scheduler: WorkerScheduler,

    /// Worker-owned state neutral decoder activity wait-а.
    decoder_activity: WorkerDecoderActivityState,

    /// Receiver основной очереди команд.
    command_rx: Receiver<WorkerCommand>,

    /// Latest snapshot publisher.
    snapshot_publisher: LatestSnapshotPublisher,

    /// Event stream sender.
    event_tx: Sender<PlayerWorkerEvent>,

    /// Worker-side bridge render lease handoff/release/diagnostics.
    render_bridge: RenderLeaseBridge,

    /// Аварийный shutdown receiver.
    shutdown_rx: Receiver<()>,

    /// Runtime config worker-а.
    config: PlayerWorkerConfig,

    /// Момент последнего playback tick.
    last_tick_at: Instant,

    /// Последний debug diagnostics summary.
    last_diagnostics_summary_at: Instant,

    /// Последний active seek transaction, для которого печатали stall diagnostics.
    last_seek_stall_log_key: Option<(u64, &'static str)>,

    /// Последний момент throttled active seek stall log-а.
    last_seek_stall_log_at: Option<Instant>,
}
