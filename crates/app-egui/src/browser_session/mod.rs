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
//! В сессии 02 окна нет (решение владельца): главный цикл без winit, ящик будит его
//! через [`inbox::BrowserSessionWake`]. Окно (04) подключится к тому же ящику.
//! Байты сеанса только считаются заглушкой `CountingByteSink` (источник — сессия 03).

mod inbox;
mod normal_player_pause;
mod player_hello;
mod runtime;

use std::process::ExitCode;
use std::sync::Arc;

use browser_bridge_session::{
    BrowserSessionPolicy, BrowserSessionServer, CountingByteSink, CountingSnapshot, EntryDesk,
    OsRandomTokenSource,
};
use desktop_integration::{
    BrowserEntryServiceConfig, BrowserEntryServiceError, start_browser_entry_service,
};
use tracing::{error, info};

use self::inbox::{BrowserSessionInbox, InboxCapacity, ParkingWake};
use self::normal_player_pause::SystemNormalPlayerPause;
use self::runtime::{BrowserSessionRuntime, RuntimeFinish};

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

/// Порядок подъёма важен: сервер и стойка готовы ДО того, как имя появится на шине.
/// Иначе первый звонок посредника (ради которого шина нас и разбудила) пришёл бы в
/// ящик раньше, чем есть кому отвечать.
fn start_and_run(
    policy: BrowserSessionPolicy,
) -> Result<RuntimeFinish, BrowserSessionStartFailure> {
    let player_hello = player_hello::player_hello()?;
    let wake = Arc::new(ParkingWake::default());
    let inbox = Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        wake.clone(),
    ));
    let byte_sink = Arc::new(CountingByteSink::new());
    let server = BrowserSessionServer::start(
        policy,
        Box::new(OsRandomTokenSource),
        byte_sink.clone(),
        inbox.clone(),
    )?;
    let desk = EntryDesk::new(player_hello, server.control(), &policy);
    let entry_service =
        start_browser_entry_service(BrowserEntryServiceConfig::default(), inbox.clone())?;
    info!(port = server.port().get(), "Browser session is listening");

    let runtime = BrowserSessionRuntime::new(
        desk,
        &policy,
        inbox.clone(),
        Box::new(SystemNormalPlayerPause::default()),
    );
    let finish = runtime.run_until_exit(wake.as_ref());

    // Сначала уходим с шины: новый звонок разбудит уже новый процесс, а не попадёт
    // в закрывающийся. Затем закрываем ящик (недоставленные звонки получат
    // `ShuttingDown`) и только потом сервер (оставшиеся соединения — код 4006).
    entry_service.shutdown();
    inbox.close();
    let server_report = server.shutdown();
    let received = ReceivedTotals::of(&byte_sink.snapshot());
    info!(
        ?server_report,
        received_frames = received.frames,
        received_bytes = received.bytes,
        "Browser session stopped; bytes were counted and dropped (source arrives in session 03)"
    );
    Ok(finish)
}

/// Сколько всего байтов дошло до приёмника-заглушки за жизнь процесса.
///
/// Строка лога с этими числами — наблюдаемый итог 02 «байты дошли до приёмника»:
/// по ней сквозной тест сверяет настоящий процесс с записанным потоком.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReceivedTotals {
    frames: u64,
    bytes: u64,
}

impl ReceivedTotals {
    fn of(snapshot: &CountingSnapshot) -> Self {
        snapshot
            .source_buffers
            .values()
            .chain(snapshot.ranges.values())
            .fold(Self::default(), |totals, counters| Self {
                frames: totals.frames.saturating_add(counters.frames),
                bytes: totals.bytes.saturating_add(counters.bytes),
            })
    }
}
