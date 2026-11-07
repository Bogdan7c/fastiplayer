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
//!
//! # Порядок данных одного соединения (обещание сервера, сессия 03b)
//!
//! Данные страницы — сообщения ([`SessionEventSink::publish_page_message`]) и
//! байты ([`SessionByteSink::accept`]) — сервер выдаёт **из одного потока на
//! соединение**, синхронно, по одному кадру WebSocket за раз:
//! - вызовы одного соединения никогда не перекрываются: следующий начнётся только
//!   после возврата предыдущего (оба трейта вместе);
//! - порядок вызовов по обоим трейтам вместе **равен порядку кадров на проводе**:
//!   если страница послала `source_buffer_added`, затем байты, затем
//!   `timestamp_offset_changed`, приёмники увидят ровно эту последовательность.
//!   Поэтому приложение может сложить сообщения MSE и байты в одну очередь в порядке
//!   вызовов и не восстанавливать порядок само;
//! - выдаётся только то, что пропустил сторож потока: устаревшие кадры старого
//!   поколения выбрасываются и считаются, на порядок остальных это не влияет;
//! - сообщение, на которое приёмник ответил [`EventSinkFull`], потеряно (сервер
//!   его не повторяет): для единой очереди это дыра в последовательности, поэтому
//!   такой приёмник не должен отказывать сообщениям MSE;
//! - после обрыва и повторного `attach` данные того же сеанса продолжает выдавать
//!   поток нового соединения; поток старого к этому моменту уже закончил все выдачи
//!   (он сам сообщает об обрыве и выходит), поэтому вызовы и тут не перекрываются.
//!
//! Противодавление (сессия 09b2): перед каждым чтением сокета поток соединения
//! спрашивает приёмник байтов, есть ли место ([`SessionByteSink::wait_for_room`]), и
//! при «нет» не читает — буферы TCP наполняются, и браузер придерживает отправку сам.
//! Вопрос задаётся вне калитки доставки и вне замков сервера, с потолком ожидания.
//!
//! События жизни ([`SessionEventSink::publish_lifecycle`]) и
//! [`SessionByteSink::session_ended`] приходят из **других** потоков (поток
//! приёма, сторож сроков, `SessionControl`, выключение) — общую очередь данных и
//! событий жизни надо охранять замком. После начала `session_ended(S)` новых
//! `accept` и `publish_page_message` для `S.instance` не будет, а уже идущий вызов
//! успевает завершиться до него (калитка доставки соединения).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use browser_bridge_protocol::{
    AppendSequence, PageSessionMessage, ProtocolVersion, RangeRequestId, SessionCloseReason,
    SourceBufferIndex, StreamGeneration,
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
/// Порядок: `accept` зовёт поток соединения, тот же, что зовёт
/// [`SessionEventSink::publish_page_message`]; вызовы обоих трейтов одного
/// соединения не перекрываются и идут в порядке кадров на проводе (подробно — в
/// документации модуля, раздел «Порядок данных одного соединения»).
/// `session_ended` зовёт другой поток.
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

    /// Есть ли у приёмника место под следующую рамку (противодавление, сессия 09b2).
    ///
    /// Поток соединения спрашивает ПЕРЕД каждым чтением сокета, вне калитки доставки
    /// и вне замков сервера. [`IntakeRoom::Exhausted`] — поток сокет не читает: байты
    /// остаются в буферах TCP, браузер видит растущий `bufferedAmount` и придерживает
    /// отправку сам. Так всплеск мелких рамок (журнал страницы, решение 176: десятки
    /// тысяч однокадровых фрагментов) не обгоняет разбор, каким бы медленным тот ни был.
    /// Реализация ждёт места не дольше `patience` и возвращает, что есть; поток снова
    /// спросит после своих дел (просьбы закрыть, исходящие). По умолчанию место есть
    /// всегда (приёмники без очереди: счётчики, тесты).
    fn wait_for_room(&self, session: &SessionRef, patience: Duration) -> IntakeRoom {
        let _ = (session, patience);
        IntakeRoom::Available
    }
}

/// Ответ приёмника на вопрос «есть ли место» ([`SessionByteSink::wait_for_room`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeRoom {
    /// Следующую рамку можно читать.
    Available,
    /// Места нет: сокет пока не читать.
    Exhausted,
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

impl SessionEndReason {
    /// Причина протокола, которую назвала страница, закрыв связь сама (решения 132, 179).
    ///
    /// `None` — связь закрыла не страница, или её код не из наших прикладных 4000–4999
    /// (обычное 1000, кадр без кода, чужой код): такой конец ничего не объясняет, и
    /// угадывать по нему смысл нельзя. Перевод кода — только через
    /// [`SessionCloseReason::from_close_code`], чтобы числа жили в одном месте протокола.
    #[must_use]
    pub fn reason_named_by_page(self) -> Option<SessionCloseReason> {
        match self {
            Self::ClosedByPage { code: Some(code) } => SessionCloseReason::from_close_code(code),
            Self::ClosedByPage { code: None }
            | Self::ClosedByPlayer(_)
            | Self::ReconnectGraceExpired
            | Self::FirstAttachDeadlineExpired => None,
        }
    }
}

/// События жизни сеанса. Ни одно не должно потеряться: их мало на сеанс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycleEvent {
    /// Соединение успешно подключено (в том числе после обрыва).
    Attached {
        /// Сеанс (ключ и экземпляр).
        session: SessionRef,
        /// Общая версия протокола этого подключения (из `hello` открывшего сеанс).
        /// По ней приложение решает, что можно слать странице: при 1.0 —
        /// никаких `fetch_resource` (`PlayerSessionMessage::introduced_in`).
        agreed_protocol: ProtocolVersion,
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
///
/// Порядок: `publish_page_message` зовёт поток соединения — тот же, что зовёт
/// [`SessionByteSink::accept`]; вызовы обоих трейтов одного соединения не
/// перекрываются и идут в порядке кадров на проводе, так что сообщения MSE и байты
/// можно складывать в одну очередь в порядке вызовов. `publish_lifecycle` зовут
/// другие потоки (под замком состояния сервера): общую с ним очередь охранять
/// замком. Подробно — в документации модуля, раздел «Порядок данных одного соединения».
pub trait SessionEventSink: Send + Sync {
    /// Сообщение страницы (не байты): MSE-события, перемотка, меню и т. п.
    /// Очередь ограничена: при переполнении сервер считает и логирует потерю.
    /// Вызывается из потока соединения строго между соседними `accept` в порядке
    /// кадров на проводе.
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
