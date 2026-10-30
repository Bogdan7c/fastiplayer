//! Единая очередь данных сеанса: сообщения MSE и байты в порядке кадров на проводе.
//!
//! Сервер сеанса выдаёт сообщения страницы ([`SessionEventSink::publish_page_message`])
//! и байты ([`SessionByteSink::accept`]) из ОДНОГО потока соединения, синхронно и в
//! порядке кадров на проводе (обещание `browser-bridge-session`, модуль `sink`).
//! [`OrderedSessionQueue`] реализует оба трейта и кладёт оба вида данных в одну
//! очередь в порядке вызовов — так источник получает `source_buffer_added`, байты и
//! `timestamp_offset_changed` ровно в том порядке, в каком их сделала страница.
//!
//! Остальное:
//! - `session_ended` кладёт в ту же очередь [`SessionInput::Ended`]: сервер зовёт
//!   его только после последней выдачи данных этого экземпляра, поэтому конец
//!   оказывается после всех его байтов;
//! - события жизни сеанса уходят в ящик главного цикла без изменений
//!   (`ProcessLifecycle` работает как в сессии 02);
//! - байты `<video src>` (ответы на `fetch_range`, рамки вида 2) идут в ту же очередь
//!   в порядке провода ([`SessionInput::RangeBytes`]) и заряжаются своим размером под
//!   тот же потолок, что байты SourceBuffer: честная страница не упрётся (источник
//!   держит не больше одного запроса ≤ 16 МиБ), а страница-враг закроет свой сеанс.
//!   Сверку рамок с выданными запросами делает источник файла, не очередь.
//!
//! # Переполнение (решение владельца, сессия 03b)
//!
//! Очередь не отказывает серверу (`EventSinkFull` = дыра в последовательности MSE).
//! Если очередь упёрлась в [`QueueLimits`] — страница не слушает «полон» или наш
//! разбор встал, — экземпляр сеанса закрывается кодом 4003 (`ProtocolViolation`).
//! Потолок байтов считает и сообщения страницы, не только байты SourceBuffer
//! (см. [`SessionInput::charged_bytes`]): иначе поток крупных сообщений (меню
//! вариантов до ~80 КиБ) при вставшем разборе рос бы до потолка элементов —
//! сотни мегабайт. Дальше:
//! его данные в очереди выбрасываются, все следующие тоже (со счётом), память не
//! растёт, процесс живёт. Звать `SessionControl` отсюда нельзя (приёмники
//! вызываются под замками сервера), поэтому очередь кладёт в ящик просьбу
//! [`IntakeNotice::CloseSession`], а исполняет её главный цикл.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use browser_bridge_protocol::limits::MAX_JSON_MESSAGE_BYTES;
use browser_bridge_protocol::{
    AppendSequence, PageSessionMessage, RangeRequestId, SessionCloseReason, SourceBufferIndex,
    StreamGeneration, encode_message,
};
use browser_bridge_session::{
    ChunkSource, EventSinkFull, SessionByteSink, SessionChunk, SessionEventSink, SessionInstance,
    SessionLifecycleEvent, SessionRef,
};
use tracing::{debug, warn};

use super::inbox::BrowserSessionInbox;
use super::intake_notice::IntakeNotice;

/// Сколько байтов груза может ждать в очереди. Политика, а не настройка: это
/// предохранитель от зависшего разбора, а не окно буфера (окно — в источнике).
/// 32 МиБ — секунды видео 4K или десятки секунд 1080p: честная страница, которая
/// слушает «полон», сюда не дойдёт, а память процесса останется в разумных рамках.
pub(crate) const MAX_QUEUED_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Сколько элементов (сообщений и кусков) может ждать в очереди. Защищает от
/// потока крошечных рамок, которые не упрутся в потолок байтов: 4096 — это
/// больше минуты самой частой нарезки, какую мы видели (Twitch, ~30 рамок/с).
pub(crate) const MAX_QUEUED_ITEMS: usize = 4096;

/// Пределы очереди.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueLimits {
    /// Потолок байтов груза в очереди.
    pub(crate) max_payload_bytes: usize,
    /// Потолок элементов в очереди.
    pub(crate) max_items: usize,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: MAX_QUEUED_PAYLOAD_BYTES,
            max_items: MAX_QUEUED_ITEMS,
        }
    }
}

/// Кусок байтов SourceBuffer, скопированный из кадра (кадр живёт только на время
/// вызова `accept`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedChunk {
    /// Поколение.
    pub(crate) generation: StreamGeneration,
    /// SourceBuffer поколения.
    pub(crate) source_buffer: SourceBufferIndex,
    /// Номер рамки в SourceBuffer (сторож сервера уже проверил, что подряд).
    pub(crate) sequence: AppendSequence,
    /// Байты fMP4/WebM/TS.
    pub(crate) payload: Vec<u8>,
}

