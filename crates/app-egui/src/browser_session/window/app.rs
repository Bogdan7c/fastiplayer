//! Главный цикл окна браузерного режима (winit) вместо `ParkingWake`.
//!
//! Один проход цикла (`about_to_wait`, после всех событий пачки):
//!
//! ```text
//!  водитель: итог установки → наблюдение (события плеера, кадры, звук, снимок) → стол
//!  окно → ядро: намерения зрителя (кнопки, таймлайн, хоткеи) → медиа-часть сеанса
//!  ядро: шаг (звонки двери, события сеанса, поток приёма, медиа-часть: pump со стола)
//!        └─ открыт первый сеанс → окно появляется и берёт фокус
//!  водитель: команды пульта со стола → AppState (загрузка, play/pause/seek/скорость)
//!  сон: планировщик кадров окна ∧ срок ядра (часы странице, повтор открытия, сроки машины)
//! ```
//!
//! Ядро будит цикл событием `AppWakeOwner::BrowserSession` (ящик, байты, запросы
//! байтов), плеер — событиями показа, как в обычном окне. Звонок посредника получает
//! ответ на ближайшем проходе, даже пока окно рисует: проход не ждёт кадра.
//!
//! Выход — порядок ядра (`BrowserSessionCore::shut_down`): шина, ящик, сервер, поток
//! приёма, итог медиа-части, затем итог журнала окна и остановка плеера окна, потом
//! само окно и цикл.
//!
//! Концы сеанса (04b):
//! - зритель закрыл окно → ядро прощается со страницей (`return_to_page{T, paused}` →
//!   4000) или, если страницы уже нет, считает доигрывание оконченным — и выход сразу;
//! - доигрывание после ухода вкладки кончилось → ядро закрывается сразу (имя уходит с
//!   шины: новый звонок разбудит новый процесс), а окно ещё
//!   [`END_NOTICE_PERIOD`](super::end_notice::END_NOTICE_PERIOD)
//!   показывает последний кадр и сообщение (`end_notice`), потом закрывается;
//! - окно на паузе ждёт человека → постоянное сообщение в центре, процесс жив.
//!
//! Отказ поколения (04b, решение 129): странице ушёл `stream_rejected`, а зритель о нём
//! ещё не знает (`CoreStep::generation_rejection`) → короткий тост
//! (`browser_session::rejection_notice`). Тост, а не центр: центр держит сообщение о
//! конце сеанса. Отказ, о котором плеер сообщил сам, `AppState` уже показал — повтора нет.

use std::time::{Duration, Instant};

use browser_bridge_session::{ExitCause, LifecycleVerdict};
use fastiplayer_config::LoadedConfig;
use tracing::{debug, error, info};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

use super::super::playback::metered_audio::BrowserAudioOutputChoice;
use super::super::process_core::{
    BrowserSessionCore, SessionOpenedDuringStep, player_closed_cause,
};
use super::super::rejection_notice::{
    GenerationRejectionNotice, rejection_log_label, rejection_viewer_message,
};
use super::super::runtime::{DRAIN_FALLBACK_WAKE, RuntimeFinish};
use super::driver::WindowPlaybackDriver;
use super::end_notice::{ShownEndNotice, WindowExit, exit_plan};
use super::instance::{
    BrowserWindowInstance, BrowserWindowStartError, InstanceAudio, InstanceSignal,
    InstanceWakePorts, apply_redraw_action,
};
use super::remote::PlaybackDesk;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakePort, AppWakeProxy};
use crate::process_shutdown::ShutdownDeadline;

/// Сколько ждать плеер, очередь и фоновые задачи окна при выходе.
///
/// Тот же порядок величин, что у обычного окна (`PROCESS_TERMINAL_SHUTDOWN_BUDGET`):
/// здоровая машина закрывается за десятки миллисекунд, потолок — от зависшего драйвера.
const WINDOW_SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

/// Чем кончилась работа цикла.
pub(super) enum WindowRunResult {
    /// Процесс вышел по своей машине (или человек закрыл окно).
    Finished(RuntimeFinish),
    /// Окно не собралось: процесс закрыл сеанс и вышел.
    WindowFailed(BrowserWindowStartError),
}

/// Что выбрал человек или окно, чтобы процесс вышел сам, не дожидаясь машины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForcedExit {
    /// Окно закрыто человеком: ядро уже простилось со страницей (или закончило
    /// доигрывание) и назвало причину выхода.
    ClosedByViewer {
        /// Причина выхода процесса.
        cause: ExitCause,
    },
    /// Окно не собралось.
    WindowFailed,
}

