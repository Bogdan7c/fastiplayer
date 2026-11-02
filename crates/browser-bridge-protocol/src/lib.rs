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

pub mod capability;
pub mod codec;
pub mod control;
pub mod data_frame;
pub mod error;
pub mod extension_identity;
pub mod limits;
pub mod open_session;
pub mod session;
pub mod stream;
pub mod stream_gate;
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
pub use session::{PageSessionMessage, PlayerSessionMessage, SessionCloseReason};
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
