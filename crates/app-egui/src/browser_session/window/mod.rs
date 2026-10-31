//! Окно браузерного режима (мост, сессия 04a): видео потока страницы рисует рендер
//! обычного плеера.
//!
//! Решение 120: окно — режим обычного окна плеера, собранный из тех же деталей
//! (`AppState`, `RenderShell`, `SettingsRuntime`, `PlaylistRuntime`): VA-API без копий,
//! 10 бит, HW→SW, субтитры, цвет — без копий кода. Сеанс ведёт плеер `AppState` через
//! шов `MediaPlayback` («пульт», [`remote`]).
//!
//! ```text
//!  BrowserSessionCore (дверь, сервер, ящик, поток приёма, SessionMedia)
//!        │ MediaPlayback                                   ▲ будит: AppWakeOwner::BrowserSession
//!        ▼                                                 │
//!  WindowRemote ─► стол ◄─ WindowPlaybackDriver ─► WindowPlayer = AppState окна
//!                                  (журнал, учёт загрузок)      (BrowserWindowInstance)
//!  цикл winit: BrowserWindowApp (app.rs)
//! ```
//!
//! Обратный путь «окно → сеанс» (04a, транспорт ведёт страницу): кнопки, таймлайн и
//! хоткеи окна идут в единую точку транспорта `AppState::send_transport_command`; в этом
//! окне (`WindowMode::browser_session`) она не командует плееру, а кладёт намерение
//! зрителя в ящик `AppState`. Каждый проход цикл забирает ящик
//! (`BrowserWindowInstance::take_viewer_transport_intents`) и до шага ядра отдаёт его
//! медиа-части (`BrowserSessionCore::on_viewer_transport` →
//! `SessionMedia::on_viewer_transport`): та обновляет волю страницы, командует плееру
//! через пульт (стол → водитель → `AppState`, в том же проходе) и ставит команду странице.
//!
//! Модули:
//! - [`app`] — цикл winit: проход «водитель → ядро → окно → команды → сон», выход;
//! - [`geometry`] — размер и фуллскрин окна: открыться запомненными, сообщить об изменении (04c);
//! - [`instance`] — экземпляр окна (решение 121: один источник, не одиночка);
//! - [`remote`] — пульт и стол (схема владения `AppState` без делёжки);
//! - [`desktop_transport`] — MPRIS окна `…fastiplayer.browser` (04c): команды виджета →
//!   команды окна, публикация статуса;
//! - [`driver`] — исполнение команд, «новее заменяет», наблюдение, журнал;
//! - [`end_notice`] — конец сеанса в окне: сообщение и закрытие после него (04b);
//! - [`player_port`] — настоящий плеер окна: путь открытия `AppState`;
//! - [`settings`] — config окна: общая доска с правом записи по вердикту о файле (135);
//! - [`wake`] — пробуждение цикла ящиком сеанса.

mod app;
mod desktop_transport;
mod driver;
mod end_notice;
mod geometry;
mod instance;
mod player_port;
mod remote;
mod settings;
#[cfg(test)]
mod test_support;
mod wake;

use std::sync::Arc;
use std::time::Instant;

use browser_bridge_session::BrowserSessionPolicy;
use winit::event_loop::{ControlFlow, EventLoop};

use self::app::{BrowserWindowApp, WindowRunResult};
use self::driver::WindowPlaybackDriver;
pub(crate) use self::instance::BrowserWindowStartError;
pub(crate) use self::remote::WindowCommandRefusal;
use self::remote::{PlaybackDesk, WindowRemote};
use self::wake::EventLoopSessionWake;
use super::BrowserSessionStartFailure;
use super::playback::metered_audio::{AudioMeter, BrowserAudioOutputChoice};
use super::process_core::BrowserSessionCore;
use super::runtime::RuntimeFinish;
use super::session_config::BrowserSessionSettings;
use super::session_media::PausedDrain;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakeProxy};

/// Процесс браузерного режима с окном: цикл winit, окно на первом сеансе.
///
/// Без графического сеанса процесс не поднимается (ошибка старта, код выхода 1):
/// посредник получит отказ звонка, и страница останется со своим видео.
pub(super) fn run_windowed_browser_session(
    policy: BrowserSessionPolicy,
    settings: BrowserSessionSettings,
) -> Result<RuntimeFinish, BrowserSessionStartFailure> {
    let event_loop = EventLoop::<AppWakeEvent>::with_user_event()
        .build()
        .map_err(|error| BrowserWindowStartError::EventLoop(error.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = AppWakeProxy::new(event_loop.create_proxy());
    let session_wake_port = proxy.port(AppWakeOwner::BrowserSession);

    let desk = PlaybackDesk::new();
    let driver = WindowPlaybackDriver::new(desk.clone(), AudioMeter::default(), Instant::now());
    let core = BrowserSessionCore::start(
        policy,
        &settings,
        Arc::new(EventLoopSessionWake::new(session_wake_port.clone())),
        Box::new(WindowRemote::new(desk.clone())),
        // Окно на паузе ждёт человека, а не закрывается (04b, замена правила 103).
        PausedDrain::WaitForViewer,
    )?;
    let mut app = BrowserWindowApp::new(
        core,
        driver,
        desk,
        proxy,
        session_wake_port,
        settings::browser_window_loaded_config(&settings),
        BrowserAudioOutputChoice::from_environment(),
    );
    if let Err(error) = event_loop.run_app(&mut app) {
        tracing::error!(%error, "The browser window event loop failed");
    }
    match app.into_result() {
        WindowRunResult::Finished(finish) => Ok(finish),
        WindowRunResult::WindowFailed(failure) => Err(failure.into()),
    }
}
