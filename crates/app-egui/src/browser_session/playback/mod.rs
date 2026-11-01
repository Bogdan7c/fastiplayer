//! Воспроизведение браузерного режима: поток страницы играет НАСТОЯЩИЙ плеер.
//!
//! Решение владельца сессии 03b: процесс браузерного режима (`--browser-session`, без
//! окна — решения 88/92) использует production `PlayerWorker`, собранный тем же кодом,
//! что у обычного плеера (`PlayerWorkerConfig::from_app_config`): тот же планировщик,
//! те же декодеры звука, тот же tempo. Своей логики расписания здесь нет.
//!
//! ```text
//!   страница ─► (источник 03b) ─► PreparedMedia ─► BrowserPlayback::load
//!                                                     │
//!   главный цикл ── pump(now) ◄── wake ◄── PlayerWorker (поток player-worker)
//!        │             ├─ события плеера (запрос декодера, ошибки)
//!        │             ├─ BrowserVideoOutput::present (кадры показа → счёт/рендер)
//!        │             ├─ AudioMeter (записано/прозвучало)
//!        │             └─ PlaybackJournal (сводки в журнал)
//!        └─► PlaybackObservation (позиция, состояние, скорость, конец, отказ)
//! ```
//!
//! Модель потоков: `BrowserPlayback` живёт в главном цикле процесса и не создаёт своих
//! потоков, кроме тех, что поднимает плеер (worker, декодеры, выход звука). Worker будит
//! цикл через тот же [`BrowserSessionWake`], что и ящик звонков (`ParkingWake` в 03b,
//! `EventLoopProxy` окна в 04), — после нового кадра или события плеера. Цикл обязан
//! звать [`BrowserPlayback::pump`] и по таймеру (часы звука и сводки журнала идут сами).
//!
//! С 04a это безоконный (тестовый) плеер: окно браузерного режима играет поток своим
//! `AppState` (`super::window`), а не через [`video_output::BrowserVideoOutput`]. Общий
//! с окном учёт «чья загрузка и чей отказ» — [`load_ledger::LoadLedger`].
//!
//! Решение владельца (99): новое медиа сразу играет, если страница играет; дальше плеер
//! следует за play/pause страницы (команды приходят через `play()`/`pause()`). Как
//! начать загрузку, решает вызывающий ([`LoadStart`]).
//!
//! Чей отказ (ревью 03b, пакет I): плеер один, медиа сменяют друг друга, а снимок и
//! события плеера ещё какое-то время описывают ПРЕЖНЕЕ медиа. Поэтому отказ всегда
//! приписан загрузке ([`LoadFault`]), и только той, чьё медиа его дало: отказ
//! установки — по квитанции установки; `Failed` в снимке и ошибки в событиях — только
//! если их экземпляр медиа (`MediaInstanceId`) — установленный экземпляр ТЕКУЩЕЙ
//! загрузки. Отказ прежнего медиа новой загрузке не достаётся никогда.

pub(crate) mod load_ledger;
mod media_identity;
pub(crate) mod metered_audio;
mod simulated_audio;
pub(crate) mod video_output;

use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{
    AutomaticSurfacePool, CorrelatedPlayerEvent, MediaInstallCompletion, MediaInstallReceipt,
    PlaybackRate, PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent,
    PlayerRuntimeApplyError, PlayerVideoBackendInstallIntent, PlayerVideoDecoderThreadConfig,
    PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent, PlayerWorkerPresentationWake,
    PlayerWorkerSendError, PlayerWorkerShutdownDeadline, PlayerWorkerShutdownOutcome,
    PreparedMedia, SeekRequest, VideoDecodeRequirement,
};
use tracing::{debug, warn};

use self::load_ledger::{CancelledInstall, LoadLedger};
pub(crate) use self::media_identity::{MediaLoadTicket, MediaLoadTicketIssuer};
use self::metered_audio::{AudioMeter, BrowserAudioOutputChoice};
use self::video_output::{
    BrowserVideoOutput, VideoBackendStart, VideoBackendStartError, VideoBackendStartRequest,
};
use super::inbox::BrowserSessionWake;
use super::journal::{
    BROWSER_PLAYBACK_SUMMARY_INTERVAL, JournalNotice, PlaybackJournal, PlaybackSummary,
    SummaryKind, log_playback_ended, log_playback_summary,
};
use super::session_config::BrowserSessionSettings;