/// Что делать с окном, когда ядро закрывается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowAfterCore {
    /// Закрыть вместе с ядром.
    CloseNow,
    /// Оставить показывать сообщение о конце сеанса (закроется по сроку).
    KeepForNotice,
}

/// Процесс браузерного режима с окном.
pub(super) struct BrowserWindowApp {
    core: Option<BrowserSessionCore>,
    driver: WindowPlaybackDriver,
    desk: PlaybackDesk,
    instance: Option<BrowserWindowInstance>,
    proxy: AppWakeProxy,
    session_wake_port: AppWakePort,
    loaded_config: LoadedConfig,
    audio_choice: BrowserAudioOutputChoice,
    /// Когда разбудить цикл ради ядра (без внешнего повода).
    core_wake_at: Option<Instant>,
    forced_exit: Option<ForcedExit>,
    window_failure: Option<BrowserWindowStartError>,
    finish: Option<RuntimeFinish>,
    /// Сообщение о конце сеанса, которое окно показывает сейчас.
    end_notice: ShownEndNotice,
    /// Ядро уже закрыто, окно показывает сообщение о конце сеанса до этого момента.
    closing_after_notice: Option<Instant>,
}

impl BrowserWindowApp {
    /// Цикл над уже поднятым ядром.
    pub(super) fn new(
        core: BrowserSessionCore,
        driver: WindowPlaybackDriver,
        desk: PlaybackDesk,
        proxy: AppWakeProxy,
        session_wake_port: AppWakePort,
        loaded_config: LoadedConfig,
        audio_choice: BrowserAudioOutputChoice,
    ) -> Self {
        Self {
            core: Some(core),
            driver,
            desk,
            instance: None,
            proxy,
            session_wake_port,
            loaded_config,
            audio_choice,
            core_wake_at: None,
            forced_exit: None,
            window_failure: None,
            finish: None,
            end_notice: ShownEndNotice::default(),
            closing_after_notice: None,
        }
    }

    /// Итог после конца цикла. Если цикл кончился без выхода ядра (ошибка winit),
    /// ядро закрывается здесь — никакой поток и никакое имя на шине не остаются.
    pub(super) fn into_result(mut self) -> WindowRunResult {
        if self.core.is_some() {
            self.shut_down(player_closed_cause());
        }
        self.close_window_after_notice();
        match (self.window_failure, self.finish) {
            (Some(failure), _) => WindowRunResult::WindowFailed(failure),
            (None, Some(finish)) => WindowRunResult::Finished(finish),
            // `shut_down` выше всегда оставляет итог; ветка — страховка без паники.
            (None, None) => WindowRunResult::Finished(RuntimeFinish {
                cause: player_closed_cause(),
            }),
        }
    }

    /// Один проход: водитель → шаг ядра → окно на первом сеансе → команды → сон.
    fn run_pass(&mut self, event_loop: &ActiveEventLoop) {
        let Some(core) = self.core.as_mut() else {
            self.run_notice_pass(event_loop);
            return;
        };
        let now = Instant::now();
        // Снять флаг ДО шага: пробуждение во время шага поднимет его снова.
        self.session_wake_port.clear_pending_for_drain();
        if let Some(instance) = self.instance.as_mut() {
            instance.drain_queue_each_pass();
            let mut player = instance.player();
            self.driver.drive_loads(&mut player);
            self.driver.observe(&mut player, now);
            // Действия зрителя — до шага ядра: команды странице уходят на этом же шаге, а
            // команды плееру водитель исполнит сразу после него.
            core.on_viewer_transport(instance.take_viewer_transport_intents(), now);
        }
        let step = core.step(now);
        if step.session_opened == SessionOpenedDuringStep::Yes && self.instance.is_none() {
            self.open_window(event_loop);
        }
        // Сообщение о конце сеанса — каждый проход (ставится и снимается только при смене).
        if let Some(change) = self.end_notice.update(step.session_end)
            && let Some(instance) = self.instance.as_mut()
        {
            instance.apply_center_status(change);
        }
        // Отказ поколения (04b, решение 129): странице ушёл `stream_rejected` — зрителю
        // тост вместо молча застывшей картинки. Не центр: его держит сообщение о конце.
        if let Some(instance) = self.instance.as_mut() {
            show_generation_rejection(instance, step.generation_rejection);
        }
        if let Some(instance) = self.instance.as_mut() {
            {
                let mut player = instance.player();
                self.driver.execute_commands(&mut player);
                self.driver.drive_loads(&mut player);
            }
            instance.drain_queue_each_pass();
        }
        if let Some(forced) = self.forced_exit {
            // Код закрытия странице уже выбран: `window_event` (или `open_window`) закрыл
            // сеанс кодом «плеер закрыт» (4000, с прощанием `return_to_page` — 04b) ДО
            // этого выхода, а сервер дописывает прощание и кадр закрытия даже при
            // немедленном выключении («первая просьба побеждает», 4006 её не перетирает:
            // тесты `browser-bridge-session` `player_close_followed_by_an_immediate_shutdown_still_sends_4000`
            // и прощания при выключении).
            info!(?forced, "Browser window process exits");
            let cause = match forced {
                ForcedExit::ClosedByViewer { cause } => cause,
                ForcedExit::WindowFailed => player_closed_cause(),
            };
            self.shut_down(cause);
            event_loop.exit();
            return;
        }
        self.core_wake_at = match step.verdict {
            LifecycleVerdict::Exit(cause) => {
                match exit_plan(step.session_end, now) {
                    WindowExit::AfterNotice { close_at } if self.instance.is_some() => {
                        // Доигрывание кончилось: ядро закрывается сразу, окно ещё
                        // показывает последний кадр и сообщение.
                        info!("Browser window shows the end notice before closing");
                        self.shut_down_core(cause, WindowAfterCore::KeepForNotice);
                        self.closing_after_notice = Some(close_at);
                        self.core_wake_at = Some(close_at);
                        self.schedule_next_wake(event_loop);
                    }
                    WindowExit::AfterNotice { .. } | WindowExit::Now => {
                        self.shut_down(cause);
                        event_loop.exit();
                    }
                }
                return;
            }
            LifecycleVerdict::KeepRunning { wake_at } => wake_at,
            // `step` это состояние не отдаёт; страховка от зависания цикла.
            LifecycleVerdict::AwaitPlaybackDrain => Some(now + DRAIN_FALLBACK_WAKE),
        };
        self.schedule_next_wake(event_loop);
    }

