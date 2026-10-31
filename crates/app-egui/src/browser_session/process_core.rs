//! Общее ядро процесса браузерного режима: подъём, шаг главного цикла, выход.
//!
//! Процесс бывает двух видов (решения 88, 120, 123):
//! - безоконный — тестовый: главный цикл спит на [`super::inbox::ParkingWake`], плеер —
//!   [`super::playback::BrowserPlayback`] со счётчиком кадров вместо рендера;
//! - оконный (с 04a) — главный цикл winit, плеер — `AppState` окна через «пульт».
//!
//! Различаются они только тем, кто будит цикл и кто играет медиа. Всё остальное —
//! дверь D-Bus, сервер WebSocket, ящик, единая очередь, поток приёма, главный цикл
//! [`BrowserSessionRuntime`] и медиа-часть [`SessionMedia`] — живёт здесь, в одном
//! экземпляре кода для обоих видов. Порядок подъёма и выхода — тоже.
//!
//! Ядро не знает ни winit, ни `ParkingWake`: его гоняют снаружи по шагу
//! ([`BrowserSessionCore::step`]), а когда проснуться, решает вызывающий цикл.

use std::sync::Arc;
use std::time::{Duration, Instant};

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_session::{
    BrowserSessionPolicy, BrowserSessionServer, EntryDesk, ExitCause, LifecycleVerdict,
    OsRandomTokenSource, SessionControl, SessionEndReason, SessionRef,
};
use desktop_integration::{
    BrowserEntryService, BrowserEntryServiceConfig, start_browser_entry_service,
};
use pushed_media_source::{BufferWindow, PushedSourcePolicy, PushedSourcePolicyError};
use tracing::{debug, info};

use super::BrowserSessionStartFailure;
use super::inbox::{BrowserSessionInbox, BrowserSessionWake, InboxCapacity};
use super::intake_notice::IntakeNotice;
use super::normal_player_pause::SystemNormalPlayerPause;
use super::outbound::PageOutbound;
use super::range_wake::RangeRequestWake;
use super::runtime::{BrowserSessionRuntime, RuntimeFinish, SessionMediaPort};
use super::session_config::BrowserSessionSettings;
use super::session_end::SessionEndView;
use super::session_media::{
    DrainProgress, MediaPlayback, PausedDrain, SessionMedia, ViewerCloseStep,
};
use super::stream_intake::StreamIntake;
use super::stream_queue::{OrderedSessionQueue, QueueLimits};
use super::translator::ProtocolTranslator;
use super::translator::file_port::PushedFileFactory;
use super::translator::stream_port::PushedStreamFactory;
use crate::viewer_transport::ViewerTransportIntent;

/// Сколько ждать поток приёма при выходе. Разбор одной пачки — миллисекунды;
/// 2 с — с запасом на занятую машину, но выход процесса не висит на зависшем
/// разборе (браузер через ~2–3 с после закрытия порта всё равно убивает посредника).
const INTAKE_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// Итог одного шага ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CoreStep {
    /// Что решила машина «когда выходить» (с учётом сроков медиа-части).
    pub(super) verdict: LifecycleVerdict,
    /// За шаг открыт сеанс (новый, заменивший или повтор): окну пора появиться.
    pub(super) session_opened: SessionOpenedDuringStep,
    /// Что сказать зрителю о конце сеанса (04b): окно показывает сообщение, безоконный
    /// процесс его не читает.
    pub(super) session_end: SessionEndView,
}

/// Открывался ли за шаг сеанс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionOpenedDuringStep {
    /// Нет.
    No,
    /// Да (хотя бы один раз).
    Yes,
}

/// Ядро процесса браузерного режима (всё, кроме главного цикла и плеера).
pub(super) struct BrowserSessionCore {
    runtime: BrowserSessionRuntime,
    media: SessionMedia,
    inbox: Arc<BrowserSessionInbox>,
    queue: Arc<OrderedSessionQueue>,
    intake: StreamIntake,
    server: BrowserSessionServer,
    entry_service: BrowserEntryService,
    control: SessionControl,
}

