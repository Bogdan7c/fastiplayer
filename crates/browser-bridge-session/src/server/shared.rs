//! Общее состояние сервера: единственный замок над реестром сеансов и таблицей
//! соединений, плюс счётчики потерь.
//!
//! Правила замка:
//! - всё, что меняет реестр или таблицу соединений, и публикация событий жизни
//!   сеанса идут под одним замком, поэтому порядок событий равен порядку переходов;
//! - байты и сообщения страницы публикуются ВНЕ замка состояния (их много, порядок
//!   внутри соединения даёт его поток), но через `DeliveryGate`: конец сеанса
//!   закрывает калитку раньше, чем зовёт `session_ended`;
//! - отравление замка снимаем (`PoisonError::into_inner`): критические секции
//!   только переключают поля и зовут приёмники ПОСЛЕ переключения, так что
//!   паника приёмника не оставляет реестр наполовину изменённым.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use browser_bridge_protocol::{
    LoopbackPort, OpenSessionRequest, PlayerSessionMessage, SessionCloseReason, SessionKey,
    SessionToken, encode_message,
};

use super::control::{CloseOutcome, OpenSessionError, OpenedSession, SendToPageError};
use super::report::ServerShutdownReport;
use crate::policy::BrowserSessionPolicy;
use crate::registry::{
    AttachRejection, ConnectionId, RegistryTimings, SessionRef, SessionRegistry,
};
use crate::sink::{SessionByteSink, SessionEndReason, SessionEventSink, SessionLifecycleEvent};
use crate::token::SessionTokenSource;

/// Просьба закрыть соединение: «кто первый, тот и прав». Код хранится числом
/// (0 — просьбы нет), поэтому поток соединения читает её без замка.
#[derive(Debug, Default)]
pub(super) struct CloseRequest {
    code: AtomicU16,
}

impl CloseRequest {
    /// Просит закрыть соединение этим кодом; повторные просьбы не перетирают первую.
    pub(super) fn request(&self, reason: SessionCloseReason) {
        let _ =
            self.code
                .compare_exchange(0, reason.close_code(), Ordering::AcqRel, Ordering::Acquire);
    }

    /// Причина закрытия, если её уже попросили.
    pub(super) fn requested(&self) -> Option<SessionCloseReason> {
        match self.code.load(Ordering::Acquire) {
            0 => None,
            code => SessionCloseReason::from_close_code(code),
        }
    }
}

/// Калитка доставки данных соединения приёмникам.
///
/// Поток соединения проводит через калитку каждую выдачу байтов и сообщений
/// страницы (`deliver`); конец сеанса закрывает калитку (`close`) ДО вызова
/// `session_ended`. Закрытие ждёт выдачу, которая уже идёт, а после него новых
/// выдач нет: приёмник не получает данные сеанса после его конца.
/// Порядок замков: замок состояния сервера, затем калитка (поток соединения
/// берёт только калитку), поэтому взаимной блокировки нет.
#[derive(Debug)]
pub(super) struct DeliveryGate {
    open: Mutex<bool>,
}

impl DeliveryGate {
    fn new() -> Self {
        Self {
            open: Mutex::new(true),
        }
    }

    /// Выполняет выдачу, если калитка открыта; `false` — сеанс уже закончен.
    pub(super) fn deliver(&self, delivery: impl FnOnce()) -> bool {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if *open {
            delivery();
        }
        *open
    }

    fn close(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }
}

/// Запись о соединении в таблице сервера.
struct ConnectionSlot {
    close: Arc<CloseRequest>,
    delivery: Arc<DeliveryGate>,
    outgoing: SyncSender<String>,
    /// `attach` удался: соединение больше не считается «неподключённым».
    attached: bool,
}

/// Фаза сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerPhase {
    Running,
    ShuttingDown,
}

/// Всё, что охраняет замок.
struct ServerState {
    registry: SessionRegistry,
    tokens: Box<dyn SessionTokenSource>,
    connections: HashMap<ConnectionId, ConnectionSlot>,
    phase: ServerPhase,
}

