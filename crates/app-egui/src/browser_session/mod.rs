//! Процесс браузерного режима: `fastiplayer --browser-session` (мост в браузер, сессия 02).
//!
//! Его будит D-Bus activation, когда посредник экстеншена звонит на имя
//! `io.github.Bogdan7c.Fastiplayer.BrowserSession`. Процесс — композиция трёх
//! готовых частей, своей бизнес-логики здесь почти нет:
//!
//! ```text
//!  посредник ── D-Bus Entry(json) ──► BrowserSessionInbox ──► главный цикл ──► EntryDesk
//!                (desktop-integration)   (ящик, разбудить)      (этот модуль)     (порт+пропуск)
//!  background ══ WebSocket 127.0.0.1 ══► BrowserSessionServer ──► события сеанса ──► ящик
//! ```
//!
//! Решения владельца, которые держит модуль:
//! - 41 — отдельный процесс; очередь, позицию и track-memory обычного плеера он не
//!   трогает (инвариант 7), lease обычного плеера не берёт (выбор режима — до bootstrap);
//! - 48 — при старте сеанса обычный плеер ставится на паузу (строго его MPRIS, MPRIS
//!   браузеров не трогаем — решение владельца сессии 02) и остаётся на паузе;
//! - 56 — после конца сеанса процесс выходит сразу;
//! - 85 — D-Bus несёт только «вход», всё живое идёт по WebSocket.
//!
//! Окна нет (решения 88/92): главный цикл без winit, ящик будит его через
//! [`inbox::BrowserSessionWake`]. Окно (04) подключится к тому же ящику.
//!
//! С сессии 03b процесс играет поток страницы настоящим плеером:
//!
//! ```text
//!  сервер ══ сообщения MSE + байты (один поток соединения) ══► OrderedSessionQueue
//!     │                                                         (единая очередь)
//!     │ события жизни ─► ящик                                          │
//!     │                                       поток browser-stream: ProtocolTranslator
//!     │                                       → писатель pushed-media-source
//!     │                                                                │ IntakeNotice
//!     ▼                                                                ▼
//!  SessionControl ◄── clock/buffer_state/seek/stream_rejected ── главный цикл:
//!                                                               SessionMedia → BrowserPlayback
//! ```
//!
//! Настройки (`[browser_session]`, видео, звук) читаются один раз при старте и только
//! на чтение: config пишет обычный плеер.

mod inbox;
mod intake_notice;
mod journal;
mod normal_player_pause;
mod outbound;
mod playback;
mod player_hello;
mod rejection;
mod runtime;
mod session_config;
mod session_media;
mod stream_intake;
mod stream_queue;
#[cfg(test)]
mod test_log_capture;
mod translator;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use browser_bridge_session::{
    BrowserSessionPolicy, BrowserSessionServer, EntryDesk, OsRandomTokenSource,
};
use desktop_integration::{
    BrowserEntryServiceConfig, BrowserEntryServiceError, start_browser_entry_service,
};
use pushed_media_source::{BufferWindow, PushedSourcePolicy, PushedSourcePolicyError};
use tracing::{error, info};

use self::inbox::{BrowserSessionInbox, InboxCapacity, ParkingWake};
use self::normal_player_pause::SystemNormalPlayerPause;
use self::outbound::PageOutbound;
use self::playback::metered_audio::BrowserAudioOutputChoice;
use self::playback::video_output::HeadlessVideoOutput;
use self::playback::{
    BrowserPlayback, BrowserPlaybackStartError, decoder_thread_config_from_settings,
};
use self::runtime::{BrowserSessionRuntime, RuntimeFinish};
use self::session_config::BrowserSessionSettings;
use self::session_media::SessionMedia;
use self::stream_intake::StreamIntake;
use self::stream_queue::{OrderedSessionQueue, QueueLimits};
use self::translator::ProtocolTranslator;
use self::translator::stream_port::PushedStreamFactory;

/// Сколько ждать поток приёма при выходе. Разбор одной пачки — миллисекунды;
/// 2 с — с запасом на занятую машину, но выход процесса не висит на зависшем
/// разборе (браузер через ~2–3 с после закрытия порта всё равно убивает посредника).
const INTAKE_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// Итог процесса браузерного режима (для кода выхода). Подробности уже в логе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserSessionProcessOutcome {
    /// Отработал и вышел по своей машине «когда выходить».
    Completed,
    /// Не смог подняться.
    FailedToStart,
}

impl BrowserSessionProcessOutcome {
    /// Код выхода: 0 — штатно, 1 — не поднялся. Окно ошибки не показывается:
    /// процесс запускает шина, а не человек; человеку о сбое скажет экстеншен
    /// (посредник получит ошибку звонка).
    pub(crate) fn exit_code(self) -> ExitCode {
        match self {
            Self::Completed => ExitCode::SUCCESS,
            Self::FailedToStart => ExitCode::FAILURE,
        }
    }
}