/// Сколько ждать завершения потока плеера при выходе процесса.
///
/// Плеер останавливает декодеры и выход звука; на здоровой машине это десятки
/// миллисекунд. 2 с — с запасом на занятый декодер, но процесс не висит, если поток
/// застрял: браузер после закрытия порта убивает посредника через ~2–3 с.
const WORKER_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// Пул поверхностей «авто» для конфигурации декодера.
///
/// Без окна рендера нет; значение важно только аппаратному декоду, которого без окна
/// нет. Окно браузерного режима (04a) берёт пул по семейству своего рендера сам —
/// в `AppState::new`, как обычное окно.
fn automatic_surface_pool() -> AutomaticSurfacePool {
    crate::video_pipeline_candidate::hardware_surface_pool_default_or_wgpu(None)
}

/// Лимиты потока декодера из настроек — тем же отображением, что у worker-а.
pub(crate) fn decoder_thread_config_from_settings(
    settings: &BrowserSessionSettings,
) -> PlayerVideoDecoderThreadConfig {
    PlayerWorkerConfig::decoder_thread_config_from_app_config(
        settings.app_config(),
        automatic_surface_pool(),
    )
}

/// Почему воспроизведение текущего медиа не идёт (то, что увидит страница).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserPlaybackFault {
    /// Видео дорожки не декодируется этим выводом (позже → `stream_rejected{unsupported_codec}`).
    UnsupportedVideo,
    /// Декодер подходил, но не запустился.
    VideoDecoderStartFailed,
    /// Плеер остановился с ошибкой другого рода.
    PlaybackFailed(PlayerErrorKind),
    /// Установку медиа отменили, хотя более новая загрузка её не заменяла (остановка,
    /// структурная отмена до барьера установки, установка пропала без итога): медиа
    /// так и не поставлено.
    InstallAborted,
}

/// Как начинается только что загруженное медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadStart {
    /// Сразу играть (страница играет — решение 99).
    Playing,
    /// Встать на паузу: страница на паузе, звук не должен даже мелькнуть.
    Paused,
}

impl LoadStart {
    /// Флаг автозапуска для плеера.
    const fn autoplay(self) -> bool {
        matches!(self, Self::Playing)
    }
}

/// Сообщил ли плеер об отказе сам — то есть видел ли его уже зритель окна.
///
/// Окно браузерного режима показывает зрителю короткое сообщение при отказе поколения
/// (решение 129). Но часть отказов плеер публикует сам — событием ошибки
/// (`PlayerEvent::RecoverableError`/`FatalError`) или состоянием `Failed` в снимке, — и
/// `AppState` окна уже показал их обычным путём (тост ошибки плеера или ошибка в центре,
/// `state/notifications/player_feed.rs`). Второе сообщение на то же событие не нужно.
/// Отказ установки (итог установки, отмена, отказ начать) плеер ошибкой не публикует —
/// о нём зритель без нашего сообщения ничего не узнает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultAnnouncement {
    /// Плеер сам сообщил об ошибке (событие ошибки или `Failed` в снимке).
    AnnouncedByPlayer,
    /// Отказ установки: плеер его ошибкой не публиковал.
    NotAnnounced,
}

/// Отказ, приписанный конкретной загрузке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadFault {
    /// Чья загрузка отказала.
    pub(crate) load: MediaLoadTicket,
    /// Что случилось.
    pub(crate) fault: BrowserPlaybackFault,
    /// Сообщил ли о нём плеер сам (см. [`FaultAnnouncement`]).
    pub(crate) announcement: FaultAnnouncement,
}

/// Что главный цикл узнал за один [`BrowserPlayback::pump`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlaybackObservation {
    /// Позиция воспроизведения.
    pub(crate) position: Duration,
    /// Длительность, если известна.
    pub(crate) duration: Option<Duration>,
    /// Состояние плеера.
    pub(crate) state: PlaybackState,
    /// Текущая скорость.
    pub(crate) rate: PlaybackRate,
    /// Плеер ждёт данных.
    pub(crate) buffering: bool,
    /// Плеер дошёл до конца медиа.
    pub(crate) ended: bool,
    /// Отказ последней загрузки (держится до следующей). Читать — только через
    /// [`PlaybackObservation::fault_of`]: отказ принадлежит своей загрузке.
    pub(crate) fault: Option<LoadFault>,
    /// Чью загрузку описывает это наблюдение: квитанция загрузки, чей экземпляр
    /// медиа (`MediaInstanceId` плеера) сейчас в снимке. `None` — ни одной: медиа ещё
    /// не загружали, или плеер ещё показывает прежнее, пока ставит новое.
    /// Сравнивать — только через [`PlaybackObservation::shows`].
    pub(crate) shown_load: Option<MediaLoadTicket>,
    /// Цель перемотки, которую плеер сейчас исполняет (`None` — перемотки нет). Позиция
    /// плеера до коммита перемотки — ещё старое место; часы странице показывают цель
    /// ([`PlaybackObservation::page_clock_position`]).
    pub(crate) seek_target: Option<Duration>,
}