impl BrowserSessionCore {
    /// Поднимает ядро вокруг уже готового плеера.
    ///
    /// Порядок подъёма важен: плеер, очередь и поток приёма готовы до сервера, а сервер
    /// и стойка — ДО того, как имя появится на шине. Иначе первый звонок посредника
    /// (ради которого шина нас и разбудила) пришёл бы в ящик раньше, чем есть кому
    /// отвечать, а первые байты страницы — раньше, чем есть кому их разбирать.
    pub(super) fn start(
        policy: BrowserSessionPolicy,
        settings: &BrowserSessionSettings,
        wake: Arc<dyn BrowserSessionWake>,
        playback: Box<dyn MediaPlayback>,
        paused_drain: PausedDrain,
    ) -> Result<Self, BrowserSessionStartFailure> {
        let player_hello = super::player_hello::player_hello()?;
        let source_policy = pushed_source_policy(settings)?;
        // Окно предзагрузки и повторы файла `<video src>` — из тех же `[network]`, что у ссылок.
        let file_source = settings.file_source()?;
        let file_policy = file_source.policy()?;
        info!(
            window_bytes = file_source.prefetch().window_bytes(),
            chunk_bytes = file_source.prefetch().chunk_bytes(),
            reconnect_budget_ms =
                u64::try_from(file_source.reconnect_budget().as_millis()).unwrap_or(u64::MAX),
            "Browser file source settings"
        );
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
            ProtocolTranslator::new(
                PushedStreamFactory::new(source_policy),
                // Запрос байтов будит главный цикл сразу, а не по тику часов.
                Box::new(PushedFileFactory::new(
                    file_policy,
                    Arc::new(RangeRequestWake::new(wake)),
                )),
            ),
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
        let entry_service = match start_browser_entry_service(
            BrowserEntryServiceConfig::default(),
            inbox.clone(),
        ) {
            Ok(entry_service) => entry_service,
            Err(entry_error) => {
                server.shutdown();
                intake.shut_down(INTAKE_SHUTDOWN_BUDGET);
                return Err(entry_error.into());
            }
        };
        info!(port = server.port().get(), "Browser session is listening");

        // Правило паузы при доигрывании — от вида процесса: окно ждёт зрителя,
        // безоконный выходит сразу (`PausedDrain`).
        let media = SessionMedia::new(
            playback,
            Box::new(control.clone()),
            PageOutbound::new(Box::new(control.clone())),
        )
        .with_drain_policy(super::drain_override::drain_policy_from_environment(
            paused_drain,
        ));
        let runtime = BrowserSessionRuntime::new(
            desk,
            &policy,
            inbox.clone(),
            Box::new(SystemNormalPlayerPause::default()),
        );
        Ok(Self {
            runtime,
            media,
            inbox,
            queue,
            intake,
            server,
            entry_service,
            control,
        })
    }

    /// Один шаг главного цикла (не блокирует): звонки, события, медиа, вердикт.
    pub(super) fn step(&mut self, now: Instant) -> CoreStep {
        let mut observed = SessionOpenObserver {
            media: &mut self.media,
            opened: SessionOpenedDuringStep::No,
        };
        let verdict = self.runtime.step(now, &mut observed);
        CoreStep {
            verdict,
            session_opened: observed.opened,
            session_end: self.media.session_end_view(),
        }
    }

    /// Действия зрителя в окне (транспорт): медиа-часть исполняет их на плеере и
    /// передаёт странице (`session_media::viewer_transport`). Зовётся перед шагом ядра,
    /// чтобы команды странице ушли на этом же шаге.
    pub(super) fn on_viewer_transport(
        &mut self,
        intents: Vec<ViewerTransportIntent>,
        now: Instant,
    ) {
        for intent in intents {
            self.media.on_viewer_transport(intent, now);
        }
    }

    /// Окно не собралось: активный сеанс закрывается кодом «плеер закрыт» без прощания
    /// (зритель видео не видел, позиции нет), процесс выходит.
    pub(super) fn close_active_session_without_window(&self) {
        self.control
            .close_active_session(SessionCloseReason::PlayerClosed);
    }

