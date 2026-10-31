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
//! - 56 — после конца сеанса процесс выходит сразу, КРОМЕ `tab_gone` и пропажи экстеншена:
//!   тогда (решение 42, сессия 03c) он доигрывает полученное и выходит по «доиграно»;
//! - 85 — D-Bus несёт только «вход», всё живое идёт по WebSocket.
//!
//! В 02–03 окна не было (решения 88/92): главный цикл без winit, ящик будил его через
//! [`inbox::BrowserSessionWake`]. С 04a видео рисует окно браузерного режима
//! ([`window`]): тот же ящик будит цикл winit, а медиа играет `AppState` окна. Безоконный
//! путь остался только для тестов ([`video_mode`]). Общее ядро обоих —
//! [`process_core::BrowserSessionCore`].
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
//! С сессии 03c страница может объявить поколение «файл» (`<video src>`): плеер читает
//! его как обычную progressive-ссылку, а «диском» служит экстеншен:
//!
//! ```text
//!  страница ══ file_source_added, рамки вида 2, range_fetch_finished ══► единая очередь
//!                                                        │ поток browser-stream
//!                                                        ▼
//!   ProtocolTranslator ─► PushedFileWriter ─► (файл: окно предзагрузки, демуксер) ─► плеер
//!                                                        │ запрос байтов + пробуждение цикла
//!   главный цикл (SessionMedia) ◄── PushedFileRequests ──┘
//!        │ PageOutbound::request_range  (надёжно, не сливается)
//!        ▼
//!   fetch_range ═► страница ═► байты рамками вида 2 ═► ...
//! ```
//!
//! Настройки (`[browser_session]`, видео, звук, окно предзагрузки и повторы файла из
//! `[network]`) читаются один раз при старте и только на чтение: config пишет обычный
//! плеер. Окно браузерного режима меняет их «только на сеанс» (решение 122, 04a).

mod drain_override;
mod inbox;
mod intake_notice;
mod journal;
mod normal_player_pause;
mod opened_page;
mod outbound;
mod playback;
mod player_hello;
mod process_core;
mod range_wake;
mod rejection;
mod rejection_notice;
mod runtime;
mod session_config;
mod session_end;
mod session_media;
mod stream_intake;
mod stream_queue;
#[cfg(test)]
mod test_log_capture;
mod translator;
mod video_mode;
mod window;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use browser_bridge_session::{BrowserSessionPolicy, LifecycleVerdict};
use desktop_integration::BrowserEntryServiceError;
use pushed_media_source::PushedSourcePolicyError;
use tracing::{error, info};

use self::inbox::ParkingWake;
use self::playback::metered_audio::BrowserAudioOutputChoice;
use self::playback::video_output::HeadlessVideoOutput;
use self::playback::{
    BrowserPlayback, BrowserPlaybackStartError, decoder_thread_config_from_settings,
};
use self::process_core::BrowserSessionCore;
use self::runtime::{DRAIN_FALLBACK_WAKE, RuntimeFinish};
use self::session_config::{BrowserSessionSettings, FileSourceSettingsError};
use self::session_media::PausedDrain;
use self::video_mode::BrowserVideoMode;

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
    /// Настройки файла `<video src>` из `[network]` не собрались.
    #[error("the browser file source settings are not valid: {0}")]
    FileSource(#[from] FileSourceSettingsError),
    /// Плеер не поднялся.
    #[error("the browser session player did not start: {0}")]
    Playback(#[from] BrowserPlaybackStartError),
    /// ОС не дала создать поток приёма байтов.
    #[error("the browser stream intake thread did not start: {0}")]
    IntakeThread(std::io::Error),
    /// Окно браузерного режима не поднялось (нет графического сеанса, рендера и т. п.).
    #[error("the browser session window did not start: {0}")]
    Window(#[from] window::BrowserWindowStartError),
}

/// Запускает процесс браузерного режима и ждёт его конца.
pub(crate) fn run_browser_session_process() -> BrowserSessionProcessOutcome {
    info!("=== fastiplayer browser session ===");
    let policy = BrowserSessionPolicy::default();
    let settings = BrowserSessionSettings::load_read_only();
    info!(origin = ?settings.origin(), "Browser session settings loaded (read-only)");
    let result = match BrowserVideoMode::from_environment() {
        BrowserVideoMode::Window => window::run_windowed_browser_session(policy, settings),
        BrowserVideoMode::HeadlessForTests => run_headless(policy, &settings),
    };
    match result {
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

/// Безоконный процесс (тесты): плеер со счётчиком кадров, цикл спит на `ParkingWake`.
fn run_headless(
    policy: BrowserSessionPolicy,
    settings: &BrowserSessionSettings,
) -> Result<RuntimeFinish, BrowserSessionStartFailure> {
    let wake = Arc::new(ParkingWake::default());
    let playback = BrowserPlayback::start(
        settings,
        Box::new(HeadlessVideoOutput::new(
            decoder_thread_config_from_settings(settings),
        )),
        BrowserAudioOutputChoice::from_environment(),
        wake.clone(),
        Instant::now(),
    )?;
    // Без окна нажать «играть» некому: пауза в момент ухода вкладки — выход сразу
    // (правило 103; окно ждёт зрителя — `PausedDrain::WaitForViewer`).
    let mut core = BrowserSessionCore::start(
        policy,
        settings,
        wake.clone(),
        Box::new(playback),
        PausedDrain::ExitAtOnce,
    )?;
    loop {
        match core.step(Instant::now()).verdict {
            // Безоконный плеер принадлежит медиа-части: его итог уже в `media.finish()`.
            LifecycleVerdict::Exit(cause) => return Ok(core.shut_down(cause, || {})),
            LifecycleVerdict::KeepRunning { wake_at } => wake.wait(wake_at),
            // `step` это состояние не отдаёт (см. `BrowserSessionRuntime::step`); если
            // оно когда-нибудь выйдет, цикл не должен ни виснуть, ни крутиться.
            LifecycleVerdict::AwaitPlaybackDrain => {
                wake.wait(Some(Instant::now() + DRAIN_FALLBACK_WAKE));
            }
        }
    }
}
