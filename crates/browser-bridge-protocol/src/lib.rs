//! Протокол моста Fastiplayer Bridge: экстеншен ↔ посредник ↔ плеер (issue #2).
//!
//! Главная идея (решение 37): **браузер качает — мы играем**. Экстеншен
//! перехватывает куски видео, которые плеер страницы кормит видеоэлементу
//! (MSE), и отдаёт их плееру. Плеер разбирает, декодирует и показывает.
//!
//! Две дороги (решения 82 и 85):
//!
//! ```text
//!  экстеншен ── native messaging ──► посредник ── D-Bus ──► плеер
//!            ◄─────────────────────            ◄──────────
//!     участок «вход» (crate::control): hello, open_session → порт и пропуск
//!
//!  background экстеншена ════ WebSocket 127.0.0.1 ════► процесс браузерного режима
//!     участок «сеанс» (crate::session, crate::data_frame):
//!     куски байтов, MSE-события, часы, команды, события страницы
//! ```
//!
//! Посредник — «звонок в дверь»: будит плеер и выдаёт порт с одноразовым
//! пропуском. Всё живое идёт напрямую, поэтому посредник может умереть посреди
//! фильма, а сеанс продолжится (инвариант 3).
//!
//! Crate содержит только типы, разбор и чистые правила. Транспорта (stdio,
//! D-Bus, сокеты) здесь нет: он у каждой ОС свой (сессии 02 и 05), а протокол
//! один — шов под Windows.
//!
//! Политика совместимости — «договорились — значит строго» (решение 86,
//! `crate::version`). Общие фикстуры для Rust и TypeScript — в `fixtures/`.
//!
//! Версия 1.1 (решение 228) добавила нативный HLS по номеркам ресурсов:
//! `crate::resource`, `crate::resource_address`; 1.2 (решение 232) — окно эфира
//! `live_window`; 1.3 (решение 237) — значение `tab_gone{page_player_failed}`;
//! 1.4 (решения 240–241) — возврат вкладки: `page_left`, `page_returned`,
//! `page_reopen_failed`, `reopen_page` (`crate::session::page_reopen`).
//! История версий — `crate::version`.

pub mod capability;
pub mod codec;
pub mod control;
pub mod data_frame;
pub mod error;
pub mod extension_identity;
pub mod limits;
pub mod open_session;
pub mod resource;
pub mod resource_address;
pub mod session;
pub mod stream;
pub mod stream_gate;
mod strict_shape;
pub mod text;
pub mod time;
pub mod version;

pub use capability::{OptionalFeature, ParsedCapabilities, PeerCapabilities, VideoCodecFamily};
pub use codec::{ProtocolMessage, decode_message, encode_message};
pub use control::{
    ControlRefusal, ControlRequestId, ExtensionControlMessage, GrantOutcome, Hello, HelloAccepted,
    LoopbackPort, OpenSessionRequest, PageIdentity, PlaybackHints, PlayerControlMessage,
    PositionHint, RefusalReason, ReplyTarget, SessionGrant, SessionIntent,
};
pub use data_frame::{
    DataFrame, DataFrameError, DataFrameHeader, RangeBytesHeader, SourceBufferBytesHeader,
    decode_data_frame, encode_data_frame,
};
pub use error::{MessageError, MessageErrorKind, ValueViolation};
pub use open_session::{OpenSessionDecision, decide_open_session};
pub use resource::{
    HttpStatusCode, ResourceBodySize, ResourceByteRange, ResourceFetchFailure, ResourceFetchLength,
    ResourceFetchOutcome, ResourceHandle,
};
pub use resource_address::{
    RESOURCE_ADDRESS_HOST, format_resource_address, parse_resource_address,
};
pub use session::{LiveSeekable, PageSessionMessage, PlayerSessionMessage, SessionCloseReason};
pub use stream::{AppendSequence, RangeRequestId, SeekId, SourceBufferIndex, StreamGeneration};
pub use stream_gate::{GateDecision, StreamGate, StreamViolation};
pub use text::{
    CapabilityToken, MimeType, PageTitle, SessionKey, SessionToken, SiteDomain, SoftwareVersion,
    VariantId, VariantLabel,
};
pub use time::{
    ByteOffset, MediaDuration, MediaTime, MediaTimeOffset, PlaybackRate, RangeFetchLength,
};
pub use version::{
    BridgeSoftware, CURRENT_PROTOCOL_VERSION, IncompatibleVersion, ProtocolVersion,
    negotiate_protocol_version,
};
