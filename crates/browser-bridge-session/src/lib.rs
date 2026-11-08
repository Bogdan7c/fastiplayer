//! Сеанс моста «браузер качает — мы играем» на стороне процесса браузерного
//! режима (issue #2, сессия 02, решения 82, 85, 86).
//!
//! Crate нейтральный и кроссплатформенный: без UI, D-Bus и tokio. Он связывает
//! два участка протокола (`browser-bridge-protocol`):
//!
//! ```text
//!  D-Bus/посредник ──► EntryDesk ──► SessionControl ──► SessionRegistry
//!   «вход»: hello,       (ответ:        (открыть,            (один активный
//!   open_session          порт+пропуск)  закрыть, послать)     сеанс, пропуск)
//!
//!  background экстеншена ═ WebSocket 127.0.0.1 ═► BrowserSessionServer
//!   «сеанс»: attach → байты/сообщения → StreamGate → SessionByteSink / SessionEventSink
//! ```
//!
//! Инварианты, которые держит этот crate:
//! - **1 (враг на входе)**: всё от страницы разбирается строго; нарушение —
//!   закрытие кодом `SessionCloseReason`, а не «попытка понять»;
//! - **3 (вход ≠ сеанс)**: после `open_session` и `attach` байты идут по WebSocket
//!   без участия входа; посредник и D-Bus могут исчезнуть;
//! - **6 (поколения)**: байты старого поколения выбрасывает `StreamGate` и
//!   считает; нарушение порядка закрывает сеанс.
//! - **5 (версии, решение 86)**: общая версия из `hello` идёт `EntryDesk` →
//!   `SessionControl::open_session` → `SessionRegistry` → соединение после
//!   `attach`; сторож соединения создаётся с этой версией и закрывает сеанс
//!   кодом 4003 на сообщении новее неё (при 1.0 — HLS протокола 1.1; при общей
//!   ниже 1.3 — `tab_gone{page_player_failed}`: версия значения тоже строгая), а
//!   `SessionLifecycleEvent::Attached` сообщает версию приложению.
//! - **порядок данных**: сообщения страницы и байты одного соединения приёмники
//!   получают из одного потока, без перекрытия вызовов, в порядке кадров на
//!   проводе (по обоим трейтам вместе); события жизни и `session_ended` — из
//!   других потоков. Подробно — [`sink`], раздел «Порядок данных одного соединения».
//!
//! Модули: [`policy`] (сроки), [`token`] (пропуска), [`registry`] (таблица
//! сеансов, чистая), [`entry_desk`] (стойка входа), [`server`] (WebSocket),
//! [`sink`] (швы к приложению и заглушка 02), [`lifecycle`] (когда выходить).

pub mod entry_desk;
pub mod lifecycle;
pub mod policy;
pub mod registry;
pub mod server;
pub mod sink;
pub mod token;
pub mod wire_tap;

#[cfg(test)]
mod test_support;

pub use entry_desk::{EntryCaller, EntryCallerError, EntryDesk, EntryEvent, EntryOutcome};
pub use lifecycle::{
    ExitCause, LifecycleVerdict, PlaybackDrainOutcome, ProcessLifecycle, ends_with_playback_drain,
};
pub use policy::BrowserSessionPolicy;
pub use registry::{AttachedSession, ConnectionId, SessionInstance, SessionRef};
pub use server::{
    ActiveFarewellOutcome, BrowserSessionServer, CloseOutcome, FarewellError, OpenSessionError,
    OpenedSession, ReplacedFarewellOutcome, SendToPageError, ServerShutdownReport,
    ServerStartError, SessionControl,
};
pub use sink::{
    ChunkSource, CountingByteSink, CountingSnapshot, EventSinkFull, IntakeRoom, RangeChunk,
    SessionByteSink, SessionChunk, SessionEndReason, SessionEventSink, SessionLifecycleEvent,
    SourceBufferChunk, StreamCounters,
};
pub use token::{OsRandomTokenSource, SessionTokenSource, TokenSourceError};
pub use wire_tap::SessionWireTap;
