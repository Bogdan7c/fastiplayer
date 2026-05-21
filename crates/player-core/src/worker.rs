use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use media_core::{MediaTime, TrackId};
use tracing::{debug, warn};

#[cfg(test)]
use crate::render_lease_bridge::{
    LatestPresentFrameAcquire, LatestPresentFrameHandoff, RenderAcquireSample, RenderLeaseRelease,
    RenderTextureViewPreviousFrameReuseSample, RenderTimingSample,
};
use crate::render_lease_bridge::{
    PlayerPresentFrame, PresentFrameLease, RenderLeaseBridge, RenderLeaseBridgeClient,
};
use crate::scrub_command::{
    ScrubCoalescedUpdateDecision, ScrubCoalescerSubmitError, ScrubCommandSequencer,
    ScrubSequencerBeginDecision, ScrubSequencerEndDecision, ScrubSequencerInterruptDecision,
    ScrubSequencerUpdateDecision, ScrubUpdateCoalescer,
};
use crate::scrub_driver::{
    ScrubBeginDecision, ScrubBeginPlaybackAction, ScrubDriver, ScrubEndDecision,
    ScrubInterruptDecision, ScrubPreviewDecision, ScrubPreviewDispatch, ScrubUpdateDecision,
};
use crate::seek_controller::{PlaybackResumeCommand, PlaybackResumeIntent};
use crate::tick::PlayerWorkerWakeupPlan;
use crate::worker_scheduler::{PlannedWorkerWakeup, WorkerScheduler, WorkerWakeupDeadline};
use crate::{
    ActiveSeekDiagnosticsSnapshot, FrameCounters, LatencyCounterSnapshot, MediaOpenRequest,
    MediaSource, PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult,
    PlayerSession, PlayerSnapshot, PlayerTickConfig, PlayerTickContext, PlayerTickResult,
    PlayerVideoDecoderThreadConfig, PreparedMedia, QualitySelection, ScrubCommitIntent,
    ScrubCommitPolicy, ScrubGeneration, ScrubUpdateIntent, SeekRequest, SessionScrubCommand,
    VideoBackendFactory,
};

/// Редкий fallback wakeup активного pipeline, когда нет точного media deadline-а.
const DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL: Duration = Duration::from_millis(250);

/// Короткий poll готовности decoder thread-а, пока его frame channel не участвует в `select!`.
const DEFAULT_DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Ёмкость основной очереди команд без high-frequency scrub updates.
const COMMAND_CHANNEL_CAPACITY: usize = 128;

/// Максимум ordinary worker command-ов перед обязательной проверкой render/scrub/tick.
const MAX_COMMANDS_PER_LOOP: usize = 8;

/// Ёмкость wake-очереди scrub updates; сами координаты лежат в latest-slot.
const SCRUB_WAKE_CHANNEL_CAPACITY: usize = 1;

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

/// Максимальный возраст preview seek-а перед stall log-ом.
const PREVIEW_SEEK_STALL_LOG_MAX_AFTER: Duration = Duration::from_millis(250);

/// Минимальный интервал между повторными active seek stall logs.
const SEEK_STALL_LOG_INTERVAL: Duration = Duration::from_millis(500);

/// Default throttle live preview seek-а, если worker создан без app config.
const DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL: Duration = Duration::from_millis(100);

/// Конфигурация playback worker.
#[derive(Debug, Clone, Copy)]
pub struct PlayerWorkerConfig {
    /// Редкий progress wakeup для активного pipeline без точного media deadline-а.
    pub coarse_wakeup_interval: Duration,

    /// Poll interval готовности decoder thread-а без привязки к video FPS.
    pub decoder_readiness_poll_interval: Duration,

    /// Scheduler/backpressure лимиты, передаваемые в `PlayerSession::tick`.
    pub tick_config: PlayerTickConfig,

    /// Минимальный интервал между live preview seek-ами во время scrub.
    pub live_scrub_preview_interval: Duration,

    /// Bounded queue/runtime limits decoder thread-а.
    pub decoder_thread_config: PlayerVideoDecoderThreadConfig,
}

impl PlayerWorkerConfig {
    /// Создаёт worker config из runtime tick config приложения.
    #[must_use]
    pub fn new(tick_config: PlayerTickConfig) -> Self {
        Self {
            coarse_wakeup_interval: DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL,
            decoder_readiness_poll_interval: DEFAULT_DECODER_READINESS_POLL_INTERVAL,
            tick_config,
            live_scrub_preview_interval: DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL,
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        }
    }

    /// Создаёт worker config напрямую из validated app config.
    #[must_use]
    pub fn from_app_config(config: &rustiplayer_config::AppConfig) -> Self {
        Self {
            coarse_wakeup_interval: DEFAULT_WORKER_COARSE_WAKEUP_INTERVAL,
            decoder_readiness_poll_interval: DEFAULT_DECODER_READINESS_POLL_INTERVAL,
            tick_config: PlayerTickConfig::from(config),
            live_scrub_preview_interval: Duration::from_millis(config.player.seek.live_interval_ms),
            decoder_thread_config: decoder_thread_config_from_app_config(config),
        }
    }