impl PlaybackObservation {
    /// Описывает ли наблюдение медиа загрузки `ticket`.
    ///
    /// Только такие позиция и состояние принадлежат поколению потока этой загрузки;
    /// иначе это ещё прежнее медиа (установка асинхронна).
    pub(crate) fn shows(&self, ticket: MediaLoadTicket) -> bool {
        self.shown_load == Some(ticket)
    }

    /// Отказ загрузки `ticket`, если он есть.
    ///
    /// Отказ установки приходит до того, как медиа показано (`shows` ещё ложь), — он
    /// всё равно принадлежит этой загрузке; отказ прежнего медиа — нет.
    pub(crate) fn fault_of(&self, ticket: MediaLoadTicket) -> Option<&BrowserPlaybackFault> {
        self.load_fault_of(ticket)
            .map(|load_fault| &load_fault.fault)
    }

    /// То же, что [`Self::fault_of`], но целиком: с тем, сообщил ли о нём плеер сам.
    pub(crate) fn load_fault_of(&self, ticket: MediaLoadTicket) -> Option<&LoadFault> {
        self.fault
            .as_ref()
            .filter(|load_fault| load_fault.load == ticket)
    }

    /// То, что увидит страница, пока плеер ставит новое медиа: «жду данных» в
    /// позиции 0 с прежней скоростью.
    ///
    /// Почему 0, а не позиция прежнего медиа: она принадлежит другому `MediaSource`.
    /// Новый `MediaSource` на элементе по правилам HTML начинает с `currentTime = 0`;
    /// куда страница перемотает дальше, часы покажут, как только плеер поставит
    /// медиа. Длительность неизвестна, отказа ещё нет (отказ установки придёт
    /// отдельно через [`Self::fault`] и не теряется: здесь он сохраняется).
    pub(crate) fn while_media_installs(&self) -> Self {
        Self {
            position: Duration::ZERO,
            duration: None,
            state: PlaybackState::Opening,
            rate: self.rate,
            buffering: true,
            ended: false,
            fault: self.fault.clone(),
            shown_load: None,
            seek_target: None,
        }
    }

    /// Какую позицию сказать странице в часах (пакет F 04d, решение 150).
    ///
    /// Пока плеер исполняет перемотку (`Seeking`/`Scrubbing` — часы говорят `waiting`),
    /// позиция — ЦЕЛЬ перемотки, как `currentTime` у элемента HTML: он встаёт на новое
    /// место сразу, а `seeking` и `waiting` говорят, что данных там ещё нет. Позиция
    /// плеера в это время — старое место (ядро меняет её только на коммите); увидев её,
    /// страница решила бы, что плеер вернулся назад, и перемоталась бы туда сама. Перемотка
    /// браузерного режима теперь ждёт страницу сколько угодно, так что «старое место в
    /// часах» висело бы всё ожидание. В остальных состояниях — позиция плеера.
    pub(crate) fn page_clock_position(&self) -> Duration {
        match (self.state, self.seek_target) {
            (PlaybackState::Seeking | PlaybackState::Scrubbing, Some(target)) => target,
            _ => self.position,
        }
    }
}