/// Почему процесс браузерного режима не поднялся.
#[derive(Debug, thiserror::Error)]
enum BrowserSessionStartFailure {
    /// Не открылся сервер WebSocket на loopback.
    #[error("could not start the browser session server: {0}")]
    SessionServer(#[from] browser_bridge_session::ServerStartError),
    /// Не поднялась служба «входа» на шине (в том числе имя уже занято другим
    /// процессом браузерного режима — при D-Bus activation такого не бывает).
    #[error("could not start the browser session D-Bus service: {0}")]
    EntryService(#[from] BrowserEntryServiceError),
    /// Версия пакета не годится для `hello` протокола (ошибка сборки).
    #[error("the player version is not valid for the bridge protocol: {0}")]
    PlayerHello(#[from] browser_bridge_protocol::ValueViolation),
    /// Пределы источника не собрались (окно буфера из config или ошибка сборки).
    #[error("the browser stream source policy is not valid: {0}")]
    SourcePolicy(#[from] PushedSourcePolicyError),
    /// Плеер не поднялся.
    #[error("the browser session player did not start: {0}")]
    Playback(#[from] BrowserPlaybackStartError),
    /// ОС не дала создать поток приёма байтов.
    #[error("the browser stream intake thread did not start: {0}")]
    IntakeThread(std::io::Error),
}

/// Запускает процесс браузерного режима и ждёт его конца.
pub(crate) fn run_browser_session_process() -> BrowserSessionProcessOutcome {
    info!("=== fastiplayer browser session ===");
    match start_and_run(BrowserSessionPolicy::default()) {
        Ok(finish) => {
            info!(?finish, "Browser session process finished");
            BrowserSessionProcessOutcome::Completed
        }
        Err(failure) => {
            error!(%failure, "Browser session process could not start");
            BrowserSessionProcessOutcome::FailedToStart
        }
    }
}

/// Порядок подъёма важен: плеер, очередь и поток приёма готовы до сервера, а сервер
/// и стойка — ДО того, как имя появится на шине. Иначе первый звонок посредника
/// (ради которого шина нас и разбудила) пришёл бы в ящик раньше, чем есть кому
/// отвечать, а первые байты страницы — раньше, чем есть кому их разбирать.
fn start_and_run(
    policy: BrowserSessionPolicy,
) -> Result<RuntimeFinish, BrowserSessionStartFailure> {
    let player_hello = player_hello::player_hello()?;
    let settings = BrowserSessionSettings::load_read_only();
    info!(origin = ?settings.origin(), "Browser session settings loaded (read-only)");
    let source_policy = pushed_source_policy(&settings)?;
    let wake = Arc::new(ParkingWake::default());
    let playback = BrowserPlayback::start(
        &settings,
        Box::new(HeadlessVideoOutput::new(
            decoder_thread_config_from_settings(&settings),
        )),
        BrowserAudioOutputChoice::from_environment(),
        wake.clone(),
        Instant::now(),
    )?;
    let inbox = Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        wake.clone(),
    ));
    // Одна очередь — оба приёмника сервера: сообщения MSE и байты в порядке кадров.
    let queue = Arc::new(OrderedSessionQueue::new(
        QueueLimits::default(),
        inbox.clone(),
    ));
    let intake = StreamIntake::start(
        queue.clone(),
        inbox.clone(),
        ProtocolTranslator::new(PushedStreamFactory::new(source_policy)),
    )
    .map_err(BrowserSessionStartFailure::IntakeThread)?;
    let server = match BrowserSessionServer::start(
        policy,
        Box::new(OsRandomTokenSource),
        queue.clone(),
        queue.clone(),
    ) {
        Ok(server) => server,
        Err(server_error) => {
            intake.shut_down(INTAKE_SHUTDOWN_BUDGET);
            return Err(server_error.into());
        }
    };
    let control = server.control();
    let desk = EntryDesk::new(player_hello, control.clone(), &policy);
    let entry_service =
        match start_browser_entry_service(BrowserEntryServiceConfig::default(), inbox.clone()) {
            Ok(entry_service) => entry_service,
            Err(entry_error) => {
                server.shutdown();
                intake.shut_down(INTAKE_SHUTDOWN_BUDGET);
                return Err(entry_error.into());
            }
        };
    info!(port = server.port().get(), "Browser session is listening");

    let mut media = SessionMedia::new(
        Box::new(playback),
        Box::new(control.clone()),
        PageOutbound::new(Box::new(control)),
    );
    let runtime = BrowserSessionRuntime::new(
        desk,
        &policy,
        inbox.clone(),
        Box::new(SystemNormalPlayerPause::default()),
    );
    let finish = runtime.run_until_exit(wake.as_ref(), &mut media);

    // Сначала уходим с шины: новый звонок разбудит уже новый процесс, а не попадёт
    // в закрывающийся. Затем закрываем ящик (недоставленные звонки получат
    // `ShuttingDown`), сервер (оставшиеся соединения — код 4006), поток приёма (с
    // потолком ожидания) и только потом плеер — его итоговая строка журнала.
    entry_service.shutdown();
    inbox.close();
    let server_report = server.shutdown();
    let intake_report = intake.shut_down(INTAKE_SHUTDOWN_BUDGET);
    media.finish();
    // Строка с этими числами — наблюдаемый итог «байты дошли до процесса»: по ней
    // сквозной тест сверяет настоящий процесс с записанным потоком.
    let received = queue.counters();
    info!(
        ?server_report,
        ?intake_report,
        received_frames = received.received_frames,
        received_bytes = received.received_bytes,
        "Browser session stopped"
    );
    Ok(finish)
}

/// Пределы источника с окном буфера пользователя (`[browser_session]` config-а).
fn pushed_source_policy(
    settings: &BrowserSessionSettings,
) -> Result<PushedSourcePolicy, PushedSourcePolicyError> {
    let window = settings.buffer_window();
    PushedSourcePolicy::recommended()?.with_buffer_window(BufferWindow {
        ahead: window.ahead(),
        behind: window.behind(),
    })
}
