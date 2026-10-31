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
//!
//! Закрытие соединения (любое: команда плеера, замена, выключение) — это только
//! ПРОСЬБА (`CloseRequest`). Исполняет её поток соединения в строгом порядке:
//! очередь исходящих → прощание (`FarewellSlot`, если положено) → кадр закрытия
//! (решение 127). Прощание кладётся под замком ДО просьбы закрыть.

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

use super::control::{
    ActiveFarewellOutcome, CloseOutcome, FarewellError, OpenSessionError, OpenedSession,
    ReplacedFarewellOutcome, SendToPageError,
};
use super::farewell::{CorridorClaim, FarewellCorridors, FarewellSlot};
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

    /// Сеанс этого соединения уже закончен (калитка закрыта). Так поток соединения
    /// узнаёт «коридор прощания»: сеанс кончился, а закрыть соединение ещё не просили.
    pub(super) fn is_closed(&self) -> bool {
        !*self.open.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// Место для прощального сообщения (пишется после очереди, перед закрытием).
    farewell: Arc<FarewellSlot>,
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
    /// Недавно заменённые экземпляры и их коридоры прощания.
    corridors: FarewellCorridors,
    phase: ServerPhase,
}

impl ServerState {
    /// Просит закрыть соединение, если его запись ещё есть (поток мог уже уйти).
    fn request_close(&self, connection: ConnectionId, reason: SessionCloseReason) {
        if let Some(slot) = self.connections.get(&connection) {
            slot.close.request(reason);
        }
    }
}

/// Ручка потока соединения: что ему выдал сервер при приёме.
pub(super) struct ConnectionHandle {
    pub(super) id: ConnectionId,
    pub(super) close: Arc<CloseRequest>,
    pub(super) delivery: Arc<DeliveryGate>,
    pub(super) outgoing: Receiver<String>,
    pub(super) farewell: Arc<FarewellSlot>,
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
                corridors: FarewellCorridors::new(&policy),
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
        let farewell = Arc::new(FarewellSlot::default());
        let (outgoing_tx, outgoing_rx) = sync_channel(self.policy.outgoing_queue_capacity);
        state.connections.insert(
            id,
            ConnectionSlot {
                close: Arc::clone(&close),
                delivery: Arc::clone(&delivery),
                outgoing: outgoing_tx,
                farewell: Arc::clone(&farewell),
                attached: false,
            },
        );
        Ok(ConnectionHandle {
            id,
            close,
            delivery,
            outgoing: outgoing_rx,
            farewell,
        })
    }

    /// Убирает запись о соединении, когда его поток завершается. Если соединение
    /// было в коридоре прощания, коридор закрыт: прощаться больше не с кем.
    pub(super) fn release_connection(&self, connection: ConnectionId) {
        let mut state = self.lock_state();
        state.connections.remove(&connection);
        state.corridors.connection_released(connection);
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
        // Плеер не попрощался за `farewell_grace`: закрываем 4001, как до 04b.
        // Конец сеанса уже объявлен при замене, здесь — только сокет.
        for connection in state.corridors.expire(now) {
            state.request_close(connection, SessionCloseReason::ReplacedByAnotherTab);
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
        let replaced = match grant.replaced {
            Some(replaced) => {
                // Сеанс кончается сразу (калитка, `session_ended`, событие), а
                // соединение НЕ закрываем: оно входит в коридор прощания и ждёт
                // `bid_farewell_to_replaced` или срока (решение 127). Поток
                // соединения мог уже уйти — тогда и коридора нет.
                let live_connection = replaced
                    .connection
                    .filter(|connection| state.connections.contains_key(connection));
                let evicted = state.corridors.remember_replaced(
                    replaced.session.clone(),
                    live_connection,
                    now,
                );
                if let Some(evicted) = evicted {
                    tracing::warn!("too many farewell corridors; closing the oldest one");
                    state.request_close(evicted, SessionCloseReason::ReplacedByAnotherTab);
                }
                self.announce_end(
                    state,
                    replaced.connection,
                    replaced.session.clone(),
                    SessionEndReason::ClosedByPlayer(SessionCloseReason::ReplacedByAnotherTab),
                );
                Some(replaced.session)
            }
            None => None,
        };
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

    /// Как `close_session`, но соединению сперва уходит прощание. Прощание
    /// кладётся в слот соединения РАНЬШЕ просьбы закрыть (оба — под замком), так
    /// что поток, заметив просьбу, его уже видит.
    pub(super) fn close_session_with_farewell(
        &self,
        session: &SessionRef,
        farewell: &PlayerSessionMessage,
        reason: SessionCloseReason,
    ) -> Result<ActiveFarewellOutcome, FarewellError> {
        // Кодируем до замка: ошибка ничего не меняет.
        let farewell_json =
            encode_message(farewell).map_err(|error| FarewellError::Unencodable(error.kind()))?;
        let mut state = self.lock_state();
        let Some(ended) = state.registry.end_instance(session) else {
            return Ok(ActiveFarewellOutcome::NotActive);
        };
        let outcome = match ended
            .connection
            .and_then(|connection| state.connections.get(&connection))
        {
            Some(slot) => {
                slot.farewell.leave(farewell_json);
                slot.close.request(reason);
                ActiveFarewellOutcome::FarewellQueued
            }
            None => ActiveFarewellOutcome::ClosedWithoutConnection,
        };
        self.announce_end(
            &state,
            ended.connection,
            ended.session,
            SessionEndReason::ClosedByPlayer(reason),
        );
        Ok(outcome)
    }

    /// Прощание заменённому экземпляру в его коридоре. Активный сеанс в таблице
    /// коридоров не бывает никогда, поэтому его этот путь не тронет.
    pub(super) fn bid_farewell_to_replaced(
        &self,
        replaced: &SessionRef,
        farewell: &PlayerSessionMessage,
    ) -> Result<ReplacedFarewellOutcome, FarewellError> {
        let farewell_json =
            encode_message(farewell).map_err(|error| FarewellError::Unencodable(error.kind()))?;
        let mut guard = self.lock_state();
        let state = &mut *guard;
        let outcome = match state.corridors.claim(replaced) {
            CorridorClaim::Claimed(connection) => match state.connections.get(&connection) {
                // Закрыть уже просили (запись живёт до выхода потока) — опоздали.
                Some(slot) if slot.close.requested().is_none() => {
                    slot.farewell.leave(farewell_json);
                    slot.close.request(SessionCloseReason::ReplacedByAnotherTab);
                    ReplacedFarewellOutcome::FarewellQueued
                }
                Some(_) | None => ReplacedFarewellOutcome::CorridorClosed,
            },
            CorridorClaim::AlreadyClosed => ReplacedFarewellOutcome::CorridorClosed,
            CorridorClaim::NoConnection => ReplacedFarewellOutcome::NoConnection,
            CorridorClaim::Unknown => ReplacedFarewellOutcome::NotReplaced,
        };
        Ok(outcome)
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
    ///
    /// Соединения в коридоре прощания получают 4001, а не 4006: их сеанс уже
    /// закончился ЗАМЕНОЙ (так и опубликовано в `Ended`), и странице честнее
    /// узнать именно это. Уже принятое прощание всё равно пишется первым:
    /// просьба закрыть «кто первый, тот и прав», а прощание лежит в слоте.
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
            for connection in state.corridors.close_all() {
                state.request_close(connection, SessionCloseReason::ReplacedByAnotherTab);
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