    /// Первый сеанс: окно появляется. Не собралось — сеанс закрывается, процесс выходит.
    fn open_window(&mut self, event_loop: &ActiveEventLoop) {
        let created = BrowserWindowInstance::create(
            event_loop,
            self.loaded_config.clone(),
            InstanceWakePorts::from_proxy(&self.proxy),
            InstanceAudio {
                choice: self.audio_choice,
                meter: self.driver.audio_meter(),
            },
        );
        match created {
            Ok(instance) => {
                self.driver.attach_window();
                self.instance = Some(instance);
            }
            Err(failure) => {
                error!(%failure, "The browser window did not start; closing the session");
                if let Some(core) = self.core.as_ref() {
                    core.close_active_session_without_window();
                }
                self.desk.close_window();
                self.window_failure = Some(failure);
                self.forced_exit = Some(ForcedExit::WindowFailed);
            }
        }
    }

    /// Сон: решение планировщика кадров окна, но не позже срока ядра.
    fn schedule_next_wake(&mut self, event_loop: &ActiveEventLoop) {
        match self.instance.as_mut() {
            Some(instance) => {
                let action = instance.before_idle_wait(&self.driver);
                apply_redraw_action(event_loop, instance.window(), action);
            }
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        let merged = merge_wake_deadline(event_loop.control_flow(), self.core_wake_at);
        event_loop.set_control_flow(merged);
    }

    /// Выход в порядке ядра; итог журнала и остановка плеера окна — сразу после итога
    /// медиа-части, затем окно.
    fn shut_down(&mut self, cause: ExitCause) {
        self.shut_down_core(cause, WindowAfterCore::CloseNow);
    }

    /// Закрывает ядро; окно — сразу или после сообщения о конце сеанса.
    ///
    /// Итог журнала окна и стол пульта закрываются вместе с ядром всегда: после выхода
    /// ядра плееру окна больше никто не командует. Если окно остаётся, его `AppState`
    /// живёт до срока сообщения — кадр остаётся на экране.
    fn shut_down_core(&mut self, cause: ExitCause, window: WindowAfterCore) {
        let Some(core) = self.core.take() else {
            return;
        };
        let driver = &self.driver;
        let desk = &self.desk;
        let instance = &mut self.instance;
        let finish = core.shut_down(cause, || {
            let _summary = driver.finish();
            desk.close_window();
            if window == WindowAfterCore::CloseNow
                && let Some(instance) = instance.take()
            {
                instance.shut_down(ShutdownDeadline::after(WINDOW_SHUTDOWN_BUDGET));
            }
        });
        self.finish = Some(finish);
    }

    /// Проход, когда ядро уже закрыто, а окно показывает сообщение о конце сеанса:
    /// окно рисуется до срока, потом закрывается вместе с циклом (раньше — если человек
    /// закрыл его сам).
    fn run_notice_pass(&mut self, event_loop: &ActiveEventLoop) {
        let Some(close_at) = self.closing_after_notice else {
            return;
        };
        let now = Instant::now();
        if self.forced_exit.is_some() || now >= close_at {
            self.close_window_after_notice();
            event_loop.exit();
            return;
        }
        if let Some(instance) = self.instance.as_mut() {
            instance.drain_queue_each_pass();
            // Сеанса больше нет: кнопкам зрителя некому передать волю.
            let ignored = instance.take_viewer_transport_intents();
            if !ignored.is_empty() {
                debug!(
                    ignored = ignored.len(),
                    "Viewer transport ignored: the session has already ended"
                );
            }
        }
        self.core_wake_at = Some(close_at);
        self.schedule_next_wake(event_loop);
    }

    /// Закрывает окно, оставленное ради сообщения о конце сеанса (если оно есть).
    fn close_window_after_notice(&mut self) {
        self.closing_after_notice = None;
        if let Some(instance) = self.instance.take() {
            instance.shut_down(ShutdownDeadline::after(WINDOW_SHUTDOWN_BUDGET));
            info!("Browser window closed after the end notice");
        }
    }
}

impl ApplicationHandler<AppWakeEvent> for BrowserWindowApp {
    /// Окно создаётся не здесь, а на первом сеансе: шина будит процесс раньше сеанса.
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppWakeEvent) {
        match event.owner() {
            // Работу ядра делает проход цикла (`about_to_wait`) сразу после события.
            AppWakeOwner::BrowserSession => {}
            owner => {
                if let Some(instance) = self.instance.as_mut() {
                    instance.on_owner_wake(owner);
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(instance) = self.instance.as_mut() else {
            return;
        };
        if instance.on_window_event(event_loop, event) == InstanceSignal::CloseRequested
            && self.forced_exit.is_none()
        {
            // 04b: сеанс шёл — прощание `return_to_page{T, paused}` и 4000; страница уже
            // ушла (доигрываем или ждём зрителя) — «доиграно». Ядро уже закрыто (окно
            // показывает сообщение о конце) — просто закрыть окно.
            let cause = match self.core.as_mut() {
                Some(core) => core.close_for_viewer(Instant::now()),
                None => player_closed_cause(),
            };
            self.forced_exit = Some(ForcedExit::ClosedByViewer { cause });
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.run_pass(event_loop);
    }

    /// Цикл кончается не по нашему выходу (система закрыла сеанс): закрываем всё.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.core.is_some() {
            self.shut_down(player_closed_cause());
        }
        self.close_window_after_notice();
    }
}

/// Показывает зрителю отказ поколения (если за шаг он был) и пишет одну строку журнала
/// без данных страницы — её читают сквозные тесты.
fn show_generation_rejection(
    instance: &mut BrowserWindowInstance,
    notice: GenerationRejectionNotice,
) {
    let GenerationRejectionNotice::Rejected(reason) = notice else {
        return;
    };
    info!(
        reason = %rejection_log_label(reason),
        "Browser window shows the stream rejection notice"
    );
    instance.notify_viewer(rejection_viewer_message(reason));
}

/// Режим ожидания winit, не позже срока ядра.
fn merge_wake_deadline(flow: ControlFlow, deadline: Option<Instant>) -> ControlFlow {
    match (flow, deadline) {
        (flow, None) | (flow @ ControlFlow::Poll, _) => flow,
        (ControlFlow::Wait, Some(deadline)) => ControlFlow::WaitUntil(deadline),
        (ControlFlow::WaitUntil(current), Some(deadline)) => {
            ControlFlow::WaitUntil(current.min(deadline))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_deadline_never_extends_the_window_wait() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(10);
        let later = now + Duration::from_secs(1);
        assert_eq!(
            merge_wake_deadline(ControlFlow::Wait, None),
            ControlFlow::Wait
        );
        assert_eq!(
            merge_wake_deadline(ControlFlow::Wait, Some(soon)),
            ControlFlow::WaitUntil(soon)
        );
        assert_eq!(
            merge_wake_deadline(ControlFlow::WaitUntil(later), Some(soon)),
            ControlFlow::WaitUntil(soon)
        );
        assert_eq!(
            merge_wake_deadline(ControlFlow::WaitUntil(soon), Some(later)),
            ControlFlow::WaitUntil(soon)
        );
        assert_eq!(
            merge_wake_deadline(ControlFlow::Poll, Some(later)),
            ControlFlow::Poll
        );
    }
}