/// Ручка потока соединения: что ему выдал сервер при приёме.
pub(super) struct ConnectionHandle {
    pub(super) id: ConnectionId,
    pub(super) close: Arc<CloseRequest>,
    pub(super) delivery: Arc<DeliveryGate>,
    pub(super) outgoing: Receiver<String>,
}

/// Почему соединение не принято.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmissionRefusal {
    TooManyUnattached,
    ShuttingDown,
}

/// Счётчики потерь и доставки (видны в отчёте о завершении).
#[derive(Debug, Default)]
struct Counters {
    delivered_chunks: AtomicU64,
    dropped_stale_frames: AtomicU64,
    dropped_stale_messages: AtomicU64,
    dropped_page_messages: AtomicU64,
}

/// Общее состояние сервера.
pub(super) struct Shared {
    pub(super) policy: BrowserSessionPolicy,
    port: LoopbackPort,
    state: Mutex<ServerState>,
    pub(super) byte_sink: Arc<dyn SessionByteSink>,
    pub(super) events: Arc<dyn SessionEventSink>,
    counters: Counters,
    next_connection: AtomicU64,
    stopping: AtomicBool,
    /// Потоки соединений; сюда кладёт цикл приёма, забирает `shutdown`.
    pub(super) workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    pub(super) fn new(
        policy: BrowserSessionPolicy,
        port: LoopbackPort,
        tokens: Box<dyn SessionTokenSource>,
        byte_sink: Arc<dyn SessionByteSink>,
        events: Arc<dyn SessionEventSink>,
    ) -> Self {
        Self {
            policy,
            port,
            state: Mutex::new(ServerState {
                registry: SessionRegistry::new(RegistryTimings::from(&policy)),
                tokens,
                connections: HashMap::new(),
                phase: ServerPhase::Running,
            }),
            byte_sink,
            events,
            counters: Counters::default(),
            next_connection: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, ServerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn port(&self) -> LoopbackPort {
        self.port
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Публикует конец сеанса: закрывает калитку доставки его соединения (если
    /// было), затем сообщает приёмнику байт и шлёт событие.
    fn announce_end(
        &self,
        state: &ServerState,
        connection: Option<ConnectionId>,
        session: SessionRef,
        reason: SessionEndReason,
    ) {
        if let Some(slot) = connection.and_then(|id| state.connections.get(&id)) {
            slot.delivery.close();
        }
        tracing::info!(
            session = session.key.as_str(),
            ?reason,
            "browser session ended"
        );
        self.byte_sink.session_ended(&session);
        self.events
            .publish_lifecycle(SessionLifecycleEvent::Ended { session, reason });
    }

    // ---- приём соединений ------------------------------------------------

    /// Регистрирует новое TCP-соединение или отказывает (лимит, выключение).
    pub(super) fn register_connection(&self) -> Result<ConnectionHandle, AdmissionRefusal> {
        let mut state = self.lock_state();
        if state.phase == ServerPhase::ShuttingDown {
            return Err(AdmissionRefusal::ShuttingDown);
        }
        // Места тех, кого уже просили закрыться (вытеснены), не считаются.
        let waiting: Vec<ConnectionId> = state
            .connections
            .iter()
            .filter(|(_, slot)| !slot.attached && slot.close.requested().is_none())
            .map(|(id, _)| *id)
            .collect();
        if waiting.len() >= self.policy.max_unattached_connections {
            // Новое соединение важнее старого молчуна: иначе горстка залипших
            // локальных подключений навсегда закрыла бы дверь настоящему экстеншену.
            // Номера растут, поэтому самый старый — с наименьшим номером.
            let oldest = waiting
                .iter()
                .min()
                .and_then(|oldest_id| state.connections.get(oldest_id));
            let Some(oldest) = oldest else {
                return Err(AdmissionRefusal::TooManyUnattached);
            };
            oldest.close.request(SessionCloseReason::AttachTimeout);
            tracing::warn!("too many unattached connections; evicting the oldest one");
        }
        let id = ConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed));
        let close = Arc::new(CloseRequest::default());
        let delivery = Arc::new(DeliveryGate::new());
        let (outgoing_tx, outgoing_rx) = sync_channel(self.policy.outgoing_queue_capacity);
        state.connections.insert(
            id,
            ConnectionSlot {
                close: Arc::clone(&close),
                delivery: Arc::clone(&delivery),
                outgoing: outgoing_tx,
                attached: false,
            },
        );
        Ok(ConnectionHandle {
            id,
            close,
            delivery,
            outgoing: outgoing_rx,
        })
    }

    /// Убирает запись о соединении, когда его поток завершается.
    pub(super) fn release_connection(&self, connection: ConnectionId) {
        self.lock_state().connections.remove(&connection);
    }

    // ---- события потока соединения ---------------------------------------

    /// Проверка `attach`; успех привязывает соединение и публикует `Attached`.
    pub(super) fn attach(
        &self,
        connection: ConnectionId,
        session: &SessionKey,
        presented: &SessionToken,
    ) -> Result<SessionRef, AttachRejection> {
        let mut state = self.lock_state();
        let attached = state.registry.attach(session, presented, connection)?;
        if let Some(slot) = state.connections.get_mut(&connection) {
            slot.attached = true;
        }
        tracing::info!(session = session.as_str(), "browser session attached");
        self.events
            .publish_lifecycle(SessionLifecycleEvent::Attached {
                session: attached.clone(),
            });
        Ok(attached)
    }

    /// Подключённое соединение оборвалось без кадра закрытия.
    pub(super) fn connection_lost(&self, connection: ConnectionId) {
        let mut state = self.lock_state();
        if let Some(session) = state.registry.connection_lost(connection, Instant::now()) {
            tracing::warn!(
                session = session.key.as_str(),
                "browser session connection lost"
            );
            self.events
                .publish_lifecycle(SessionLifecycleEvent::ConnectionLost { session });
        }
    }

    /// Страница сама закрыла соединение: сеанс закончен.
    pub(super) fn session_closed_by_page(&self, connection: ConnectionId, code: Option<u16>) {
        let mut state = self.lock_state();
        if let Some(session) = state.registry.end_session_of(connection) {
            let reason = SessionEndReason::ClosedByPage { code };
            self.announce_end(&state, Some(connection), session, reason);
        }
    }

    /// Мы закрываем соединение этого сеанса (нарушение, `tab_gone`): сеанс закончен.
    pub(super) fn session_closed_by_player(
        &self,
        connection: ConnectionId,
        reason: SessionCloseReason,
    ) {
        let mut state = self.lock_state();
        if let Some(session) = state.registry.end_session_of(connection) {
            let reason = SessionEndReason::ClosedByPlayer(reason);
            self.announce_end(&state, Some(connection), session, reason);
        }
    }

    // ---- счётчики --------------------------------------------------------

    pub(super) fn count_delivered_chunk(&self) {
        self.counters
            .delivered_chunks
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_stale_frame(&self) {
        self.counters
            .dropped_stale_frames
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_stale_message(&self) {
        self.counters
            .dropped_stale_messages
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Считает потерянное сообщение страницы; возвращает накопленное число,
    /// чтобы вызывающий мог ограничить частоту записей в журнал.
    pub(super) fn count_dropped_page_message(&self) -> u64 {
        self.counters
            .dropped_page_messages
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    // ---- сроки -----------------------------------------------------------

    /// Применяет истёкшие сроки реестра (вызывает цикл приёма).
    pub(super) fn expire_deadlines(&self, now: Instant) {
        let mut state = self.lock_state();
        // У истёкшего сеанса соединения нет (ждал attach или возврата).
        if let Some((session, reason)) = state.registry.expire(now) {
            self.announce_end(&state, None, session, reason);
        }
    }

    // ---- команды SessionControl ------------------------------------------

    pub(super) fn open_session(
        &self,
        request: &OpenSessionRequest,
        now: Instant,
    ) -> Result<OpenedSession, OpenSessionError> {
        let mut guard = self.lock_state();
        let state = &mut *guard;
        if state.phase == ServerPhase::ShuttingDown {
            return Err(OpenSessionError::ShuttingDown);
        }
        let grant = state
            .registry
            .open(&request.session_key, now, &mut *state.tokens)
            .map_err(OpenSessionError::TokenSource)?;
        let replaced = grant.replaced.map(|replaced| {
            if let Some(slot) = replaced
                .connection
                .and_then(|connection| state.connections.get(&connection))
            {
                slot.close.request(SessionCloseReason::ReplacedByAnotherTab);
            }
            self.announce_end(
                state,
                replaced.connection,
                replaced.session.clone(),
                SessionEndReason::ClosedByPlayer(SessionCloseReason::ReplacedByAnotherTab),
            );
            replaced.session
        });
        Ok(OpenedSession {
            token: grant.token,
            outcome: grant.outcome,
            instance: grant.instance,
            replaced,
        })
    }

    pub(super) fn send_to_page(
        &self,
        message: &PlayerSessionMessage,
    ) -> Result<(), SendToPageError> {
        let json =
            encode_message(message).map_err(|error| SendToPageError::Unencodable(error.kind()))?;
        let state = self.lock_state();
        if state.phase == ServerPhase::ShuttingDown {
            return Err(SendToPageError::ShuttingDown);
        }
        let slot = state
            .registry
            .attached_connection()
            .and_then(|connection| state.connections.get(&connection))
            .ok_or(SendToPageError::NoAttachedSession)?;
        slot.outgoing.try_send(json).map_err(|error| match error {
            TrySendError::Full(_) => SendToPageError::QueueFull,
            TrySendError::Disconnected(_) => SendToPageError::NoAttachedSession,
        })
    }

    /// Закрывает сеанс, только если активен именно этот экземпляр. Проверка и
    /// закрытие — под одним замком: между ними никто не успеет заменить сеанс.
    pub(super) fn close_session(
        &self,
        session: &SessionRef,
        reason: SessionCloseReason,
    ) -> CloseOutcome {
        let mut state = self.lock_state();
        let Some(ended) = state.registry.end_instance(session) else {
            return CloseOutcome::NotActive;
        };
        if let Some(slot) = ended
            .connection
            .and_then(|connection| state.connections.get(&connection))
        {
            slot.close.request(reason);
        }
        self.announce_end(
            &state,
            ended.connection,
            ended.session,
            SessionEndReason::ClosedByPlayer(reason),
        );
        CloseOutcome::Closed
    }

    pub(super) fn close_active_session(&self, reason: SessionCloseReason) {
        let mut state = self.lock_state();
        if let Some(ended) = state.registry.end_active() {
            if let Some(slot) = ended
                .connection
                .and_then(|connection| state.connections.get(&connection))
            {
                slot.close.request(reason);
            }
            self.announce_end(
                &state,
                ended.connection,
                ended.session,
                SessionEndReason::ClosedByPlayer(reason),
            );
        }
    }

    // ---- выключение ------------------------------------------------------

    /// Начинает выключение: больше никого не принимаем, всем соединениям — 4006.
    pub(super) fn begin_shutdown(&self) {
        let mut state = self.lock_state();
        if state.phase == ServerPhase::Running {
            state.phase = ServerPhase::ShuttingDown;
            if let Some(ended) = state.registry.end_active() {
                self.announce_end(
                    &state,
                    ended.connection,
                    ended.session,
                    SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerShuttingDown),
                );
            }
            for slot in state.connections.values() {
                slot.close.request(SessionCloseReason::PlayerShuttingDown);
            }
        }
        self.stopping.store(true, Ordering::Release);
    }

    pub(super) fn report(&self, detached_threads: usize) -> ServerShutdownReport {
        ServerShutdownReport {
            delivered_chunks: self.counters.delivered_chunks.load(Ordering::Relaxed),
            dropped_stale_frames: self.counters.dropped_stale_frames.load(Ordering::Relaxed),
            dropped_stale_messages: self.counters.dropped_stale_messages.load(Ordering::Relaxed),
            dropped_page_messages: self.counters.dropped_page_messages.load(Ordering::Relaxed),
            detached_threads,
        }
    }
}

#[cfg(test)]
mod tests;
