//! Швы между сервером сеанса и приложением: куда падают байты и события.
//!
//! Сервер ничего не знает ни о демуксере, ни об окне (правило «модули зависят от
//! контрактов»). Приложение (сессия 03/04) реализует два трейта:
//! - [`SessionByteSink`] — принимает куски байтов, уже прошедшие сторожа потока;
//! - [`SessionEventSink`] — принимает сообщения страницы и события жизни сеанса.
//!
//! В 02 вместо демуксера стоит [`CountingByteSink`]: он считает и выбрасывает.
//!
//! Обещание для реализаций: методы вызываются из потоков сервера и **не должны
//! блокироваться надолго** и обращаться обратно в `SessionControl` синхронно
//! (события жизни публикуются под замком состояния, чтобы порядок событий был
//! тем же, что порядок переходов).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use browser_bridge_protocol::{
    AppendSequence, PageSessionMessage, RangeRequestId, SessionCloseReason, SourceBufferIndex,
    StreamGeneration,
};

use crate::registry::SessionRef;

/// Кусок байтов одного SourceBuffer, принятый сторожем потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBufferChunk<'payload> {
    /// Поколение (новый MediaSource).
    pub generation: StreamGeneration,
    /// Индекс SourceBuffer в поколении.
    pub source_buffer: SourceBufferIndex,
    /// Порядковый номер рамки в SourceBuffer (подряд с 0, это уже проверено).
    pub sequence: AppendSequence,
    /// Байты fMP4/WebM; куски режутся где угодно, в том числе посреди бокса.
    pub payload: &'payload [u8],
}

/// Кусок файла `<video src>` — ответ на `fetch_range`.
///
/// Сторож потока такие рамки не охватывает (запросы диапазонов не привязаны к
/// поколению), поэтому в 02 сервер их пропускает как есть; сверку с
/// выданными `fetch_range` делает приёмник (сессия 03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeChunk<'payload> {
    /// Номер запроса `fetch_range`.
    pub request_id: RangeRequestId,
    /// Смещение первого байта в файле.
    pub offset: u64,
    /// Байты файла.
    pub payload: &'payload [u8],
}

/// Откуда кусок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkSource<'payload> {
    /// Байты SourceBuffer (MSE).
    SourceBuffer(SourceBufferChunk<'payload>),
    /// Байты файла `<video src>`.
    Range(RangeChunk<'payload>),
}

/// Кусок байтов сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionChunk<'payload> {
    /// Сеанс, которому принадлежат байты.
    pub session: &'payload SessionRef,
    /// Содержимое.
    pub source: ChunkSource<'payload>,
}

/// Приёмник байтов сеанса (в 03 — вход демуксера).
///
/// Гарантия сервера: после того как `session_ended(S)` начал вызываться, новых
/// `accept` с `S.instance` не будет, а уже идущий `accept` этого экземпляра
/// успел завершиться (сервер ждёт его перед `session_ended`). Приёмник всё равно
/// должен сверять `instance`: ключ сеанса может повториться у следующего экземпляра.
pub trait SessionByteSink: Send + Sync {
    /// Принимает кусок; вызывается из потока соединения по порядку рамок.
    fn accept(&self, chunk: SessionChunk<'_>);

    /// Сеанс закончился (любая причина): приёмник освобождает буферы сеанса.
    fn session_ended(&self, session: &SessionRef);
}

/// Очередь приёмника событий переполнена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("session event sink is full")]
pub struct EventSinkFull;

/// Почему сеанс закончился.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    /// Мы закрыли соединение этим кодом (замена, нарушение, `tab_gone`, выход).
    ClosedByPlayer(SessionCloseReason),
    /// Страница сама закрыла соединение кадром закрытия (с кодом или без).
    ClosedByPage {
        /// Код из кадра закрытия; `None` — кадр без кода (1005).
        code: Option<u16>,
    },
    /// После обрыва никто не вернулся за `reconnect_grace`.
    ReconnectGraceExpired,
    /// Выданный сеанс не дождался `attach` за `first_attach_deadline`.
    FirstAttachDeadlineExpired,
}

/// События жизни сеанса. Ни одно не должно потеряться: их мало на сеанс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycleEvent {
    /// Соединение успешно подключено (в том числе после обрыва).
    Attached {
        /// Сеанс (ключ и экземпляр).
        session: SessionRef,
    },
    /// Подключённое соединение оборвалось без кадра закрытия: начался срок
    /// ожидания возврата (`reconnect_grace`).
    ConnectionLost {
        /// Сеанс (ключ и экземпляр).
        session: SessionRef,
    },
    /// Сеанс закончился.
    Ended {
        /// Сеанс (ключ и экземпляр): конец старого экземпляра не касается нового с тем же ключом.
        session: SessionRef,
        /// Причина.
        reason: SessionEndReason,
    },
}

/// Приёмник сообщений страницы и событий жизни сеанса.
pub trait SessionEventSink: Send + Sync {
    /// Сообщение страницы (не байты): MSE-события, перемотка, меню и т. п.
    /// Очередь ограничена: при переполнении сервер считает и логирует потерю.
    ///
    /// # Errors
    /// [`EventSinkFull`] — приёмник не успевает.
    fn publish_page_message(
        &self,
        session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull>;

    /// Событие жизни сеанса; реализация обязана сохранить его без потерь.
    fn publish_lifecycle(&self, event: SessionLifecycleEvent);
}

/// Счётчики одного потока байт.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamCounters {
    /// Принятых рамок.
    pub frames: u64,
    /// Принятых байт груза.
    pub bytes: u64,
}

/// Снимок [`CountingByteSink`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CountingSnapshot {
    /// По (поколение, SourceBuffer).
    pub source_buffers: BTreeMap<(StreamGeneration, SourceBufferIndex), StreamCounters>,
    /// По номеру запроса диапазона.
    pub ranges: BTreeMap<RangeRequestId, StreamCounters>,
    /// Сеансы, о завершении которых сообщили, по порядку.
    pub ended_sessions: Vec<SessionRef>,
}

/// Заглушка 02: считает рамки и байты, груз выбрасывает.
#[derive(Debug, Default)]
pub struct CountingByteSink {
    state: Mutex<CountingSnapshot>,
}

impl CountingByteSink {
    /// Пустой счётчик.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Копия счётчиков на сейчас.
    #[must_use]
    pub fn snapshot(&self) -> CountingSnapshot {
        self.lock_state().clone()
    }

    /// Замок счётчиков. Отравление снимаем: каждая секция — одно сложение,
    /// паниковать внутри нечему, а счётчик важнее падения сервера.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, CountingSnapshot> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Прибавляет рамку к счётчикам без паники при переполнении.
fn add_frame(counters: &mut StreamCounters, payload_len: usize) {
    counters.frames = counters.frames.saturating_add(1);
    counters.bytes = counters
        .bytes
        .saturating_add(u64::try_from(payload_len).unwrap_or(u64::MAX));
}

impl SessionByteSink for CountingByteSink {
    fn accept(&self, chunk: SessionChunk<'_>) {
        let mut state = self.lock_state();
        match chunk.source {
            ChunkSource::SourceBuffer(buffer) => add_frame(
                state
                    .source_buffers
                    .entry((buffer.generation, buffer.source_buffer))
                    .or_default(),
                buffer.payload.len(),
            ),
            ChunkSource::Range(range) => add_frame(
                state.ranges.entry(range.request_id).or_default(),
                range.payload.len(),
            ),
        }
    }

    fn session_ended(&self, session: &SessionRef) {
        self.lock_state().ended_sessions.push(session.clone());
    }
}

#[cfg(test)]
mod tests;