/// Рамка байтов файла `<video src>`, скопированная из кадра.
///
/// Номер запроса и смещение — как на проводе: сервер их не проверяет (запросы знает
/// только источник файла).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedRangeChunk {
    /// Номер запроса `fetch_range`, на который отвечает рамка.
    pub(crate) request_id: RangeRequestId,
    /// Смещение первого байта рамки в файле.
    pub(crate) offset: u64,
    /// Байты файла.
    pub(crate) payload: Vec<u8>,
}

/// Элемент единой очереди — в порядке кадров на проводе.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionInput {
    /// Сообщение страницы (MSE, перемотка, намерения).
    Message(SessionRef, PageSessionMessage),
    /// Байты SourceBuffer.
    Bytes(SessionRef, OwnedChunk),
    /// Байты файла `<video src>` (ответ на `fetch_range`).
    RangeBytes(SessionRef, OwnedRangeChunk),
    /// Экземпляр сеанса закончился: данных от него больше не будет.
    Ended(SessionRef),
}

impl SessionInput {
    /// Сеанс элемента.
    pub(crate) fn session(&self) -> &SessionRef {
        match self {
            Self::Message(session, _)
            | Self::Bytes(session, _)
            | Self::RangeBytes(session, _)
            | Self::Ended(session) => session,
        }
    }

    /// Сколько байтов элемент списывает с потолка [`QueueLimits::max_payload_bytes`].
    ///
    /// - байты SourceBuffer и файла — их длина (основной вес очереди);
    /// - сообщение страницы — длина его компактного JSON. Выбор: точный размер рамки
    ///   до разбора приёмнику не виден, а повторная запись дешёвая (сообщения — единицы
    ///   в секунду, сотни байтов, меню — до 128 КиБ) и честно отражает строки, которые
    ///   разобранное сообщение держит в памяти (MIME, подписи меню). Записать не
    ///   удалось (сообщение не по протоколу — сервер такое не пропускает) — списывается
    ///   потолок сообщения протокола: лучше переоценить, чем пропустить;
    /// - конец экземпляра — ничего (он не проходит проверку пределов вовсе).
    fn charged_bytes(&self) -> usize {
        match self {
            Self::Bytes(_, chunk) => chunk.payload.len(),
            Self::RangeBytes(_, chunk) => chunk.payload.len(),
            Self::Message(_, message) => {
                encode_message(message).map_or(MAX_JSON_MESSAGE_BYTES, |json| json.len())
            }
            Self::Ended(_) => 0,
        }
    }
}

/// Элемент в очереди вместе с тем, что он списал с потолка байтов (считается один
/// раз — при постановке, без повторной записи JSON при чистке).
#[derive(Debug)]
struct QueuedInput {
    /// Элемент.
    input: SessionInput,
    /// Списано с потолка байтов.
    charged_bytes: usize,
}

/// Счётчики очереди за жизнь процесса.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueCounters {
    /// Рамок байтов, пришедших от сервера (SourceBuffer и `<video src>`), — как
    /// у прежнего `CountingByteSink`: считается всё принятое, даже выброшенное.
    pub(crate) received_frames: u64,
    /// Байтов груза в этих рамках.
    pub(crate) received_bytes: u64,
    /// Рамок файла `<video src>`, поставленных в очередь (дальше их судьба — в
    /// счётчиках переходника: принято / устарело / отвергнуто). Выброшенные из-за
    /// переполнения считает `dropped_overflow_items`.
    pub(crate) queued_range_frames: u64,
    /// Элементов, выброшенных из-за переполнения (из очереди и после него).
    pub(crate) dropped_overflow_items: u64,
    /// Элементов, пришедших после остановки очереди.
    pub(crate) dropped_after_shutdown: u64,
    /// Сколько раз очередь просила закрыть сеанс из-за переполнения.
    pub(crate) overflow_closes: u64,
}

/// Что получил забирающий поток.
#[derive(Debug)]
pub(crate) enum QueueTake {
    /// Элементы в порядке прихода.
    Batch(Vec<SessionInput>),
    /// Очередь остановлена: забирающему пора выходить.
    Shutdown,
}