/// Плеер не поднялся.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BrowserPlaybackStartError {
    /// Поток плеера не запустился.
    #[error("the player worker did not start: {0}")]
    Worker(#[from] PlayerError),
    /// Плеер не принял возможности вывода.
    #[error("the player did not accept the output capabilities: {0}")]
    Capabilities(PlayerWorkerSendError),
    /// Декодер по умолчанию не запустился.
    #[error(transparent)]
    VideoBackend(#[from] VideoBackendStartError),
    /// У вывода нет декодера даже для старта.
    #[error("the video output has no default decoder")]
    NoStartupBackend,
    /// Плеер не установил декодер.
    #[error("the player did not install the video decoder: {0:?}")]
    BackendInstall(PlayerRuntimeApplyError),
}

/// Команда не дошла до плеера.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BrowserPlaybackCommandError {
    /// Очередь команд плеера отказала (переполнена или поток плеера завершился).
    #[error("the player did not accept the command: {0}")]
    Player(#[from] PlayerWorkerSendError),
    /// Окно браузерного режима не приняло команду: его очередь команд полна (окно не
    /// исполняет их — зависло) или окно уже закрыто.
    #[error("the browser window did not accept the command: {0}")]
    Window(#[from] super::window::WindowCommandRefusal),
}

/// Мост «плеер показал кадр/событие» → пробуждение главного цикла.
struct PresentationWakeBridge {
    wake: Arc<dyn BrowserSessionWake>,
}

impl PlayerWorkerPresentationWake for PresentationWakeBridge {
    fn wake_presentation(&self) {
        self.wake.wake();
    }
}

/// Воспроизведение потока страницы настоящим плеером.
pub(crate) struct BrowserPlayback {
    /// Production-плеер на своём потоке.
    worker: PlayerWorker,
    /// Вывод видео (безоконный счётчик или окно).
    video_output: Box<dyn BrowserVideoOutput>,
    /// Счётчик звука.
    audio_meter: AudioMeter,
    /// Журнал прогона.
    journal: PlaybackJournal,
    /// Чья загрузка показана и чей отказ (общий учёт с окном браузерного режима).
    ledger: LoadLedger,
    /// Последняя загрузка, итог установки которой ещё не забран.
    pending_install: Option<PendingInstall>,
    /// Выдача квитанций загрузок.
    load_tickets: MediaLoadTicketIssuer,
}

/// Загрузка, ждущая итога установки.
struct PendingInstall {
    /// Квитанция, выданная вызывающему `load`.
    ticket: MediaLoadTicket,
    /// Квитанция установки плеера: источник отказа и `MediaInstanceId`.
    receipt: MediaInstallReceipt,
}

impl BrowserPlayback {
    /// Поднимает плеер и ставит декодер по умолчанию ДО первого медиа.
    pub(crate) fn start(
        settings: &BrowserSessionSettings,
        mut video_output: Box<dyn BrowserVideoOutput>,
        audio: BrowserAudioOutputChoice,
        wake: Arc<dyn BrowserSessionWake>,
        now: Instant,
    ) -> Result<Self, BrowserPlaybackStartError> {
        let (audio_output_factory, audio_meter) =
            audio.build_metered_factory(settings.audio_output_device_id());
        let worker_config =
            PlayerWorkerConfig::from_app_config(settings.app_config(), automatic_surface_pool())
                .with_audio_decoder_factory(Arc::new(
                    audio::ProductionAudioDecoderFactory::default(),
                ))
                .with_audio_output_factory(audio_output_factory)
                .with_audio_tempo_processor_factory(Arc::new(
                    audio_signalsmith::SignalsmithTempoProcessorFactory,
                ))
                .with_presentation_wake(Arc::new(PresentationWakeBridge { wake }));
        let worker = PlayerWorker::spawn(worker_config)?;
        worker
            .set_system_capabilities(video_output.capabilities())
            .map_err(BrowserPlaybackStartError::Capabilities)?;
        match video_output.start_backend(VideoBackendStartRequest::Startup)? {
            VideoBackendStart::Started(backend) => worker
                .set_video_backend(
                    backend,
                    PlayerVideoBackendInstallIntent::PipelineDemand,
                    None,
                )
                .map_err(BrowserPlaybackStartError::BackendInstall)?,
            VideoBackendStart::NoCompatibleBackend => {
                return Err(BrowserPlaybackStartError::NoStartupBackend);
            }
        }
        Ok(Self {
            worker,
            video_output,
            audio_meter,
            journal: PlaybackJournal::new(now, BROWSER_PLAYBACK_SUMMARY_INTERVAL),
            ledger: LoadLedger::default(),
            pending_install: None,
            load_tickets: MediaLoadTicketIssuer::default(),
        })
    }

    /// Загружает новое медиа (новое поколение потока): играет сразу или встаёт на
    /// паузу — по `start`.
    ///
    /// Прежнее медиа плеер закрывает сам; отказ прежнего медиа забывается и новой
    /// загрузке не приписывается.
    /// Возвращает квитанцию загрузки: наблюдения относятся к ней только после
    /// установки (см. [`PlaybackObservation::shows`]).
    pub(crate) fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        // Временный путь установки плеера (compatibility facade): тот же алгоритм strong
        // install внутри worker-а, но без кандидатов и авторизации приложения — их
        // браузерному режиму не из чего строить. См. «открытые вопросы» отчёта 03b.
        let receipt = self
            .worker
            .load_prepared_media(prepared, start.autoplay())?;
        self.journal.note_media_load();
        let ticket = self.load_tickets.issue();
        self.ledger.begin_load(ticket);
        // Итог прежней незавершённой установки больше не нужен: плеер её отменит
        // новой загрузкой, а отказ прежнего медиа уже забыт.
        self.pending_install = Some(PendingInstall { ticket, receipt });
        Ok(ticket)
    }

    /// Страница нажала play.
    pub(crate) fn play(&self) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.worker.try_send_command(PlayerCommand::Play)?)
    }

    /// Страница нажала паузу.
    pub(crate) fn pause(&self) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.worker.try_send_command(PlayerCommand::Pause)?)
    }

    /// Страница сменила скорость.
    pub(crate) fn set_rate(&self, rate: PlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self
            .worker
            .try_send_command(PlayerCommand::SetPlaybackRate(rate))?)
    }

    /// Точная перемотка к `target` (время медиа).
    pub(crate) fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        self.worker
            .try_send_command(PlayerCommand::Seek(SeekRequest::accurate(target)))?;
        self.journal.note_seek_requested();
        Ok(())
    }

    /// Один шаг главного цикла: события плеера, кадры, звук, журнал.
    pub(crate) fn pump(&mut self, now: Instant) -> PlaybackObservation {
        // Итог установки — раньше событий: события только что установленного медиа
        // несут его экземпляр, и связать их с загрузкой можно, только зная его.
        self.drain_install_receipt();
        for event in self.worker.drain_events() {
            if let PlayerWorkerEvent::Player(correlated) = event {
                self.handle_player_event(&correlated);
            }
        }

        let tally = self.video_output.present(&self.worker);
        self.journal.note_taken_frames(tally.taken_frames);
        if let Some(telemetry) = self.worker.take_tick_telemetry() {
            self.journal
                .note_presented_frames(telemetry.video_frames_presented);
        }
        self.journal.note_audio(self.audio_meter.reading());

        let snapshot = self.worker.latest_snapshot();
        // `Failed` прежнего медиа (снимок ещё про него, пока новое ставится) — не
        // отказ текущей загрузки: учёт берёт только снимок её собственного экземпляра.
        self.ledger.note_snapshot(&snapshot);
        let notice = self.journal.note_snapshot(
            snapshot.media_instance_id,
            snapshot.current_position,
            snapshot.playback_state,
        );
        if notice == Some(JournalNotice::PlaybackEnded) {
            log_playback_ended(&self.journal.summary());
        }
        if let Some(summary) = self.journal.take_due_summary(now) {
            log_playback_summary(SummaryKind::Periodic, &summary);
        }

        self.ledger.observation(&snapshot)
    }

    /// Любая команда плееру напрямую — тестам живого скраба окна: в окне команды
    /// скраба идут плееру `AppState` мимо медиа-части, а здесь плеер безоконный.
    #[cfg(test)]
    pub(crate) fn send_command_for_test(
        &self,
        command: PlayerCommand,
    ) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.worker.try_send_command(command)?)
    }

    /// Время показа кадра, который плеер сейчас показывает (тестам живого скраба:
    /// во время скраба позиция плеера стоит, а кадр меняется).
    #[cfg(test)]
    pub(crate) fn shown_frame_pts_for_test(&mut self) -> Option<Duration> {
        self.worker
            .latest_snapshot()
            .current_video_frame
            .as_ref()
            .map(|frame| frame.pts)
    }

    /// Сводка журнала на текущий момент (без записи в журнал).
    #[cfg(test)]
    pub(crate) fn summary(&self) -> PlaybackSummary {
        self.journal.summary()
    }

    /// Завершение: итоговая строка журнала и остановка плеера в пределах бюджета.
    pub(crate) fn finish(mut self) -> PlaybackSummary {
        let summary = self.journal.summary();
        log_playback_summary(SummaryKind::Final, &summary);
        let deadline = PlayerWorkerShutdownDeadline::after(WORKER_SHUTDOWN_BUDGET);
        match self.worker.shutdown_before(deadline) {
            PlayerWorkerShutdownOutcome::TimedOut { .. } => {
                warn!("The browser session player did not stop in time");
            }
            PlayerWorkerShutdownOutcome::ThreadPanicked { .. } => {
                warn!("The browser session player thread panicked while stopping");
            }
            _ => {}
        }
        summary
    }

    /// Реакция на одно событие плеера.
    fn handle_player_event(&mut self, correlated: &CorrelatedPlayerEvent) {
        let event = &correlated.event;
        self.journal.note_player_event(event);
        match event {
            PlayerEvent::VideoBackendSelectionRequested(request)
                if !request.decodable_by_active_backend =>
            {
                self.answer_video_backend_request(&request.requirement);
            }
            PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) => {
                // В журнал — только вид ошибки: текст может нести метку источника.
                warn!(kind = ?error.kind, "Browser session player reported an error");
                // Ошибка прежнего медиа, пришедшая после новой загрузки, текущей не
                // приписывается: её экземпляр — не экземпляр текущей загрузки.
                self.ledger
                    .note_player_error(correlated.media_instance_id, &error.kind);
            }
            _ => {}
        }
    }

    /// Плеер просит декодер под дорожку, которую установленный не тянет.
    ///
    /// Отказ отсюда приписывается ТЕКУЩЕЙ загрузке, без сверки экземпляра: плеер
    /// просит декодер в момент установки, когда событие ещё несёт экземпляр прежнего
    /// медиа (снимок меняется позже), а отказ (`reject_pending_video_backend`)
    /// получает та просьба, что ждёт в плеере сейчас, — просьбу прежнего медиа новая
    /// загрузка уже отменила.
    fn answer_video_backend_request(&mut self, requirement: &VideoDecodeRequirement) {
        let start = self
            .video_output
            .start_backend(VideoBackendStartRequest::Stream(requirement));
        let rejection = match start {
            Ok(VideoBackendStart::Started(backend)) => match self.worker.set_video_backend(
                backend,
                PlayerVideoBackendInstallIntent::PipelineDemand,
                None,
            ) {
                Ok(()) => return,
                Err(error) => {
                    warn!(error = ?error, "The player did not install the requested video decoder");
                    (
                        BrowserPlaybackFault::VideoDecoderStartFailed,
                        "decoder install failed",
                    )
                }
            },
            Ok(VideoBackendStart::NoCompatibleBackend) => {
                warn!(
                    requirement = %requirement.describe(),
                    "Browser session cannot decode this video stream"
                );
                (
                    BrowserPlaybackFault::UnsupportedVideo,
                    "no compatible video decoder in the browser session",
                )
            }
            Err(error) => {
                warn!(error = %error, "Browser session video decoder did not start");
                (
                    BrowserPlaybackFault::VideoDecoderStartFailed,
                    "video decoder did not start",
                )
            }
        };
        let (fault, reason) = rejection;
        self.ledger.note_current_load_fault(fault);
        if let Err(error) = self.worker.reject_pending_video_backend(reason.to_string()) {
            warn!(error = %error, "The player did not receive the video decoder rejection");
        }
    }

    /// Забирает итог установки медиа.
    ///
    /// Отказ установки (например, видео дорожки не показывается этим выводом) приходит
    /// ТОЛЬКО сюда: событий плеера и смены состояния при этом нет — плеер остаётся без
    /// медиа. Поэтому квитанция — источник отказа, а не только диагностика.
    /// Успех установки сообщает `MediaInstanceId` — по нему наблюдения связываются
    /// с квитанцией загрузки.
    fn drain_install_receipt(&mut self) {
        let Some(pending) = self.pending_install.as_ref() else {
            return;
        };
        let Some(completion) = pending.receipt.try_take_completion() else {
            return;
        };
        let ticket = pending.ticket;
        self.pending_install = None;
        match completion {
            MediaInstallCompletion::Failed { failure, .. } => {
                // В журнал — только вид и стадия: текст ошибки может нести метку источника.
                warn!(
                    kind = ?failure.error.kind,
                    stage = ?failure.stage,
                    "Browser session media did not install"
                );
                self.ledger.note_install_failed(ticket, failure.error.kind);
            }
            MediaInstallCompletion::Installed {
                media_instance_id, ..
            } => {
                debug!(completion = ?completion, "Browser session media installed");
                self.ledger.note_installed(ticket, media_instance_id);
            }
            MediaInstallCompletion::Cancelled { .. } => {
                match self.ledger.note_install_cancelled(ticket) {
                    CancelledInstall::ReplacedByNewerLoad => {
                        debug!(completion = ?completion, "Browser session media install cancelled");
                    }
                    CancelledInstall::CurrentLoadAborted => {
                        warn!(completion = ?completion, "Browser session media install was aborted");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