    /// Возвращает decoder-thread limits из validated app config для shell-owned backend factory.
    #[must_use]
    pub fn decoder_thread_config_from_app_config(
        config: &rustiplayer_config::AppConfig,
    ) -> PlayerVideoDecoderThreadConfig {
        decoder_thread_config_from_app_config(config)
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
        zero_copy_surface_pool_slots: config.video.zero_copy_surface_pool_slots,
        ..PlayerVideoDecoderThreadConfig::from_env()
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

impl From<ScrubCoalescerSubmitError> for PlayerWorkerSendError {
    /// Сохраняет public sender error contract для coalesced scrub wake boundary.
    fn from(error: ScrubCoalescerSubmitError) -> Self {
        match error {
            ScrubCoalescerSubmitError::Disconnected => Self::Disconnected,
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

    /// Backend texture view lookup завершился poisoned/fatal состоянием.
    TextureViewLookupFailed,

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

    /// Создаёт ошибку fatal texture view lookup для конкретного lease-а.
    #[must_use]
    pub fn texture_view_lookup_failed(lease: &PresentFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::TextureViewLookupFailed,
            render_generation: Some(lease.render_generation),
            frame_handle: Some(lease.texture_handle().0),
            message: format!(
                "Render texture view lookup failed for {} frame handle {} in generation {}",
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
            | PlayerRenderErrorKind::TextureViewLookupFailed
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

/// Public scrub command, который sender должен маршрутизировать вне обычной worker queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerCommandScrubRoute {
    /// Начать sender-sequenced scrub operation.
    Begin,

    /// Сохранить latest scrub target в coalesced update slot.
    Update(SeekRequest),

    /// Отправить explicit preview command с текущим generation token-ом.
    Preview(SeekRequest),

    /// Завершить active scrub operation выбранной policy.
    End(ScrubCommitPolicy),
}

/// Worker-side player command без scrub variants.
///
/// Этот тип является внутренней границей: `WorkerCommand::Player` больше не
/// принимает raw `PlayerCommand`, поэтому interactive scrub не может попасть
/// в ordinary worker path без sender-side sequencer/coalescer.
#[derive(Debug, Clone, PartialEq)]
enum WorkerPlayerCommand {
    /// Открыть новый media-источник.
    OpenMedia(MediaOpenRequest),

    /// Начать или продолжить воспроизведение.
    Play,

    /// Приостановить воспроизведение.
    Pause,

    /// Переключить состояние между play и pause.
    TogglePlayback,

    /// Перемотать текущий media.
    Seek(SeekRequest),

    /// Остановить текущий media без завершения всей session.
    Stop,

    /// Установить громкость в диапазоне `0.0..=1.0`.
    SetVolume(f32),

    /// Выбрать активный video track.
    SelectVideoTrack(TrackId),

    /// Выбрать активный audio track.
    SelectAudioTrack(TrackId),

    /// Выбрать subtitle track или отключить субтитры через `None`.
    SelectSubtitleTrack(Option<TrackId>),

    /// Выбрать качество потока.
    SelectQuality(QualitySelection),

    /// Перечитать runtime config.
    ReloadConfig,

    /// Завершить player session.
    Shutdown,
}

impl TryFrom<PlayerCommand> for WorkerPlayerCommand {
    type Error = PlayerCommandScrubRoute;

    /// Классифицирует public command на sender boundary.
    fn try_from(command: PlayerCommand) -> Result<Self, Self::Error> {
        match command {
            PlayerCommand::OpenMedia(request) => Ok(Self::OpenMedia(request)),
            PlayerCommand::Play => Ok(Self::Play),
            PlayerCommand::Pause => Ok(Self::Pause),
            PlayerCommand::TogglePlayback => Ok(Self::TogglePlayback),
            PlayerCommand::Seek(request) => Ok(Self::Seek(request)),
            PlayerCommand::BeginScrub => Err(PlayerCommandScrubRoute::Begin),
            PlayerCommand::UpdateScrub(request) => Err(PlayerCommandScrubRoute::Update(request)),
            PlayerCommand::PreviewScrub(request) => Err(PlayerCommandScrubRoute::Preview(request)),
            PlayerCommand::EndScrub { policy } => Err(PlayerCommandScrubRoute::End(policy)),
            PlayerCommand::Stop => Ok(Self::Stop),
            PlayerCommand::SetVolume(volume) => Ok(Self::SetVolume(volume)),
            PlayerCommand::SelectVideoTrack(track_id) => Ok(Self::SelectVideoTrack(track_id)),
            PlayerCommand::SelectAudioTrack(track_id) => Ok(Self::SelectAudioTrack(track_id)),
            PlayerCommand::SelectSubtitleTrack(track_id) => Ok(Self::SelectSubtitleTrack(track_id)),
            PlayerCommand::SelectQuality(selection) => Ok(Self::SelectQuality(selection)),
            PlayerCommand::ReloadConfig => Ok(Self::ReloadConfig),
            PlayerCommand::Shutdown => Ok(Self::Shutdown),
        }
    }
}

impl WorkerPlayerCommand {
    /// Возвращает `true` для external boundary команд, которые закрывают active scrub lifecycle.
    fn invalidates_sender_scrub_generation(&self) -> bool {
        matches!(self, Self::OpenMedia(_) | Self::Stop | Self::Shutdown)
    }

    /// Классифицирует playback-команду в operation-level resume intent boundary.
    fn playback_resume_command(&self) -> Option<PlaybackResumeCommand> {
        match self {
            Self::Play => Some(PlaybackResumeCommand::Play),
            Self::Pause => Some(PlaybackResumeCommand::Pause),
            Self::TogglePlayback => Some(PlaybackResumeCommand::TogglePlayback),
            _ => None,
        }
    }

    /// Восстанавливает public session command после worker-side policy decisions.
    fn into_player_command(self) -> PlayerCommand {
        match self {
            Self::OpenMedia(request) => PlayerCommand::OpenMedia(request),
            Self::Play => PlayerCommand::Play,
            Self::Pause => PlayerCommand::Pause,
            Self::TogglePlayback => PlayerCommand::TogglePlayback,
            Self::Seek(request) => PlayerCommand::Seek(request),
            Self::Stop => PlayerCommand::Stop,
            Self::SetVolume(volume) => PlayerCommand::SetVolume(volume),
            Self::SelectVideoTrack(track_id) => PlayerCommand::SelectVideoTrack(track_id),
            Self::SelectAudioTrack(track_id) => PlayerCommand::SelectAudioTrack(track_id),
            Self::SelectSubtitleTrack(track_id) => PlayerCommand::SelectSubtitleTrack(track_id),
            Self::SelectQuality(selection) => PlayerCommand::SelectQuality(selection),
            Self::ReloadConfig => PlayerCommand::ReloadConfig,
            Self::Shutdown => PlayerCommand::Shutdown,
        }
    }
}

/// Cloneable sender для команд player worker.
#[derive(Clone)]
pub struct PlayerCommandSender {
    /// Основная очередь low-frequency команд.
    command_tx: Sender<WorkerCommand>,

    /// Явный latest-slot для high-frequency scrub updates.
    scrub_coalescer: Arc<ScrubUpdateCoalescer>,

    /// Общий generation sequencer для всех clone-сендеров UI/integration слоя.
    scrub_command_sequencer: Arc<Mutex<ScrubCommandSequencer>>,
}

impl PlayerCommandSender {
    /// Отправляет команду без блокировки render/UI thread.
    pub fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        match WorkerPlayerCommand::try_from(command) {
            Ok(worker_command) => self.try_send_worker_player_command(worker_command),
            Err(PlayerCommandScrubRoute::Begin) => self.try_send_begin_scrub(),
            Err(PlayerCommandScrubRoute::Update(request)) => self.try_send_scrub_update(request),
            Err(PlayerCommandScrubRoute::Preview(request)) => self.try_send_preview_scrub(request),
            Err(PlayerCommandScrubRoute::End(policy)) => self.try_send_end_scrub(policy),
        }
    }

    /// Отправляет ordinary command в worker queue и обновляет sender-side scrub lifecycle.
    fn try_send_worker_player_command(
        &self,
        command: WorkerPlayerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        let invalidates_scrub_generation = command.invalidates_sender_scrub_generation();

        self.command_tx
            .try_send(WorkerCommand::Player(command))
            .map_err(PlayerWorkerSendError::from)?;

        if invalidates_scrub_generation {
            self.invalidate_sender_scrub_generation();
        }

        Ok(())
    }

    /// Отправляет начало scrub-а с новым generation token-ом.
    fn try_send_begin_scrub(&self) -> Result<(), PlayerWorkerSendError> {
        let mut sequencer = self.scrub_command_sequencer_guard();
        let decision = sequencer.dispatch_begin_scrub(|generation| {
            self.command_tx
                .try_send(WorkerCommand::BeginScrub { generation })
                .map_err(PlayerWorkerSendError::from)
        })?;
        match decision {
            ScrubSequencerBeginDecision::Started { generation } => {
                debug!(
                    generation = generation.as_u64(),
                    "Sender sequenced BeginScrub"
                );
            }
            ScrubSequencerBeginDecision::ReplacedActive {
                previous,
                generation,
            } => {
                debug!(
                    previous_generation = previous.as_u64(),
                    generation = generation.as_u64(),
                    "Sender sequenced BeginScrub over active scrub"
                );
            }
        }
        Ok(())
    }

    /// Отправляет latest scrub target без доступа sender-а к receiver side.
    fn try_send_scrub_update(&self, request: SeekRequest) -> Result<(), PlayerWorkerSendError> {
        match self.scrub_command_sequencer_guard().update_intent(request) {
            ScrubSequencerUpdateDecision::Accepted { intent } => self
                .scrub_coalescer
                .submit_latest(intent)
                .map_err(PlayerWorkerSendError::from),
            ScrubSequencerUpdateDecision::Rejected { request, reason } => {
                debug!(
                    request = ?request,
                    reason = ?reason,
                    "Sender rejected UpdateScrub before worker wakeup"
                );
                Ok(())
            }
        }
    }

    /// Отправляет explicit preview scrub command с текущим generation token-ом.
    fn try_send_preview_scrub(&self, request: SeekRequest) -> Result<(), PlayerWorkerSendError> {
        match self.scrub_command_sequencer_guard().update_intent(request) {
            ScrubSequencerUpdateDecision::Accepted { intent } => self
                .command_tx
                .try_send(WorkerCommand::PreviewScrub(intent))
                .map_err(PlayerWorkerSendError::from),
            ScrubSequencerUpdateDecision::Rejected { request, reason } => {
                debug!(
                    request = ?request,
                    reason = ?reason,
                    "Sender rejected PreviewScrub before worker command"
                );
                Ok(())
            }
        }
    }

    /// Отправляет завершение scrub-а и инвалидирует поздние updates старого intent-а.
    fn try_send_end_scrub(&self, policy: ScrubCommitPolicy) -> Result<(), PlayerWorkerSendError> {
        let mut sequencer = self.scrub_command_sequencer_guard();
        let decision = sequencer.dispatch_end_scrub(policy, |intent| {
            self.command_tx
                .try_send(WorkerCommand::EndScrub(intent))
                .map_err(PlayerWorkerSendError::from)
        })?;
        match decision {
            ScrubSequencerEndDecision::Accepted { intent } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    policy = ?intent.policy,
                    "Sender sequenced EndScrub"
                );
            }
            ScrubSequencerEndDecision::Rejected { policy, reason } => {
                debug!(
                    policy = ?policy,
                    reason = ?reason,
                    "Sender rejected EndScrub before worker command"
                );
            }
        }
        Ok(())
    }

    /// Инвалидирует sender-side scrub generation после внешней boundary-команды.
    fn invalidate_sender_scrub_generation(&self) {
        match self.scrub_command_sequencer_guard().interrupt_scrub() {
            ScrubSequencerInterruptDecision::Interrupted {
                previous,
                generation,
            } => {
                debug!(
                    previous_generation = previous.as_u64(),
                    generation = generation.as_u64(),
                    "Sender interrupted active scrub generation"
                );
            }
            ScrubSequencerInterruptDecision::Idle {
                previous,
                generation,
                reason,
            } => {
                debug!(
                    previous_generation = previous.as_u64(),
                    generation = generation.as_u64(),
                    reason = ?reason,
                    "Sender advanced idle scrub generation for external boundary"
                );
            }
        }
    }

    /// Возвращает sequencer guard и восстанавливается после poison без потери команд.
    fn scrub_command_sequencer_guard(&self) -> MutexGuard<'_, ScrubCommandSequencer> {
        match self.scrub_command_sequencer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("Scrub command sequencer mutex was poisoned; recovering generation state");
                poisoned.into_inner()
            }
        }
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

    /// Render-side handle для прежнего public API acquire/timing без доступа к runtime loop.
    render_bridge_client: RenderLeaseBridgeClient,

    /// Decoder-thread limits, которые shell должен передать concrete backend factory.
    decoder_thread_config: PlayerVideoDecoderThreadConfig,

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
        let (render_bridge, render_bridge_client) = RenderLeaseBridge::new();
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let scrub_coalescer = Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx));
        let decoder_thread_config = config.decoder_thread_config;

        let command_sender = PlayerCommandSender {
            command_tx,
            scrub_coalescer: Arc::clone(&scrub_coalescer),
            scrub_command_sequencer: Arc::new(Mutex::new(ScrubCommandSequencer::new())),
        };
        let snapshot_rx_for_worker = snapshot_rx.clone();

        let worker_started_at = Instant::now();
        let join_handle = thread::Builder::new()
            .name("player-worker".into())
            .spawn(move || {
                let runtime = PlayerWorkerRuntime {
                    session: PlayerSession::new(),
                    scrub_driver: ScrubDriver::new(config.live_scrub_preview_interval),
                    worker_scheduler: WorkerScheduler,
                    command_rx,
                    scrub_wake_rx,
                    scrub_coalescer,
                    snapshot_publisher: LatestSnapshotPublisher::new(
                        snapshot_tx,
                        snapshot_rx_for_worker,
                    ),
                    event_tx,
                    render_bridge,
                    shutdown_rx,
                    config,
                    last_tick_at: worker_started_at,
                    last_diagnostics_summary_at: worker_started_at,
                    last_seek_stall_log_key: None,
                    last_seek_stall_log_at: None,
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
            render_bridge_client,
            decoder_thread_config,
            shutdown_tx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает cloneable sender для long-lived UI callbacks.
    #[must_use]
    pub fn command_sender(&self) -> PlayerCommandSender {
        self.command_sender.clone()
    }

    /// Возвращает decoder-thread limits, которые shell использует при сборке backend factory.
    #[must_use]
    pub const fn decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.decoder_thread_config
    }

    /// Отправляет обычную player command без блокировки.
    pub fn try_send_command(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.command_sender.try_send(command)
    }

    /// Передаёт уже подготовленный media во владение worker thread.
    pub fn load_prepared_media(
        &self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::LoadPreparedMedia {
                prepared_media,
                autoplay,
            })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт уже открытый streaming demuxer во владение worker thread.
    pub fn load_demuxer(
        &self,
        label: String,
        demuxer: Box<dyn media_core::Demuxer + Send>,
        autoplay: bool,
    ) -> Result<(), PlayerWorkerSendError> {
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        self.load_prepared_media(prepared_media, autoplay)
    }

    /// Публикует ошибку adapter-а, который не смог подготовить media.
    pub fn fail_media_open(
        &self,
        request: MediaOpenRequest,
        error: PlayerError,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::MediaOpenFailed { request, error })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Инициализирует video decoder pipeline через factory, созданную shell composition root.
    pub fn init_video_pipeline(
        &self,
        backend_factory: impl VideoBackendFactory + 'static,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::InitVideoPipeline {
                backend_factory: Box::new(backend_factory),
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
        self.render_bridge_client.try_acquire_present_frame()
    }

    /// Сообщает worker-у renderer submit/present timing без блокировки render thread.
    pub fn report_gpu_submit_present_latency(&self, submit_present_elapsed: Duration) {
        self.render_bridge_client
            .report_gpu_submit_present_latency(submit_present_elapsed);
    }

    /// Сообщает worker-у, что renderer повторил previous valid frame из-за busy texture lock-а.
    pub fn report_texture_view_previous_frame_reuse(&self) {
        self.render_bridge_client
            .report_texture_view_previous_frame_reuse();
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
    /// Обычная worker-side команда без interactive scrub variants.
    Player(WorkerPlayerCommand),

    /// Начать scrub с generation, выданным sender boundary.
    BeginScrub {
        /// User intent generation, общий для дальнейших update/preview/end.
        generation: ScrubGeneration,
    },

    /// Explicit preview command с generation token-ом.
    PreviewScrub(ScrubUpdateIntent),

    /// Завершить scrub с generation token-ом.
    EndScrub(ScrubCommitIntent),

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

    /// Инициализация video backend через concrete factory, созданную shell layer.
    InitVideoPipeline {
        /// Concrete backend factory уже содержит GPU handles и startup config.
        backend_factory: Box<dyn VideoBackendFactory>,
    },

    /// Capability report из shell/backend layer.
    SetSystemCapabilities(SystemCapabilities),

    /// Fatal render boundary error.
    MarkFatalError(PlayerError),

    /// Typed render bridge error.
    RenderError(PlayerRenderError),
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

    /// Worker-side coordinator seek/scrub orchestration.
    scrub_driver: ScrubDriver,

    /// Чистый planner ближайшего worker wakeup-а без владения session/scrub state.
    worker_scheduler: WorkerScheduler,

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

impl PlayerWorkerRuntime {
    /// Главный цикл worker thread.
    fn run(mut self) {
        self.publish_session_outputs();

        loop {
            self.drain_render_feedback();

            if self.shutdown_rx.try_recv().is_ok() {
                self.handle_shutdown_request();
                break;
            }

            let processed_commands = self.drain_pending_command_batch();
            self.service_worker_fairness_checkpoint(processed_commands);
            self.log_active_seek_stall_if_needed(Instant::now());

            if self.session.is_shutdown_requested() {
                break;
            }

            if processed_commands == MAX_COMMANDS_PER_LOOP {
                continue;
            }

            if self.wait_for_worker_wakeup() {
                break;
            }
        }

        self.publish_session_outputs();
    }

    /// Снимает render feedback, которым worker владеет как частью session lifecycle.
    fn drain_render_feedback(&mut self) {
        self.render_bridge.drain_releases(&mut self.session);
        self.render_bridge.drain_diagnostics(&mut self.session);
    }

    /// Обрабатывает bounded batch pending command-ов без монополизации worker loop-а.
    fn drain_pending_command_batch(&mut self) -> usize {
        let mut processed_commands = 0;

        for _ in 0..MAX_COMMANDS_PER_LOOP {
            let Some(command) = self.receive_next_command() else {
                break;
            };

            self.handle_worker_command(command);
            self.publish_session_outputs();
            processed_commands += 1;

            if self.session.is_shutdown_requested() {
                break;
            }
        }

        processed_commands
    }

    /// Обязательная fairness-точка после command batch-а.
    fn service_worker_fairness_checkpoint(&mut self, processed_commands: usize) {
        self.drain_render_feedback();
        self.apply_latest_scrub_update();
        self.dispatch_due_preview_scrub_seek();
        if processed_commands > 0 {
            self.run_overdue_playback_tick();
        }
    }

    /// Ждёт ближайший command/render/shutdown wakeup вместо fixed idle polling.
    fn wait_for_worker_wakeup(&mut self) -> bool {
        match self.plan_next_worker_wakeup() {
            Some(wakeup) if wakeup.timeout().is_zero() => {
                self.handle_worker_timeout(wakeup.deadline());
                false
            }
            Some(wakeup) => self.wait_for_worker_wakeup_with_timeout(wakeup),
            None => self.wait_for_worker_wakeup_until_event(),
        }
    }

    /// Блокируется до события или ближайшего tick/preview deadline-а.
    fn wait_for_worker_wakeup_with_timeout(&mut self, wakeup: PlannedWorkerWakeup) -> bool {
        crossbeam_channel::select! {
            recv(self.command_rx) -> command_result => {
                self.handle_command_wakeup(command_result)
            }
            recv(self.render_bridge.render_release_receiver()) -> release_result => {
                self.render_bridge
                    .handle_release_wakeup(&mut self.session, release_result);
                false
            }
            recv(self.render_bridge.render_acquire_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_acquire_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.render_timing_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_timing_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.texture_view_lock_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_texture_view_lock_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.texture_view_previous_frame_reuse_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_texture_view_previous_frame_reuse_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.scrub_wake_rx) -> wake_result => {
                self.handle_scrub_wakeup(wake_result)
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                true
            }
            default(wakeup.timeout()) => {
                self.handle_worker_timeout(wakeup.deadline());
                false
            },
        }
    }

    /// Блокируется без timeout, когда playback idle и нет preview deadline-а.
    fn wait_for_worker_wakeup_until_event(&mut self) -> bool {
        crossbeam_channel::select! {
            recv(self.command_rx) -> command_result => {
                self.handle_command_wakeup(command_result)
            }
            recv(self.render_bridge.render_release_receiver()) -> release_result => {
                self.render_bridge
                    .handle_release_wakeup(&mut self.session, release_result);
                false
            }
            recv(self.render_bridge.render_acquire_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_acquire_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.render_timing_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_timing_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.texture_view_lock_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_texture_view_lock_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.texture_view_previous_frame_reuse_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_texture_view_previous_frame_reuse_sample_wakeup(&mut self.session, sample_result);
                false
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

    /// Делегирует вычисление ближайшего самостоятельного wakeup-а чистому scheduler helper-у.
    fn plan_next_worker_wakeup(&self) -> Option<PlannedWorkerWakeup> {
        let now = Instant::now();
        self.worker_scheduler.next_worker_wakeup_deadline(
            now,
            &self.config.tick_config,
            self.config.decoder_readiness_poll_interval,
            self.config.coarse_wakeup_interval,
            |now, tick_config, decoder_readiness_poll_interval, coarse_wakeup_interval| {
                self.session.worker_wakeup_plan(
                    now,
                    tick_config,
                    decoder_readiness_poll_interval,
                    coarse_wakeup_interval,
                )
            },
            |now| self.scrub_driver.next_preview_scrub_timeout(now),
        )
    }

    /// Выполняет playback tick без ожидания, если media planner уже вернул due deadline.
    fn run_overdue_playback_tick(&mut self) {
        let Some(wakeup) = self.worker_scheduler.next_playback_wakeup_deadline(
            Instant::now(),
            &self.config.tick_config,
            self.config.decoder_readiness_poll_interval,
            self.config.coarse_wakeup_interval,
            |now, tick_config, decoder_readiness_poll_interval, coarse_wakeup_interval| {
                self.session.worker_wakeup_plan(
                    now,
                    tick_config,
                    decoder_readiness_poll_interval,
                    coarse_wakeup_interval,
                )
            },
        ) else {
            return;
        };

        if !wakeup.timeout().is_zero() {
            return;
        }

        if let WorkerWakeupDeadline::Playback { plan, deadline } = wakeup.deadline() {
            self.run_tick_for_wakeup_plan(plan, deadline);
        }
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
            Err(TryRecvError::Disconnected) => {
                Some(WorkerCommand::Player(WorkerPlayerCommand::Shutdown))
            }
        }
    }

    /// Обрабатывает одну worker command.
    fn handle_worker_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Player(player_command) => self.handle_player_command(player_command),
            WorkerCommand::BeginScrub { generation } => self.handle_begin_scrub_command(generation),
            WorkerCommand::PreviewScrub(intent) => {
                self.handle_preview_scrub_command(intent);
            }
            WorkerCommand::EndScrub(intent) => self.handle_end_scrub_command(intent),
            WorkerCommand::LoadPreparedMedia {
                prepared_media,
                autoplay,
            } => {
                self.interrupt_scrub_for_external_boundary();
                self.session
                    .load_prepared_media_with_autoplay(prepared_media, autoplay);
            }
            WorkerCommand::MediaOpenFailed { request, error } => {
                self.interrupt_scrub_for_external_boundary();
                self.session.fail_media_open_with_error(request, error);
            }
            WorkerCommand::InitVideoPipeline { backend_factory } => {
                self.session.init_video_pipeline(backend_factory.as_ref());
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
        }
    }

    /// Сохраняет typed render error в snapshot и публикует worker event.
    fn handle_render_error(&mut self, error: PlayerRenderError) {
        self.publish_worker_event(PlayerWorkerEvent::RenderError(error.clone()));
        self.session.mark_fatal_error(error.to_player_error());
    }

    /// Применяет ordinary worker command с worker-level seek/scrub priority policy.
    fn handle_player_command(&mut self, command: WorkerPlayerCommand) {
        if self.consume_scrub_resume_intent_command(&command) {
            return;
        }

        match command {
            WorkerPlayerCommand::OpenMedia(request) => self.handle_open_media_request(request),
            WorkerPlayerCommand::Stop => self.handle_stop_command(),
            WorkerPlayerCommand::Shutdown => self.handle_shutdown_request(),
            WorkerPlayerCommand::Seek(request) => self.handle_seek_command(request),
            other_command => self.dispatch_player_command(other_command.into_player_command()),
        }
    }

    /// Обновляет сохранённый resume intent для Play/Pause/Toggle во время active scrub.
    fn consume_scrub_resume_intent_command(&mut self, command: &WorkerPlayerCommand) -> bool {
        let Some(resume_command) = command.playback_resume_command() else {
            return false;
        };

        self.scrub_driver
            .consume_resume_intent_command(resume_command)
    }

    /// Открывает media request без знания concrete container adapter-а.
    fn handle_open_media_request(&mut self, request: MediaOpenRequest) {
        self.interrupt_scrub_for_external_boundary();

        match request.source.clone() {
            MediaSource::LocalFile(_) => {
                self.session.fail_media_open_with_error(
                    request,
                    PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        "Локальный файл должен быть подготовлен adapter-слоем до player-core",
                    ),
                );
            }
            MediaSource::Url(_) | MediaSource::ExternalLabel(_) => {
                self.dispatch_player_command(PlayerCommand::OpenMedia(request));
            }
        }
    }

    /// Stop во время scrub сначала просит pause + seek zero, затем сбрасывает media.
    fn handle_stop_command(&mut self) {
        if matches!(
            self.scrub_driver.interrupt_scrub(),
            ScrubInterruptDecision::Interrupted
        ) {
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
    fn handle_begin_scrub_command(&mut self, generation: ScrubGeneration) {
        debug!(
            generation = generation.as_u64(),
            playback_state = ?self.session.playback_state(),
            "Worker принял BeginScrub"
        );
        let decision = self
            .scrub_driver
            .begin_scrub(generation, self.session.playback_state());
        self.dispatch_begin_scrub_decision(decision);
    }

    /// Отправляет begin decision в session и pause command.
    fn dispatch_begin_scrub_decision(&mut self, decision: ScrubBeginDecision) {
        self.dispatch_scrub_command(decision.session_command);
        self.dispatch_begin_scrub_playback_action(decision.playback_action);
    }

    /// Конвертирует internal begin-scrub playback action в существующий session command.
    fn dispatch_begin_scrub_playback_action(&mut self, action: ScrubBeginPlaybackAction) {
        match action {
            ScrubBeginPlaybackAction::Pause => self.dispatch_player_command(PlayerCommand::Pause),
        }
    }

    /// EndScrub применяет выбранную commit policy и сохранённый resume intent.
    fn handle_end_scrub_command(&mut self, intent: ScrubCommitIntent) {
        debug!(
            generation = intent.generation.as_u64(),
            policy = ?intent.policy,
            latest_target = ?self.scrub_driver.latest_scrub_target(),
            in_flight_target = ?self.scrub_driver.in_flight_target(),
            "Worker принял EndScrub"
        );
        self.apply_latest_scrub_update_for_commit();

        let decision = self.scrub_driver.end_scrub(intent);
        self.dispatch_end_scrub_decision(decision);
    }

    /// Отправляет end decision в session и применяет resume intent.
    fn dispatch_end_scrub_decision(&mut self, decision: ScrubEndDecision) {
        match decision {
            ScrubEndDecision::Accepted {
                intent: accepted_intent,
                session_command,
                resume_intent,
            } => {
                self.dispatch_scrub_command(session_command);
                debug!(
                    generation = accepted_intent.generation.as_u64(),
                    resume_intent = ?resume_intent,
                    has_active_seek_commit = self.session.has_active_seek_commit(),
                    timeline = ?self.session.snapshot().timeline,
                    "Worker применил EndScrub к session"
                );
                self.apply_end_scrub_resume_intent(resume_intent);
                self.scrub_driver.reset_preview_scrub_state();
            }
            ScrubEndDecision::Rejected {
                intent: rejected_intent,
                reason,
            } => {
                debug!(
                    generation = rejected_intent.generation.as_u64(),
                    reason = ?reason,
                    "EndScrub rejected before PlayerSession"
                );
            }
        }
    }

    /// PreviewScrub проходит в session только если generation и latest target всё ещё актуальны.
    fn handle_preview_scrub_command(&mut self, intent: ScrubUpdateIntent) -> bool {
        let decision = self.scrub_driver.preview_scrub(intent);
        self.dispatch_preview_scrub_decision(decision)
    }

    /// Отправляет preview decision в session и логирует rejected preview intent-ы.
    fn dispatch_preview_scrub_decision(&mut self, decision: ScrubPreviewDecision) -> bool {
        match decision {
            ScrubPreviewDecision::Idle => false,
            ScrubPreviewDecision::Dispatch {
                intent,
                session_command,
            } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    request = ?intent.request,
                    "Worker dispatches PreviewScrub"
                );
                self.dispatch_scrub_command(session_command);
                true
            }
            ScrubPreviewDecision::Rejected { intent, reason } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    request = ?intent.request,
                    reason = ?reason,
                    "PreviewScrub rejected before PlayerSession"
                );
                false
            }
        }
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
        if self.scrub_driver.should_ignore_external_seek() {
            debug!("External seek ignored during active scrub");
            return;
        }

        self.dispatch_player_command(PlayerCommand::Seek(request));
    }

    /// Применяет один scrub update после coalescing.
    #[cfg(test)]
    fn apply_scrub_update(&mut self, intent: ScrubUpdateIntent) {
        let decision = self.scrub_driver.apply_scrub_update(intent);
        self.dispatch_scrub_update_decision(ScrubPreviewDispatch::Allow, decision);
    }

    /// Отправляет принятое driver-ом scrub update решение в session.
    fn dispatch_scrub_update_decision(
        &mut self,
        preview_dispatch: ScrubPreviewDispatch,
        decision: ScrubUpdateDecision,
    ) {
        match decision {
            ScrubUpdateDecision::Accepted {
                intent,
                session_command,
                due_preview,
            } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    request = ?intent.request,
                    preview_dispatch = ?preview_dispatch,
                    "Worker применяет scrub update"
                );
                self.dispatch_scrub_command(session_command);
                if let Some(due_preview) = due_preview {
                    let preview_decision = self.scrub_driver.preview_due_scrub(due_preview);
                    self.dispatch_preview_scrub_decision(preview_decision);
                }
            }
            ScrubUpdateDecision::Rejected { intent, reason } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    request = ?intent.request,
                    preview_dispatch = ?preview_dispatch,
                    reason = ?reason,
                    "Scrub update rejected before PlayerSession"
                );
            }
        }
    }

    /// Забирает latest scrub update из bounded канала и применяет только последнюю цель.
    fn apply_latest_scrub_update(&mut self) {
        self.drain_latest_scrub_update_with_preview_dispatch(ScrubPreviewDispatch::Allow);
    }

    /// Забирает latest scrub update перед EndScrub, не создавая новый preview transaction.
    fn apply_latest_scrub_update_for_commit(&mut self) {
        self.drain_latest_scrub_update_with_preview_dispatch(
            ScrubPreviewDispatch::SuppressForCommit,
        );
    }

    /// Общий latest-wins drain для обычного scrub-а и release commit-а.
    fn drain_latest_scrub_update_with_preview_dispatch(
        &mut self,
        preview_dispatch: ScrubPreviewDispatch,
    ) {
        drain_receiver_without_blocking(&self.scrub_wake_rx);

        match self
            .scrub_coalescer
            .take_for_generation(self.scrub_driver.current_generation())
        {
            ScrubCoalescedUpdateDecision::Empty => {}
            ScrubCoalescedUpdateDecision::FutureGeneration { intent } => {
                debug!(
                    generation = intent.generation.as_u64(),
                    request = ?intent.request,
                    preview_dispatch = ?preview_dispatch,
                    "Coalesced scrub update deferred until matching BeginScrub"
                );
            }
            ScrubCoalescedUpdateDecision::Ready { intent } => {
                let decision = match preview_dispatch {
                    ScrubPreviewDispatch::Allow => {
                        self.scrub_driver.apply_latest_scrub_update_for_drag(intent)
                    }
                    ScrubPreviewDispatch::SuppressForCommit => self
                        .scrub_driver
                        .apply_latest_scrub_update_for_commit(intent),
                };
                self.dispatch_scrub_update_decision(preview_dispatch, decision);
                self.publish_session_outputs();
            }
        }
    }

    /// Прерывает scrub для Open/Shutdown/load boundary команд.
    fn interrupt_scrub_for_external_boundary(&mut self) {
        self.scrub_driver.interrupt_scrub();
    }

    /// Отправляет preview seek, когда прошёл throttle interval live scrub-а.
    fn dispatch_due_preview_scrub_seek(&mut self) {
        let now = Instant::now();
        let decision = self.scrub_driver.dispatch_due_preview_scrub_seek(now);
        self.dispatch_preview_scrub_decision(decision);
    }

    /// Безопасно вызывает `PlayerSession::dispatch_command` и сохраняет ошибку в session.
    fn dispatch_player_command(&mut self, command: PlayerCommand) {
        if let Err(error) = self.session.dispatch_command(command) {
            warn!(error = %error, "Player worker command failed");
            self.session.mark_fatal_error(error);
        }
    }

    /// Безопасно вызывает typed scrub boundary на session.
    fn dispatch_scrub_command(&mut self, command: SessionScrubCommand) {
        if let Err(error) = self.session.dispatch_scrub_command(command) {
            warn!(error = %error, "Player worker scrub command failed");
            self.session.mark_fatal_error(error);
        }
    }

    /// Обрабатывает timeout, который не был command/render/scrub event-ом.
    fn handle_worker_timeout(&mut self, deadline: WorkerWakeupDeadline) {
        match deadline {
            WorkerWakeupDeadline::Playback { plan, deadline } => {
                self.run_tick_for_wakeup_plan(plan, deadline);
            }
            WorkerWakeupDeadline::PreviewScrub => {
                self.dispatch_due_preview_scrub_seek();
                self.publish_session_outputs();
            }
        }
    }

    /// Выполняет playback tick по media-clock-driven wakeup plan.
    fn run_tick_for_wakeup_plan(&mut self, plan: PlayerWorkerWakeupPlan, deadline: Instant) {
        let now = Instant::now();
        let tick_late_by = now.saturating_duration_since(deadline);
        self.session
            .record_worker_wakeup(plan.diagnostics(tick_late_by));
        let tick_result = self.session.tick(PlayerTickContext::with_timing(
            now,
            self.config.tick_config,
            tick_late_by,
        ));
        self.last_tick_at = now;
        self.publish_tick_result(tick_result);
        self.log_active_seek_stall_if_needed(now);
        self.log_diagnostics_summary_if_due(now);
        self.publish_session_outputs();
    }

    /// Пишет throttled warn-log, если active seek уже выглядит как зависший transition.
    fn log_active_seek_stall_if_needed(&mut self, now: Instant) {
        let Some(active_seek) = self
            .session
            .active_seek_diagnostics(now, &self.config.tick_config)
        else {
            self.last_seek_stall_log_key = None;
            self.last_seek_stall_log_at = None;
            return;
        };

        let active_seek_key = (active_seek.generation, active_seek.kind);
        if self.last_seek_stall_log_key != Some(active_seek_key) {
            self.last_seek_stall_log_key = Some(active_seek_key);
            self.last_seek_stall_log_at = None;
        }

        let log_after = seek_stall_log_after(active_seek, self.config.tick_config);
        if active_seek.age < log_after {
            return;
        }

        if self.last_seek_stall_log_at.is_some_and(|last_log_at| {
            now.saturating_duration_since(last_log_at) < SEEK_STALL_LOG_INTERVAL
        }) {
            return;
        }

        self.last_seek_stall_log_at = Some(now);
        log_active_seek_stall(active_seek);
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
        let wake_reason = summary
            .worker_wakeup
            .reason
            .map(|reason| reason.metric_name())
            .unwrap_or("none");
        let wake_delay_ms = summary.worker_wakeup.planned_delay.map(duration_to_millis);
        let wake_late_ms = duration_to_millis(summary.worker_wakeup.tick_late_by);
        let pts_target_ms = summary
            .worker_wakeup
            .frame_timing
            .map(|timing| timing.front_frame_delta_from_target_us as f64 / 1000.0);
        let texture_slots = summary.queues.texture_slots;
        let control_channel = summary.queues.decoder_control_channel;
        let latencies = summary.worst_latencies;
        let texture_view_lock_wait = latencies.texture_view_lock_wait;
        let publish_pressure = summary.decoder_frame_publish_pressure;
        debug!(
            drops = summary.drops_total,
            drops_late = summary.drops.late,
            drops_queue = summary.drops.queue_overflow,
            drops_stale_generation = summary.drops.stale_generation,
            drops_seek_preroll = summary.drops.seek_preroll,
            drops_decoder_starvation = summary.drops.decoder_starvation,
            seek_bootstrap_dropped_until_keyframe = summary.seek_bootstrap.dropped_until_keyframe,
            seek_bootstrap_first_accepted_keyframe = ?summary
                .seek_bootstrap
                .first_accepted_keyframe,
            pauses = summary.pauses_total,
            pauses_sync_waiting = summary.pauses.sync_waiting,
            pauses_present_queue = summary.pauses.waiting_for_present_queue,
            pauses_gpu_release = summary.pauses.waiting_for_gpu_release,
            repeated_video_frames = summary.repeated_video_frames,
            texture_view_lock_busy_count = summary.texture_view_lock_busy_count,
            texture_view_previous_frame_reuse_count = summary.texture_view_previous_frame_reuse_count,
            decoder_publish_channel_full_count = publish_pressure.frame_publish_channel_full_count,
            decoder_publish_retry_count = publish_pressure.pending_publish_retry_count,
            decoder_publish_total_ms = duration_to_millis(
                publish_pressure.total_decoded_frame_publish_latency
            ),
            decoder_publish_max_ms = duration_to_millis(
                publish_pressure.max_decoded_frame_publish_latency
            ),
            memory_path = ?summary.zero_copy_memory_path,
            worst_stage,
            worst_latency_ms,
            demux_worst_ms = ?worst_latency_millis(latencies.demux_read),
            decoder_submit_worst_ms = ?worst_latency_millis(latencies.decoder_submit),
            decoder_sync_worst_ms = ?worst_latency_millis(latencies.hardware_sync),
            import_worst_ms = ?worst_latency_millis(latencies.dma_buf_import),
            worker_worst_ms = ?worst_latency_millis(latencies.worker_scheduler),
            render_acquire_worst_ms = ?worst_latency_millis(latencies.render_acquire),
            texture_view_lock_wait_count = texture_view_lock_wait.samples,
            texture_view_lock_wait_avg_ms = duration_to_millis(texture_view_lock_wait.average),
            texture_view_lock_wait_max_ms = ?worst_latency_millis(texture_view_lock_wait),
            gpu_submit_present_worst_ms = ?worst_latency_millis(latencies.gpu_submit_present),
            release_ack_worst_ms = ?worst_latency_millis(latencies.release_acknowledgement),
            wake_reason,
            wake_delay_ms = ?wake_delay_ms,
            wake_late_ms,
            pts_target_ms = ?pts_target_ms,
            pending_video_packets = summary.queues.pending_video_packets,
            present_queue_depth = summary.queues.present_queue_depth,
            decoder_in_flight_packets = summary.queues.decoder_in_flight_packets,
            decoder_control_channel_len = ?control_channel.map(|pressure| pressure.control_channel_len),
            decoder_control_channel_capacity = ?control_channel.map(|pressure| pressure.control_channel_capacity),
            decoder_control_channel_full_count = ?control_channel.map(|pressure| pressure.control_channel_full_count),
            decoder_release_control_send_fail_count = ?control_channel.map(|pressure| pressure.release_control_send_fail_count),
            decoder_flush_control_send_fail_count = ?control_channel.map(|pressure| pressure.flush_control_send_fail_count),
            active_render_leases = summary.queues.active_render_leases,
            texture_in_use = ?texture_slots.map(|slots| slots.in_use),
            texture_capacity = ?texture_slots.map(|slots| slots.capacity),
            texture_free = ?texture_slots.map(|slots| slots.free_surfaces),
            texture_waiting_gpu = ?texture_slots.map(|slots| slots.waiting_gpu_completion),
            imports_created = ?texture_slots.map(|slots| slots.imports_created),
            imports_reused = ?texture_slots.map(|slots| slots.imports_reused),
            imports_replaced = ?texture_slots.map(|slots| slots.imports_replaced),
            import_failures = ?texture_slots.map(|slots| slots.import_failures),
            "Playback diagnostics summary"
        );
    }

    /// Публикует latest snapshot и накопленные session events.
    fn publish_session_outputs(&mut self) {
        self.render_bridge
            .publish_latest_present_frame(&mut self.session);

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

/// Возвращает возраст seek-а, после которого diagnostics warning становится полезным.
fn seek_stall_log_after(
    active_seek: ActiveSeekDiagnosticsSnapshot,
    tick_config: PlayerTickConfig,
) -> Duration {
    if active_seek.kind == "preview" {
        return tick_config
            .seek_preview_timeout
            .min(PREVIEW_SEEK_STALL_LOG_MAX_AFTER);
    }

    tick_config
        .seek_commit_timeout
        .mul_f64(0.05)
        .max(FINAL_SEEK_STALL_LOG_MIN_AFTER)
        .min(FINAL_SEEK_STALL_LOG_MAX_AFTER)
        .min(tick_config.seek_commit_timeout)
}

/// Пишет один structured event, достаточный для локализации active seek blocker-а.
fn log_active_seek_stall(active_seek: ActiveSeekDiagnosticsSnapshot) {
    let queues = active_seek.queues;
    let texture_slots = queues.texture_slots;

    warn!(
        kind = active_seek.kind,
        blocker = %active_seek.blocker.metric_name(),
        blocker_state = ?active_seek.blocker,
        generation = active_seek.generation,
        pipeline_generation = active_seek.pipeline_generation,
        scrub_generation = ?active_seek.scrub_generation,
        age_ms = duration_to_millis(active_seek.age),
        target_ms = duration_to_millis(active_seek.target),
        actual_ms = duration_to_millis(active_seek.actual),
        resume_intent = active_seek.resume_intent,
        video_gate_ready = active_seek.video_gate_ready,
        audio_gate_ready = active_seek.audio_gate_ready,
        target_frame_presented = active_seek.target_frame_presented,
        ready_video_frames = active_seek.ready_video_frames,
        required_video_frames = active_seek.required_video_frames,
        present_frame_pts_ms = ?active_seek.present_frame_pts.map(duration_to_millis),
        front_queued_frame_pts_ms = ?active_seek.front_queued_frame_pts.map(duration_to_millis),
        demuxing_active = active_seek.demuxing_active,
        draining_after_eof = active_seek.draining_after_eof,
        stale_frame = active_seek.stale_frame,
        stale_generation_discards = active_seek.stale_generation_discards,
        seek_bootstrap_dropped_until_keyframe = active_seek
            .seek_bootstrap
            .dropped_until_keyframe,
        seek_bootstrap_first_accepted_keyframe = ?active_seek
            .seek_bootstrap
            .first_accepted_keyframe,
        last_pause_reason = ?active_seek.last_pause_reason,
        pending_audio_packets = queues.pending_audio_packets,
        pending_video_packets = queues.pending_video_packets,
        present_queue_depth = queues.present_queue_depth,
        decoder_send_queue_depth = queues.decoder_send_queue_depth,
        decoder_in_flight_packets = queues.decoder_in_flight_packets,
        active_render_leases = queues.active_render_leases,
        deferred_render_releases = queues.deferred_render_releases,
        texture_capacity = ?texture_slots.map(|slots| slots.capacity),
        texture_in_use = ?texture_slots.map(|slots| slots.in_use),
        texture_available = ?texture_slots.map(|slots| slots.available_slots()),
        texture_free_surfaces = ?texture_slots.map(|slots| slots.free_surfaces),
        texture_waiting_gpu = ?texture_slots.map(|slots| slots.waiting_gpu_completion),
        texture_waiting_decoder_reuse = ?texture_slots.map(|slots| slots.waiting_decoder_reuse),
        texture_import_failures = ?texture_slots.map(|slots| slots.import_failures),
        imports_created = ?texture_slots.map(|slots| slots.imports_created),
        imports_reused = ?texture_slots.map(|slots| slots.imports_reused),
        imports_replaced = ?texture_slots.map(|slots| slots.imports_replaced),
        "Active seek transaction is still waiting"
    );
}

/// Опустошает receiver без ожидания; используется для latest/coalescing каналов.
fn drain_receiver_without_blocking<T>(receiver: &Receiver<T>) {
    while receiver.try_recv().is_ok() {}
}

/// Конвертирует latency в миллисекунды для compact diagnostics logs.
fn duration_to_millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Возвращает worst latency одного stage в миллисекундах, если stage уже видел samples.
fn worst_latency_millis(counter: LatencyCounterSnapshot) -> Option<f64> {
    counter
        .worst
        .map(|sample| duration_to_millis(sample.duration))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use crossbeam_channel::unbounded;
    use media_core::{DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer};
    use video_core::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle};

    use super::*;
    use crate::{MediaSource, PlaybackState, SeekTarget};

    fn worker_config_for_tests() -> PlayerWorkerConfig {
        PlayerWorkerConfig {
            coarse_wakeup_interval: Duration::from_millis(10),
            decoder_readiness_poll_interval: Duration::from_millis(2),
            tick_config: PlayerTickConfig::default(),
            live_scrub_preview_interval: DEFAULT_LIVE_SCRUB_PREVIEW_INTERVAL,
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        }
    }

    fn seek_to_millis(milliseconds: u64) -> SeekRequest {
        SeekRequest::absolute(MediaTime::from_millis(milliseconds))
    }

    fn scrub_update_intent(generation: ScrubGeneration, milliseconds: u64) -> ScrubUpdateIntent {
        ScrubUpdateIntent::new(generation, seek_to_millis(milliseconds))
    }

    /// Fake demuxer для worker-level scrub tests без реального файла и backend resources.
    struct WorkerFakeDemuxer {
        /// Media tracks, которые session увидит после load boundary.
        tracks: Vec<media_core::TrackInfo>,

        /// Длительность нужна timeline-у, чтобы source был seekable.
        duration: Option<Duration>,

        /// Полный log seek request-ов, дошедших до demux boundary.
        seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    }

    impl WorkerFakeDemuxer {
        /// Создаёт seekable fake media без tracks, чтобы тестировать только command flow.
        fn empty_seekable(seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>) -> Self {
            Self {
                tracks: Vec::new(),
                duration: Some(Duration::from_secs(30)),
                seek_request_log,
            }
        }

        /// Записывает seek request и возвращает нейтральный successful seek result.
        fn record_seek_request(
            &mut self,
            request: DemuxSeekRequest,
        ) -> anyhow::Result<DemuxSeekResult> {
            self.seek_request_log
                .lock()
                .expect("worker fake seek request log lock")
                .push(request);

            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(request.timestamp),
                actual_position: MediaTime::from_duration(request.timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    impl Demuxer for WorkerFakeDemuxer {
        fn tracks(&self) -> &[media_core::TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn seekability(&self) -> DemuxSeekability {
            DemuxSeekability::Seekable
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(None)
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            self.record_seek_request(DemuxSeekRequest::accurate(timestamp))
        }

        fn seek_with_request(
            &mut self,
            request: DemuxSeekRequest,
        ) -> anyhow::Result<DemuxSeekResult> {
            self.record_seek_request(request)
        }
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
        runtime_for_tests_with_command_sender(last_tick_at).0
    }

    fn runtime_for_tests_with_command_sender(
        last_tick_at: Instant,
    ) -> (PlayerWorkerRuntime, Sender<WorkerCommand>) {
        let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (scrub_wake_tx, scrub_wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (render_bridge, _render_bridge_client) = RenderLeaseBridge::new();
        let (_shutdown_tx, shutdown_rx) = bounded(1);
        let config = worker_config_for_tests();
        let scrub_coalescer = Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx));

        (
            PlayerWorkerRuntime {
                session: PlayerSession::new(),
                scrub_driver: ScrubDriver::new(config.live_scrub_preview_interval),
                worker_scheduler: WorkerScheduler,
                command_rx,
                scrub_wake_rx,
                scrub_coalescer,
                snapshot_publisher: LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx),
                event_tx,
                render_bridge,
                shutdown_rx,
                config,
                last_tick_at,
                last_diagnostics_summary_at: last_tick_at,
                last_seek_stall_log_key: None,
                last_seek_stall_log_at: None,
            },
            command_tx,
        )
    }

    fn command_sender_for_scrub_path_tests() -> (
        PlayerCommandSender,
        Receiver<WorkerCommand>,
        Receiver<()>,
        Arc<ScrubUpdateCoalescer>,
    ) {
        let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (scrub_wake_tx, scrub_wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let scrub_coalescer = Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx));
        let command_sender = PlayerCommandSender {
            command_tx,
            scrub_coalescer: Arc::clone(&scrub_coalescer),
            scrub_command_sequencer: Arc::new(Mutex::new(ScrubCommandSequencer::new())),
        };

        (command_sender, command_rx, scrub_wake_rx, scrub_coalescer)
    }

    fn receive_worker_player_command(command_rx: &Receiver<WorkerCommand>) -> WorkerPlayerCommand {
        match command_rx.try_recv().unwrap() {
            WorkerCommand::Player(command) => command,
            _ => panic!("ordinary PlayerCommand must use WorkerCommand::Player"),
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
        PresentFrameLease::new_for_tests(
            render_generation,
            decoded_frame_for_tests(texture_handle),
            stale,
            release_tx,
        )
    }

    fn worker_with_latest_handoff_for_tests(
        latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,
    ) -> (
        PlayerWorker,
        Receiver<RenderAcquireSample>,
        Receiver<RenderTimingSample>,
        Receiver<RenderTextureViewPreviousFrameReuseSample>,
    ) {
        let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (scrub_wake_tx, _scrub_wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY);
        let (_snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (_event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (
            render_bridge_client,
            render_acquire_sample_rx,
            render_timing_sample_rx,
            texture_view_previous_frame_reuse_sample_rx,
        ) = RenderLeaseBridgeClient::with_handoff_for_tests(latest_present_frame_handoff);
        let (shutdown_tx, _shutdown_rx) = bounded(1);
        let command_sender = PlayerCommandSender {
            command_tx,
            scrub_coalescer: Arc::new(ScrubUpdateCoalescer::new(scrub_wake_tx)),
            scrub_command_sequencer: Arc::new(Mutex::new(ScrubCommandSequencer::new())),
        };

        (
            PlayerWorker {
                command_sender,
                snapshot_rx,
                cached_snapshot: PlayerSnapshot::empty(),
                event_rx,
                render_bridge_client,
                decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
                shutdown_tx,
                join_handle: None,
            },
            render_acquire_sample_rx,
            render_timing_sample_rx,
            texture_view_previous_frame_reuse_sample_rx,
        )
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
    fn player_worker_exposes_decoder_thread_config_for_backend_factory() {
        let decoder_thread_config = PlayerVideoDecoderThreadConfig {
            packet_channel_frames: 2,
            frame_channel_frames: 3,
            control_channel_frames: 4,
            decoder_ready_queue_frames: 5,
            decoder_surface_pool_frames: 6,
            zero_copy_surface_pool_slots: 7,
            flush_timeout: Duration::from_millis(75),
        };
        let mut config = worker_config_for_tests();
        config.decoder_thread_config = decoder_thread_config;

        let mut worker = PlayerWorker::spawn(config).unwrap();

        assert_eq!(worker.decoder_thread_config(), decoder_thread_config);
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
    fn ordinary_player_commands_classify_as_internal_worker_commands() {
        let open_request =
            MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);
        let seek_request = seek_to_millis(250);

        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::Play).unwrap(),
            WorkerPlayerCommand::Play
        );
        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::Seek(seek_request)).unwrap(),
            WorkerPlayerCommand::Seek(seek_request)
        );
        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::OpenMedia(open_request.clone())).unwrap(),
            WorkerPlayerCommand::OpenMedia(open_request)
        );
    }

    #[test]
    fn begin_scrub_does_not_classify_as_internal_worker_command() {
        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::BeginScrub),
            Err(PlayerCommandScrubRoute::Begin)
        );
    }

    #[test]
    fn update_scrub_does_not_classify_as_internal_worker_command() {
        let request = seek_to_millis(300);

        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::UpdateScrub(request)),
            Err(PlayerCommandScrubRoute::Update(request))
        );
    }

    #[test]
    fn preview_scrub_does_not_classify_as_internal_worker_command() {
        let request = seek_to_millis(400);

        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::PreviewScrub(request)),
            Err(PlayerCommandScrubRoute::Preview(request))
        );
    }

    #[test]
    fn end_scrub_does_not_classify_as_internal_worker_command() {
        assert_eq!(
            WorkerPlayerCommand::try_from(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            }),
            Err(PlayerCommandScrubRoute::End(
                ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE
            ))
        );
    }

    #[test]
    fn command_sender_routes_ordinary_commands_as_internal_worker_player_commands() {
        let (command_sender, command_rx, _scrub_wake_rx, _scrub_coalescer) =
            command_sender_for_scrub_path_tests();
        let open_request =
            MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);
        let seek_request = seek_to_millis(500);

        command_sender.try_send(PlayerCommand::Play).unwrap();
        assert_eq!(
            receive_worker_player_command(&command_rx),
            WorkerPlayerCommand::Play
        );

        command_sender
            .try_send(PlayerCommand::Seek(seek_request))
            .unwrap();
        assert_eq!(
            receive_worker_player_command(&command_rx),
            WorkerPlayerCommand::Seek(seek_request)
        );

        command_sender
            .try_send(PlayerCommand::OpenMedia(open_request.clone()))
            .unwrap();
        assert_eq!(
            receive_worker_player_command(&command_rx),
            WorkerPlayerCommand::OpenMedia(open_request)
        );
    }

    #[test]
    fn command_sender_routes_scrub_commands_through_typed_path() {
        let (command_sender, command_rx, scrub_wake_rx, scrub_coalescer) =
            command_sender_for_scrub_path_tests();

        command_sender.try_send(PlayerCommand::BeginScrub).unwrap();
        let active_generation = match command_rx.try_recv().unwrap() {
            WorkerCommand::BeginScrub { generation } => generation,
            _ => panic!("BeginScrub must use typed worker command"),
        };

        command_sender
            .try_send(PlayerCommand::UpdateScrub(seek_to_millis(250)))
            .unwrap();
        assert!(command_rx.try_recv().is_err());
        assert_eq!(scrub_wake_rx.len(), 1);
        assert_eq!(
            scrub_coalescer.take_for_generation(active_generation),
            ScrubCoalescedUpdateDecision::Ready {
                intent: scrub_update_intent(active_generation, 250),
            }
        );

        command_sender
            .try_send(PlayerCommand::PreviewScrub(seek_to_millis(250)))
            .unwrap();
        match command_rx.try_recv().unwrap() {
            WorkerCommand::PreviewScrub(intent) => {
                assert_eq!(intent, scrub_update_intent(active_generation, 250));
            }
            _ => panic!("PreviewScrub must use typed worker command"),
        }

        command_sender
            .try_send(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();
        match command_rx.try_recv().unwrap() {
            WorkerCommand::EndScrub(intent) => {
                assert_eq!(intent.generation, active_generation);
                assert_eq!(intent.policy, ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE);
            }
            _ => panic!("EndScrub must use typed worker command"),
        }
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
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
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
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
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
    fn begin_scrub_playback_action_still_pauses_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let generation = ScrubGeneration::default().next();

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));
        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Playing
        );

        runtime.handle_begin_scrub_command(generation);

        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Paused
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert_eq!(
            runtime.scrub_driver.resume_intent(),
            PlaybackResumeIntent::Play
        );
    }

    #[test]
    fn active_scrub_consumes_resume_commands_without_session_dispatch() {
        let mut runtime = runtime_for_tests(Instant::now());
        let generation = ScrubGeneration::default().next();

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));
        runtime.handle_begin_scrub_command(generation);

        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Paused
        );
        assert_eq!(
            runtime.scrub_driver.resume_intent(),
            PlaybackResumeIntent::Play
        );

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Pause));
        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Paused
        );
        assert_eq!(
            runtime.scrub_driver.resume_intent(),
            PlaybackResumeIntent::Pause
        );

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));
        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Paused
        );
        assert_eq!(
            runtime.scrub_driver.resume_intent(),
            PlaybackResumeIntent::Play
        );

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::TogglePlayback));
        assert_eq!(
            runtime.session.snapshot().playback_state,
            PlaybackState::Paused
        );
        assert_eq!(
            runtime.scrub_driver.resume_intent(),
            PlaybackResumeIntent::Pause
        );
        assert!(runtime.scrub_driver.is_scrubbing());
    }

    #[test]
    fn ordinary_non_resume_command_dispatches_to_session_during_active_scrub() {
        let mut runtime = runtime_for_tests(Instant::now());
        let generation = ScrubGeneration::default().next();

        runtime.handle_begin_scrub_command(generation);
        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::SetVolume(0.25)));

        assert_eq!(runtime.session.snapshot().volume, 0.25);
        assert!(runtime.scrub_driver.is_scrubbing());
    }

    #[test]
    fn stale_scrub_update_generation_is_ignored_before_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();

        runtime.handle_begin_scrub_command(first_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 100));
        runtime.handle_begin_scrub_command(second_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(second_generation, 250));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 900));

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(250))
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert!(runtime.session.snapshot().last_error.is_none());
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            1
        );
    }

    #[test]
    fn idle_due_preview_scrub_does_not_count_stale_or_dispatch_to_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let generation = ScrubGeneration::default().next();

        runtime.handle_begin_scrub_command(generation);
        let target_before_idle_preview = runtime.session.snapshot().timeline.target_position;
        runtime.dispatch_due_preview_scrub_seek();

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            target_before_idle_preview
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert!(runtime.session.snapshot().last_error.is_none());
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            0
        );
    }

    #[test]
    fn stale_preview_scrub_generation_is_ignored_before_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();

        runtime.handle_begin_scrub_command(first_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 100));
        runtime.handle_begin_scrub_command(second_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(second_generation, 250));
        runtime.handle_preview_scrub_command(scrub_update_intent(first_generation, 100));

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(250))
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert!(runtime.session.snapshot().last_error.is_none());
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            1
        );
    }

    #[test]
    fn stale_end_scrub_generation_does_not_commit_new_timeline() {
        let mut runtime = runtime_for_tests(Instant::now());
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();

        runtime.handle_begin_scrub_command(first_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 100));
        runtime.handle_begin_scrub_command(second_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(second_generation, 250));
        runtime.handle_end_scrub_command(ScrubCommitIntent::new(
            first_generation,
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ));

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(250))
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert_eq!(runtime.session.snapshot().current_position, Duration::ZERO);
        assert!(runtime.session.snapshot().last_error.is_none());
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            1
        );
    }

    #[test]
    fn end_scrub_does_not_drop_coalesced_update_for_queued_new_generation() {
        let mut runtime = runtime_for_tests(Instant::now());
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();

        runtime.handle_begin_scrub_command(first_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 100));
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(second_generation, 250))
            .unwrap();

        runtime.handle_end_scrub_command(ScrubCommitIntent::new(
            first_generation,
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ));
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            0
        );
        runtime.handle_begin_scrub_command(second_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_latest_scrub_update();

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(250))
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
    }

    #[test]
    fn stale_generation_coalesced_update_is_rejected_before_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();

        runtime.handle_begin_scrub_command(first_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(first_generation, 100));
        runtime.handle_begin_scrub_command(second_generation);
        runtime
            .scrub_driver
            .set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));
        runtime.apply_scrub_update(scrub_update_intent(second_generation, 250));
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(first_generation, 900))
            .unwrap();

        runtime.apply_latest_scrub_update();
        runtime.apply_latest_scrub_update();

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(250))
        );
        assert!(runtime.session.snapshot().timeline.scrubbing);
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            1
        );
    }

    #[test]
    fn interrupt_invalidates_active_generation_before_coalesced_update_reaches_session() {
        let mut runtime = runtime_for_tests(Instant::now());
        let generation = ScrubGeneration::default().next();

        runtime.handle_begin_scrub_command(generation);
        let target_before_stale_update = runtime.session.snapshot().timeline.target_position;
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(generation, 900))
            .unwrap();

        runtime.interrupt_scrub_for_external_boundary();
        runtime.apply_latest_scrub_update();

        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            target_before_stale_update
        );
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            1
        );
    }

    #[test]
    fn scrub_classifier_rejection_has_no_worker_runtime_side_effects() {
        let runtime = runtime_for_tests(Instant::now());
        let queued_intent = scrub_update_intent(ScrubGeneration::default().next(), 333);

        runtime
            .scrub_coalescer
            .submit_latest(queued_intent)
            .unwrap();

        assert!(WorkerPlayerCommand::try_from(PlayerCommand::BeginScrub).is_err());
        assert!(
            WorkerPlayerCommand::try_from(PlayerCommand::UpdateScrub(seek_to_millis(100))).is_err()
        );
        assert!(
            WorkerPlayerCommand::try_from(PlayerCommand::PreviewScrub(seek_to_millis(100)))
                .is_err()
        );
        assert!(
            WorkerPlayerCommand::try_from(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .is_err()
        );

        assert!(!runtime.scrub_driver.is_scrubbing());
        assert!(!runtime.session.snapshot().timeline.scrubbing);
        assert_eq!(runtime.session.snapshot().timeline.target_position, None);
        assert!(runtime.session.snapshot().last_error.is_none());
        assert_eq!(
            runtime.scrub_driver.diagnostics().stale_or_ignored_commands,
            0
        );
        assert_eq!(
            runtime
                .scrub_coalescer
                .take_for_generation(queued_intent.generation),
            ScrubCoalescedUpdateDecision::Ready {
                intent: queued_intent,
            }
        );
    }

    #[test]
    fn latest_scrub_update_dispatches_preview_before_release() {
        let mut runtime = runtime_for_tests(Instant::now());
        let seek_request_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = WorkerFakeDemuxer::empty_seekable(Arc::clone(&seek_request_log));
        let generation = ScrubGeneration::default().next();

        runtime.session.load_demuxer_with_autoplay(
            "worker-fake".to_string(),
            Box::new(demuxer),
            false,
        );
        runtime.handle_begin_scrub_command(generation);
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(generation, 900))
            .unwrap();

        runtime.apply_latest_scrub_update();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(
            requests.as_slice(),
            &[DemuxSeekRequest::preview(Duration::from_millis(900))]
        );
    }

    #[test]
    fn end_scrub_applies_coalesced_update_without_starting_release_preview_seek() {
        let mut runtime = runtime_for_tests(Instant::now());
        let seek_request_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = WorkerFakeDemuxer::empty_seekable(Arc::clone(&seek_request_log));
        let generation = ScrubGeneration::default().next();

        runtime.session.load_demuxer_with_autoplay(
            "worker-fake".to_string(),
            Box::new(demuxer),
            false,
        );
        runtime.handle_begin_scrub_command(generation);
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(generation, 900))
            .unwrap();

        runtime.handle_end_scrub_command(ScrubCommitIntent::new(
            generation,
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ));

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(
            requests.as_slice(),
            &[DemuxSeekRequest::accurate(Duration::from_millis(900))]
        );
        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(900))
        );
        assert!(runtime.session.has_active_seek_commit());
        assert!(!runtime.session.snapshot().timeline.scrubbing);
    }

    /// Фиксирует latest-wins contract для плотного scrub burst перед release.
    #[test]
    fn aggressive_scrub_release_commits_only_latest_update() {
        let mut runtime = runtime_for_tests(Instant::now());
        let seek_request_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = WorkerFakeDemuxer::empty_seekable(Arc::clone(&seek_request_log));
        let generation = ScrubGeneration::default().next();

        runtime.session.load_demuxer_with_autoplay(
            "worker-fake".to_string(),
            Box::new(demuxer),
            false,
        );
        runtime.handle_begin_scrub_command(generation);
        for step_index in 0..64u64 {
            let target_millis = 100 + step_index * 25;
            runtime
                .scrub_coalescer
                .submit_latest(scrub_update_intent(generation, target_millis))
                .unwrap();
        }

        runtime.handle_end_scrub_command(ScrubCommitIntent::new(
            generation,
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ));

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(
            requests.as_slice(),
            &[DemuxSeekRequest::accurate(Duration::from_millis(1_675))]
        );
        assert_eq!(
            runtime.session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(1_675))
        );
        assert_eq!(runtime.session.snapshot().current_position, Duration::ZERO);
        assert!(runtime.session.has_active_seek_commit());
        assert!(!runtime.session.snapshot().timeline.scrubbing);
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
    fn idle_worker_has_no_periodic_wakeup_timeout() {
        let runtime = runtime_for_tests(Instant::now());

        assert!(runtime.plan_next_worker_wakeup().is_none());
    }

    #[test]
    fn active_worker_uses_media_plan_as_wakeup_timeout() {
        let mut runtime = runtime_for_tests(Instant::now());

        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));

        assert!(runtime.plan_next_worker_wakeup().is_some());
    }

    #[test]
    fn command_batch_yields_to_overdue_tick_during_command_storm() {
        let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
        runtime.config.coarse_wakeup_interval = Duration::ZERO;
        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));

        for command_index in 0..MAX_COMMANDS_PER_LOOP * 2 {
            command_tx
                .try_send(WorkerCommand::SetSystemCapabilities(
                    SystemCapabilities::empty(command_index as u64),
                ))
                .unwrap();
        }

        let previous_tick_at = runtime.last_tick_at;
        let processed_commands = runtime.drain_pending_command_batch();
        runtime.service_worker_fairness_checkpoint(processed_commands);

        assert_eq!(processed_commands, MAX_COMMANDS_PER_LOOP);
        assert_eq!(runtime.command_rx.len(), MAX_COMMANDS_PER_LOOP);
        assert!(runtime.last_tick_at > previous_tick_at);
    }

    #[test]
    fn command_batch_yields_to_latest_scrub_update_during_command_storm() {
        let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
        let generation = runtime.scrub_driver.next_begin_generation();
        runtime.handle_worker_command(WorkerCommand::BeginScrub { generation });
        runtime
            .scrub_coalescer
            .submit_latest(scrub_update_intent(generation, 750))
            .unwrap();

        for command_index in 0..MAX_COMMANDS_PER_LOOP * 2 {
            command_tx
                .try_send(WorkerCommand::SetSystemCapabilities(
                    SystemCapabilities::empty(command_index as u64),
                ))
                .unwrap();
        }

        let processed_commands = runtime.drain_pending_command_batch();
        runtime.service_worker_fairness_checkpoint(processed_commands);

        assert_eq!(processed_commands, MAX_COMMANDS_PER_LOOP);
        assert_eq!(runtime.command_rx.len(), MAX_COMMANDS_PER_LOOP);
        assert_eq!(
            runtime.scrub_driver.latest_scrub_target(),
            Some(seek_to_millis(750))
        );
    }

    #[test]
    fn render_release_ack_is_drained_before_latest_publish() {
        let mut runtime = runtime_for_tests(Instant::now());
        runtime
            .session
            .register_render_lease(0, video_core::FrameTextureHandle(7));
        runtime
            .render_bridge
            .release_sender_for_tests()
            .try_send(RenderLeaseRelease {
                render_generation: 0,
                texture_handle: video_core::FrameTextureHandle(7),
                texture_provider: None,
                released_at: Instant::now(),
            })
            .unwrap();

        runtime
            .render_bridge
            .publish_latest_present_frame(&mut runtime.session);

        assert_eq!(runtime.session.render_lease_count(), 0);
        assert!(matches!(
            runtime.render_bridge.try_clone_latest_for_tests(),
            LatestPresentFrameAcquire::Empty
        ));
    }

    #[test]
    fn latest_present_frame_handoff_reuses_one_drop_ack_until_replaced() {
        let handoff = LatestPresentFrameHandoff::new();
        let (release_tx, release_rx) = unbounded();
        let first_frame =
            present_frame_lease_for_tests(2, FrameTextureHandle(12), false, release_tx.clone());
        let second_frame =
            present_frame_lease_for_tests(2, FrameTextureHandle(13), false, release_tx);

        handoff.publish(Some(first_frame));
        let first_render_clone = match handoff.try_clone_latest() {
            LatestPresentFrameAcquire::Acquired(frame) => frame,
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
                panic!("latest frame should be available")
            }
        };
        let repeated_render_clone = match handoff.try_clone_latest() {
            LatestPresentFrameAcquire::Acquired(frame) => frame,
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
                panic!("latest frame should be reusable")
            }
        };

        drop(first_render_clone);
        drop(repeated_render_clone);
        assert!(release_rx.try_recv().is_err());

        handoff.publish(Some(second_frame));
        let release = release_rx.try_recv().unwrap();
        assert_eq!(release.render_generation, 2);
        assert_eq!(release.texture_handle, FrameTextureHandle(12));
        assert!(release_rx.try_recv().is_err());
    }

    #[test]
    fn latest_present_frame_handoff_keeps_generation_safe_stale_identity() {
        let handoff = LatestPresentFrameHandoff::new();
        let (release_tx, release_rx) = unbounded();
        let old_generation_frame =
            present_frame_lease_for_tests(4, FrameTextureHandle(31), false, release_tx);

        handoff.publish(Some(old_generation_frame));
        let acquired_frame = match handoff.try_clone_latest() {
            LatestPresentFrameAcquire::Acquired(frame) => frame,
            LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
                panic!("old generation frame should be observable as stale")
            }
        };

        assert!(acquired_frame.stale_for_generation(5));

        drop(acquired_frame);
        handoff.clear();
        let release = release_rx.try_recv().unwrap();
        assert_eq!(release.render_generation, 4);
        assert_eq!(release.texture_handle, FrameTextureHandle(31));
    }

    #[test]
    fn player_worker_try_acquire_present_frame_reads_latest_slot_without_reply_wait() {
        let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
        let (release_tx, _release_rx) = unbounded();
        let expected_texture_handle = FrameTextureHandle(44);
        let frame =
            present_frame_lease_for_tests(3, expected_texture_handle, false, release_tx.clone());
        latest_present_frame_handoff.publish(Some(frame));
        let (
            worker,
            render_acquire_sample_rx,
            _render_timing_sample_rx,
            _texture_view_previous_frame_reuse_sample_rx,
        ) = worker_with_latest_handoff_for_tests(Arc::clone(&latest_present_frame_handoff));

        let acquired_frame = worker.try_acquire_present_frame().unwrap();

        assert_eq!(acquired_frame.render_generation, 3);
        assert_eq!(acquired_frame.texture_handle(), expected_texture_handle);
        assert!(render_acquire_sample_rx.try_recv().is_ok());
    }

    #[test]
    fn player_worker_reports_gpu_submit_present_latency_without_command_queue() {
        let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
        let (
            worker,
            _render_acquire_sample_rx,
            render_timing_sample_rx,
            _texture_view_previous_frame_reuse_sample_rx,
        ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

        worker.report_gpu_submit_present_latency(Duration::from_millis(1));

        let sample = render_timing_sample_rx
            .try_recv()
            .expect("render timing sample should be queued");
        assert_eq!(sample.submit_present_elapsed, Duration::from_millis(1));
    }

    #[test]
    fn player_worker_reports_texture_view_previous_frame_reuse_without_command_queue() {
        let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
        let (
            worker,
            _render_acquire_sample_rx,
            _render_timing_sample_rx,
            texture_view_previous_frame_reuse_sample_rx,
        ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

        worker.report_texture_view_previous_frame_reuse();

        texture_view_previous_frame_reuse_sample_rx
            .try_recv()
            .expect("texture view previous-frame reuse sample should be queued");
    }

    #[test]
    fn tick_runs_while_render_lease_is_active() {
        let mut runtime = runtime_for_tests(Instant::now());
        runtime
            .session
            .register_render_lease(0, video_core::FrameTextureHandle(11));
        runtime.handle_worker_command(WorkerCommand::Player(WorkerPlayerCommand::Play));
        let previous_tick_at = runtime.last_tick_at;
        let plan = runtime.session.worker_wakeup_plan(
            Instant::now(),
            &runtime.config.tick_config,
            runtime.config.decoder_readiness_poll_interval,
            runtime.config.coarse_wakeup_interval,
        );

        runtime.run_tick_for_wakeup_plan(plan, Instant::now());

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