/// Состояние под замком.
#[derive(Debug, Default)]
struct QueueState {
    /// Элементы в порядке прихода.
    items: VecDeque<QueuedInput>,
    /// Сколько байтов списали элементы `items` (см. [`SessionInput::charged_bytes`]).
    payload_bytes: usize,
    /// Экземпляры, закрываемые из-за переполнения: их данные не принимаются,
    /// пока не придёт их `Ended`.
    overflowed: BTreeSet<SessionInstance>,
    /// Счётчики.
    counters: QueueCounters,
    /// Очередь остановлена.
    shut_down: bool,
}

/// Итог постановки элемента данных.
enum Admission {
    /// Положен.
    Queued,
    /// Выброшен и посчитан, делать больше нечего.
    Dropped,
    /// Этот элемент переполнил очередь: экземпляр надо закрыть.
    Overflowed,
}

/// Единая очередь данных сеанса (оба приёмника сервера сразу).
pub(crate) struct OrderedSessionQueue {
    /// Пределы.
    limits: QueueLimits,
    /// Очередь и счётчики.
    state: Mutex<QueueState>,
    /// Будит забирающий поток.
    ready: Condvar,
    /// Ящик главного цикла: события жизни и просьбы закрыть сеанс.
    inbox: Arc<BrowserSessionInbox>,
}

impl OrderedSessionQueue {
    /// Пустая очередь с пределами `limits`; события жизни и просьбы — в `inbox`.
    pub(crate) fn new(limits: QueueLimits, inbox: Arc<BrowserSessionInbox>) -> Self {
        Self {
            limits,
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            inbox,
        }
    }

