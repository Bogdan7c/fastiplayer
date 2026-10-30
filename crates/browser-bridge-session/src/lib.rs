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

#[cfg(test)]
mod test_support;

pub use entry_desk::{EntryCaller, EntryCallerError, EntryDesk, EntryEvent, EntryOutcome};
pub use lifecycle::{
    ExitCause, LifecycleVerdict, PlaybackDrainOutcome, ProcessLifecycle, ends_with_playback_drain,
};
pub use policy::BrowserSessionPolicy;
pub use registry::{ConnectionId, SessionInstance, SessionRef};
pub use server::{
    BrowserSessionServer, CloseOutcome, OpenSessionError, OpenedSession, SendToPageError,
    ServerShutdownReport, ServerStartError, SessionControl,
};
pub use sink::{
    ChunkSource, CountingByteSink, CountingSnapshot, EventSinkFull, RangeChunk, SessionByteSink,
    SessionChunk, SessionEndReason, SessionEventSink, SessionLifecycleEvent, SourceBufferChunk,
    StreamCounters,
};
pub use token::{OsRandomTokenSource, SessionTokenSource, TokenSourceError};