    /// Человек закрыл окно (04b). Возвращает причину выхода процесса:
    /// - сеанс шёл → странице прощание `return_to_page{T, paused}` и код 4000
    ///   (`SessionMedia::close_for_viewer`), причина — «плеер закрыт»;
    /// - страница уже ушла (доигрываем, в том числе окно на паузе ждёт человека) →
    ///   прощаться не с кем, закрытие = «доиграно» (`on_playback_drained`), причина —
    ///   конец сеанса, который доигрывался.
    pub(super) fn close_for_viewer(&mut self, now: Instant) -> ExitCause {
        match self.media.close_for_viewer() {
            ViewerCloseStep::SessionClosed => player_closed_cause(),
            ViewerCloseStep::NoSession => {
                // Страховка: окно без сеанса не открывается, но если вдруг — закрыть
                // активный, если он есть, как до 04b.
                self.close_active_session_without_window();
                player_closed_cause()
            }
            ViewerCloseStep::DrainFinishedByViewer => {
                match self.runtime.finish_drain_for_viewer(now) {
                    LifecycleVerdict::Exit(cause) => cause,
                    other => {
                        // Медиа-часть доигрывала, а машина — нет (сеанс кончился концом
                        // без доигрывания): выход всё равно решён закрытием окна.
                        debug!(
                            ?other,
                            "Viewer closed the window outside of a playback drain"
                        );
                        player_closed_cause()
                    }
                }
            }
        }
    }

    /// Выход в правильном порядке; медиа-часть пишет итог последней.
    ///
    /// Сначала уходим с шины: новый звонок разбудит уже новый процесс, а не попадёт
    /// в закрывающийся. Затем закрываем ящик (недоставленные звонки получат
    /// `ShuttingDown`), сервер (оставшиеся соединения — код 4006), поток приёма (с
    /// потолком ожидания) и только потом плеер — его итоговая строка журнала.
    ///
    /// `after_media_finish` — для плеера, которым медиа-часть не владеет (окно: итог
    /// журнала и остановка `AppState`); зовётся сразу после итога медиа-части, чтобы
    /// порядок строк журнала был тем же, что у безоконного процесса.
    pub(super) fn shut_down(
        self,
        cause: ExitCause,
        after_media_finish: impl FnOnce(),
    ) -> RuntimeFinish {
        let finish = self.runtime.finish(cause);
        self.entry_service.shutdown();
        self.inbox.close();
        let server_report = self.server.shutdown();
        let intake_report = self.intake.shut_down(INTAKE_SHUTDOWN_BUDGET);
        self.media.finish();
        after_media_finish();
        // Строка с этими числами — наблюдаемый итог «байты дошли до процесса»: по ней
        // сквозной тест сверяет настоящий процесс с записанным потоком.
        let received = self.queue.counters();
        info!(
            ?server_report,
            ?intake_report,
            received_frames = received.received_frames,
            received_bytes = received.received_bytes,
            "Browser session stopped"
        );
        finish
    }
}

/// Причина выхода, когда процесс закрывает сеанс сам (окно закрыто, ошибка цикла).
pub(super) fn player_closed_cause() -> ExitCause {
    ExitCause::SessionEnded(SessionEndReason::ClosedByPlayer(
        SessionCloseReason::PlayerClosed,
    ))
}

/// Медиа-часть глазами цикла + отметка «за шаг открыт сеанс» (окно появляется на
/// первом сеансе, а не при старте процесса: шина будит процесс раньше сеанса).
struct SessionOpenObserver<'media> {
    media: &'media mut SessionMedia,
    opened: SessionOpenedDuringStep,
}

impl SessionMediaPort for SessionOpenObserver<'_> {
    fn bid_farewell_to_replaced(&mut self, replaced: &SessionRef) {
        self.media.bid_farewell_to_replaced(replaced);
    }

    fn on_session_opened(&mut self, session: &SessionRef) {
        self.opened = SessionOpenedDuringStep::Yes;
        self.media.on_session_opened(session);
    }

    fn on_session_ended(&mut self, session: &SessionRef, reason: SessionEndReason) {
        self.media.on_session_ended(session, reason);
    }

    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>) {
        self.media.on_intake(notices, bytes_appended);
    }

    fn drive(&mut self, now: Instant) -> Option<Instant> {
        self.media.drive(now)
    }

    fn playback_drain(&mut self, now: Instant) -> DrainProgress {
        self.media.playback_drain(now)
    }
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