    /// Ждёт данных и забирает всё накопленное; [`QueueTake::Shutdown`] — пора выходить
    /// (недобранные элементы при остановке никому не нужны и выбрасываются).
    pub(crate) fn wait_batch(&self) -> QueueTake {
        let mut state = self.lock_state();
        loop {
            if state.shut_down {
                return QueueTake::Shutdown;
            }
            if !state.items.is_empty() {
                state.payload_bytes = 0;
                return QueueTake::Batch(
                    state.items.drain(..).map(|queued| queued.input).collect(),
                );
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Останавливает очередь: забирающий поток проснётся с [`QueueTake::Shutdown`],
    /// дальнейшие данные выбрасываются со счётом.
    pub(crate) fn shut_down(&self) {
        {
            let mut state = self.lock_state();
            state.shut_down = true;
            state.items.clear();
            state.payload_bytes = 0;
        }
        self.ready.notify_all();
    }

    /// Пуста ли очередь (тестам: поток приёма уже забрал пачку).
    #[cfg(test)]
    pub(crate) fn is_empty_for_tests(&self) -> bool {
        self.lock_state().items.is_empty()
    }

    /// Сколько байтов сейчас списано с потолка (тестам: память ограничена).
    #[cfg(test)]
    pub(crate) fn held_bytes_for_tests(&self) -> usize {
        self.lock_state().payload_bytes
    }

    /// Копия счётчиков.
    pub(crate) fn counters(&self) -> QueueCounters {
        self.lock_state().counters
    }

    /// Ставит элемент данных (сообщение или байты) с проверкой пределов.
    fn admit(&self, input: SessionInput) {
        let session = input.session().clone();
        // Вес считается до замка: запись JSON не должна держать очередь.
        let queued = QueuedInput {
            charged_bytes: input.charged_bytes(),
            input,
        };
        let admission = {
            let mut state = self.lock_state();
            let admission = self.admit_locked(&mut state, queued);
            if matches!(admission, Admission::Overflowed) {
                Self::purge_overflowed_instance(&mut state, session.instance);
            }
            admission
        };
        match admission {
            Admission::Queued => self.ready.notify_one(),
            Admission::Dropped => {}
            Admission::Overflowed => self.request_overflow_close(session),
        }
    }

    /// Решение о постановке под замком.
    fn admit_locked(&self, state: &mut QueueState, queued: QueuedInput) -> Admission {
        if state.shut_down {
            state.counters.dropped_after_shutdown =
                state.counters.dropped_after_shutdown.saturating_add(1);
            return Admission::Dropped;
        }
        let instance = queued.input.session().instance;
        if state.overflowed.contains(&instance) {
            state.counters.dropped_overflow_items =
                state.counters.dropped_overflow_items.saturating_add(1);
            return Admission::Dropped;
        }
        let would_hold_bytes = state.payload_bytes.saturating_add(queued.charged_bytes);
        if state.items.len() >= self.limits.max_items
            || would_hold_bytes > self.limits.max_payload_bytes
        {
            state.overflowed.insert(instance);
            state.counters.dropped_overflow_items =
                state.counters.dropped_overflow_items.saturating_add(1);
            state.counters.overflow_closes = state.counters.overflow_closes.saturating_add(1);
            return Admission::Overflowed;
        }
        state.payload_bytes = would_hold_bytes;
        if matches!(queued.input, SessionInput::RangeBytes(..)) {
            state.counters.queued_range_frames =
                state.counters.queued_range_frames.saturating_add(1);
        }
        state.items.push_back(queued);
        Admission::Queued
    }

    /// Выбрасывает из очереди данные экземпляра, который закрывается: поток его
    /// всё равно умрёт, а память освобождается сразу. `Ended` других экземпляров
    /// и их данные не трогаются.
    fn purge_overflowed_instance(state: &mut QueueState, instance: SessionInstance) {
        let before = state.items.len();
        state
            .items
            .retain(|queued| queued.input.session().instance != instance);
        let purged = before - state.items.len();
        state.payload_bytes = state.items.iter().map(|queued| queued.charged_bytes).sum();
        state.counters.dropped_overflow_items = state
            .counters
            .dropped_overflow_items
            .saturating_add(u64::try_from(purged).unwrap_or(u64::MAX));
    }

    /// Просит главный цикл закрыть экземпляр кодом 4003. Зовётся вне замка очереди;
    /// ящик не теряет такие просьбы даже переполненным.
    fn request_overflow_close(&self, session: SessionRef) {
        warn!(
            session = session.key.as_str(),
            instance = session.instance.0,
            max_items = self.limits.max_items,
            max_payload_bytes = self.limits.max_payload_bytes,
            "Browser session data queue overflowed; closing the session as a protocol violation"
        );
        let delivery = self.inbox.post_intake_notices([IntakeNotice::CloseSession {
            session,
            reason: SessionCloseReason::ProtocolViolation,
        }]);
        if delivery.dropped_after_close > 0 {
            debug!("Overflow close request arrived after the inbox closed");
        }
    }

    /// Считает принятую рамку (как прежний `CountingByteSink`).
    fn count_received(&self, payload_len: usize) {
        let mut state = self.lock_state();
        let counters = &mut state.counters;
        counters.received_frames = counters.received_frames.saturating_add(1);
        counters.received_bytes = counters
            .received_bytes
            .saturating_add(u64::try_from(payload_len).unwrap_or(u64::MAX));
    }

    /// Отравленный замок означает панику в другом потоке посреди короткой правки
    /// очереди; сама очередь при этом цела, поэтому продолжаем с ней.
    fn lock_state(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SessionByteSink for OrderedSessionQueue {
    fn accept(&self, chunk: SessionChunk<'_>) {
        let payload_len = match chunk.source {
            ChunkSource::SourceBuffer(buffer) => buffer.payload.len(),
            ChunkSource::Range(range) => range.payload.len(),
        };
        self.count_received(payload_len);
        match chunk.source {
            ChunkSource::SourceBuffer(buffer) => self.admit(SessionInput::Bytes(
                chunk.session.clone(),
                OwnedChunk {
                    generation: buffer.generation,
                    source_buffer: buffer.source_buffer,
                    sequence: buffer.sequence,
                    payload: buffer.payload.to_vec(),
                },
            )),
            ChunkSource::Range(range) => self.admit(SessionInput::RangeBytes(
                chunk.session.clone(),
                OwnedRangeChunk {
                    request_id: range.request_id,
                    offset: range.offset,
                    payload: range.payload.to_vec(),
                },
            )),
        }
    }

    fn session_ended(&self, session: &SessionRef) {
        {
            let mut state = self.lock_state();
            // Данных этого экземпляра больше не будет (обещание сервера): метка
            // переполнения ему больше не нужна.
            state.overflowed.remove(&session.instance);
            if state.shut_down {
                return;
            }
            // Конец кладётся всегда, без проверки пределов: он ничего не весит, а без
            // него поток приёма держал бы писатель закончившегося сеанса.
            state.items.push_back(QueuedInput {
                input: SessionInput::Ended(session.clone()),
                charged_bytes: 0,
            });
        }
        self.ready.notify_one();
    }
}

impl SessionEventSink for OrderedSessionQueue {
    /// Никогда не отвечает `EventSinkFull`: отказ сервер не повторяет, и в
    /// последовательности MSE была бы дыра. Переполнение закрывает сеанс (см.
    /// документацию модуля), а лишние сообщения выбрасываются со счётом.
    fn publish_page_message(
        &self,
        session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull> {
        self.admit(SessionInput::Message(session.clone(), message));
        Ok(())
    }

    fn publish_lifecycle(&self, event: SessionLifecycleEvent) {
        self.inbox.publish_lifecycle(event);
    }
}

#[cfg(test)]
mod tests;
