//! Ошибки очереди запросов ресурсов и их классы.
//!
//! Два мира ошибок, которые нельзя смешивать:
//! - **нарушение страницы** ([`ResourceFetchViolation`]): внешняя сторона нарушила
//!   договор (куски не подряд, лишние байты, выдуманный номер). Ядро очереди после
//!   этого мертво для всех, приложение закрывает сеанс;
//! - **отказ ресурса** (сеть, HTTP, отказ фона, слишком большой, молчание): ресурс не
//!   получен, но очередь жива. Движок решает сам — повторить или честно упасть.

use std::time::Duration;

/// Нарушение договора внешней стороной. Первое нарушение побеждает и видно всем.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceFetchViolation {
    /// Номер, которого не выдавал никто в сеансе.
    #[error("resource request {id} was never issued")]
    UnissuedRequest {
        /// Номер из рамки.
        id: u32,
    },
    /// Номер очереди, по которому сейчас ничего не ждут: запрос ещё не отправлен,
    /// уже не был отправлен никогда или страница его уже закончила.
    #[error("resource request {id} is not open")]
    RequestNotOpen {
        /// Номер из рамки.
        id: u32,
    },
    /// Кусок начинается не там, где кончился прежний.
    #[error("resource request {id}: bytes start at offset {actual}, expected {expected}")]
    NonContiguousBytes {
        /// Номер запроса.
        id: u32,
        /// Где должен был начаться кусок.
        expected: u64,
        /// Где начался.
        actual: u64,
    },
    /// Тело больше `max_bytes` запроса (или длины диапазона).
    #[error("resource request {id}: body grows past its limit of {limit} bytes")]
    BytesBeyondLimit {
        /// Номер запроса.
        id: u32,
        /// Потолок тела.
        limit: u64,
    },
    /// Итог «завершён» сообщает другой размер, чем пришло байтов.
    #[error("resource request {id}: completed with {reported} bytes, but {received} arrived")]
    SizeMismatch {
        /// Номер запроса.
        id: u32,
        /// Размер в итоге.
        reported: u64,
        /// Сколько байтов пришло.
        received: u64,
    },
    /// Байты или второй итог после итога.
    #[error("resource request {id} was already finished")]
    AlreadyFinished {
        /// Номер запроса.
        id: u32,
    },
}

/// Ошибка, которую получает движок.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceFetchError {
    /// Движок сам отменил ожидание.
    #[error("the resource fetch was cancelled")]
    Cancelled,
    /// Внешняя сторона сообщила о сбое сети.
    #[error("the resource fetch failed with a network error")]
    Network,
    /// Внешняя сторона молчала дольше срока (ни байта с отправки или между кусками).
    #[error("the page sent nothing for {idle_timeout:?}")]
    PageSilent {
        /// Срок молчания политики.
        idle_timeout: Duration,
    },
    /// Сервер ответил HTTP-ошибкой.
    #[error("the resource fetch failed with HTTP status {status}")]
    Http {
        /// Статус HTTP.
        status: u16,
    },
    /// Внешняя сторона отказалась качать ресурс (у моста — фон отказал).
    #[error("the page refused to fetch the resource")]
    Refused,
    /// Ресурс больше `max_bytes` (так сказала внешняя сторона).
    #[error("the resource is larger than the requested limit")]
    TooLarge,
    /// Диапазон пришёл короче запрошенного: всё пришедшее уже отдано, это — конец.
    ///
    /// Не нарушение страницы: сервер вправе ответить меньше (ресурс короче, чем
    /// думал плейлист). Читатель-переходник обычно превращает это в `UnexpectedEof`.
    #[error("the byte range ended after {received} of {requested} bytes")]
    ShortRange {
        /// Длина запрошенного диапазона.
        requested: u64,
        /// Сколько пришло.
        received: u64,
    },
    /// Источник закрыт (вкладка ушла, поколение кончилось): больше запросов не будет.
    #[error("the resource source was closed")]
    SourceClosed,
    /// Внешняя сторона нарушила договор: очередь мертва.
    #[error("the page violated the resource request contract: {0}")]
    PageViolation(ResourceFetchViolation),
    /// Движок попросил больше, чем разрешает политика очереди.
    #[error("requested limit {requested} exceeds the resource ceiling {ceiling}")]
    ExceedsResourceCeiling {
        /// Что попросили.
        requested: u64,
        /// Потолок политики.
        ceiling: u64,
    },
    /// Номера запросов сеанса кончились.
    #[error("the session ran out of request numbers")]
    RequestIdsExhausted,
    /// Не удалось запустить сторожа молчания для асинхронного ожидания.
    #[error("the resource silence watchdog could not start: {reason}")]
    SilenceWatchdogUnavailable {
        /// Причина системы.
        reason: String,
    },
}

/// Грубый класс ошибки: что с ней делать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFetchFailureClass {
    /// Внешняя сторона нарушила договор: приложение закрывает сеанс (у моста — 4003).
    PageViolation,
    /// Временный сбой (сеть, молчание): повтор имеет смысл.
    Transient,
    /// Ресурс не отдан окончательно (HTTP, отказ, слишком большой, короткий диапазон):
    /// движок решает сам, ошибка честная.
    Rejected,
    /// Источник закрыт или исчерпан: запросов больше не будет.
    Closed,
    /// Ожидание отменил сам движок.
    Cancelled,
    /// Движок попросил недопустимое.
    InvalidRequest,
}

impl ResourceFetchError {
    /// Класс ошибки.
    #[must_use]
    pub const fn failure_class(&self) -> ResourceFetchFailureClass {
        match self {
            Self::PageViolation(_) => ResourceFetchFailureClass::PageViolation,
            Self::Network | Self::PageSilent { .. } | Self::SilenceWatchdogUnavailable { .. } => {
                ResourceFetchFailureClass::Transient
            }
            Self::Http { .. } | Self::Refused | Self::TooLarge | Self::ShortRange { .. } => {
                ResourceFetchFailureClass::Rejected
            }
            Self::SourceClosed | Self::RequestIdsExhausted => ResourceFetchFailureClass::Closed,
            Self::Cancelled => ResourceFetchFailureClass::Cancelled,
            Self::ExceedsResourceCeiling { .. } => ResourceFetchFailureClass::InvalidRequest,
        }
    }
}
