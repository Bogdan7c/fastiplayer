//! Ресурсы HLS по номеркам (протокол 1.1, решение 228).
//!
//! Плеер ведёт HLS сам (движок `web-media-hls`), а качает каждый ресурс —
//! плейлист, init-сегмент, медиасегмент, ключ AES — фон экстеншена сетью браузера.
//! Настоящих адресов плеер не видит: фон переписывает их в плейлистах на номерки
//! [`ResourceHandle`], а в тексте плейлиста номерок записан адресом-заглушкой
//! (`crate::resource_address`).
//!
//! Обмен одного ресурса:
//! 1. плеер → `fetch_resource{request_id, resource, byte_range, max_bytes}`;
//! 2. страница → байты тела рамками вида 2 (`crate::data_frame`,
//!    `RangeBytes{request_id, offset}`), где `offset` — смещение **внутри тела
//!    ответа**, с 0 (а не в файле на сервере, даже для `byte_range: slice`);
//! 3. страница → `resource_fetch_finished{request_id, outcome}`.
//!
//! Номера запросов (`RangeRequestId`) — одно пространство с `fetch_range`: плеер
//! ведёт один счётчик на сеанс, поэтому по номеру рамки ясно, чей это ответ.

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::MAX_RESOURCE_FETCH_BYTES;
use crate::strict_shape::strict_object;
use crate::time::ByteOffset;

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(
    ResourceByteRange,
    ResourceFetchOutcome,
    ResourceFetchFailure
);

/// Наименьший код ответа HTTP, который бывает (`100 Continue`).
const MIN_HTTP_STATUS: u16 = 100;
/// Наибольший код ответа HTTP по RFC 9110 (класс 5xx).
const MAX_HTTP_STATUS: u16 = 599;

/// Номерок ресурса HLS: его выдал фон экстеншена, переписывая плейлист.
///
/// Весь диапазон `u32` допустим: номерок непрозрачен для плеера, а фон сам
/// решает, с какого числа считать (корневой плейлист обычно 0). Четыре
/// миллиарда ресурсов за сеанс не наберутся даже у суточного live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceHandle(pub u32);

/// Сколько байт тела ресурса: длина куска `slice` и потолок `max_bytes`, от 1 до
/// [`MAX_RESOURCE_FETCH_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ResourceFetchLength(u64);

impl ResourceFetchLength {
    /// Длина из числа байт.
    ///
    /// # Errors
    /// Ноль (бессмысленный запрос) или больше лимита одного ресурса.
    pub const fn new(bytes: u64) -> Result<Self, ValueViolation> {
        if bytes == 0 || bytes > MAX_RESOURCE_FETCH_BYTES {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(bytes))
    }

    /// Число байт.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ResourceFetchLength {
    type Error = ValueViolation;

    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl From<ResourceFetchLength> for u64 {
    fn from(length: ResourceFetchLength) -> Self {
        length.0
    }
}

/// Сколько байт тела ресурса отдано рамками: от 0 (пустой ответ бывает, например
/// пустой ключ или пустой live-плейлист в редкий миг) до [`MAX_RESOURCE_FETCH_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ResourceBodySize(u64);

impl ResourceBodySize {
    /// Размер из числа байт.
    ///
    /// # Errors
    /// Больше лимита одного ресурса: столько экстеншен не отдаёт по договору.
    pub const fn new(bytes: u64) -> Result<Self, ValueViolation> {
        if bytes > MAX_RESOURCE_FETCH_BYTES {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(bytes))
    }

    /// Число байт.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ResourceBodySize {
    type Error = ValueViolation;

    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl From<ResourceBodySize> for u64 {
    fn from(size: ResourceBodySize) -> Self {
        size.0
    }
}

/// Код ответа HTTP, от 100 до 599 (RFC 9110, раздел 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct HttpStatusCode(u16);

impl HttpStatusCode {
    /// Код из числа.
    ///
    /// # Errors
    /// Вне 100..=599: таких кодов HTTP не бывает, значит отправитель ошибся.
    pub const fn new(code: u16) -> Result<Self, ValueViolation> {
        if code < MIN_HTTP_STATUS || code > MAX_HTTP_STATUS {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(code))
    }

    /// Код числом.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for HttpStatusCode {
    type Error = ValueViolation;

    fn try_from(code: u16) -> Result<Self, Self::Error> {
        Self::new(code)
    }
}

impl From<HttpStatusCode> for u16 {
    fn from(code: HttpStatusCode) -> Self {
        code.0
    }
}

/// Какую часть ресурса просит плеер.
///
/// Не необязательное поле, а явная метка: в протоколе нет полей «может не быть»,
/// и `null`/пропуск не путаются с «весь ресурс» ни в Rust, ни в TypeScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceByteRange {
    /// Весь ресурс (плейлист, ключ, сегмент без `EXT-X-BYTERANGE`).
    Whole {},
    /// Кусок ресурса (`EXT-X-BYTERANGE`): фон качает его заголовком `Range`.
    Slice {
        /// С какого байта ресурса на сервере.
        offset: ByteOffset,
        /// Сколько байт.
        length: ResourceFetchLength,
    },
}

/// Почему ресурс не получен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceFetchFailure {
    /// Сеть недоступна, соединение оборвалось или запрос отменён.
    NetworkError {},
    /// Сервер ответил ошибкой HTTP (403, 404, 5xx…).
    HttpError {
        /// Код ответа.
        status: HttpStatusCode,
    },
    /// Фон отказался качать: номерок не его, предел одновременных запросов
    /// ([`crate::limits::MAX_CONCURRENT_RESOURCE_FETCHES`]), защита локальной
    /// сети (решение 199) или фон ещё не умеет HLS.
    Refused {},
    /// Тело ответа длиннее `max_bytes` запроса.
    TooLarge {},
}

/// Чем кончился запрос ресурса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceFetchOutcome {
    /// Всё тело отправлено рамками вида 2 (смещения с 0 подряд).
    Completed {
        /// Сколько байт тела отправлено.
        size_bytes: ResourceBodySize,
    },
    /// Ресурс не получен.
    Failed {
        /// Почему.
        failure: ResourceFetchFailure,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_length_accepts_one_byte_to_the_limit_only() {
        assert_eq!(
            ResourceFetchLength::new(0),
            Err(ValueViolation::NumberOutOfRange)
        );
        assert_eq!(
            ResourceFetchLength::new(1).map(ResourceFetchLength::as_bytes),
            Ok(1)
        );
        assert_eq!(
            ResourceFetchLength::new(MAX_RESOURCE_FETCH_BYTES).map(ResourceFetchLength::as_bytes),
            Ok(MAX_RESOURCE_FETCH_BYTES)
        );
        assert_eq!(
            ResourceFetchLength::new(MAX_RESOURCE_FETCH_BYTES + 1),
            Err(ValueViolation::NumberOutOfRange)
        );
    }

    #[test]
    fn body_size_allows_an_empty_body_but_not_more_than_the_limit() {
        assert_eq!(
            ResourceBodySize::new(0).map(ResourceBodySize::as_bytes),
            Ok(0)
        );
        assert_eq!(
            ResourceBodySize::new(MAX_RESOURCE_FETCH_BYTES + 1),
            Err(ValueViolation::NumberOutOfRange)
        );
    }

    #[test]
    fn http_status_is_limited_to_real_status_classes() {
        for code in [99, 600, 0, u16::MAX] {
            assert_eq!(
                HttpStatusCode::new(code),
                Err(ValueViolation::NumberOutOfRange),
                "{code}"
            );
        }
        for code in [100, 200, 403, 404, 599] {
            assert_eq!(HttpStatusCode::new(code).map(HttpStatusCode::get), Ok(code));
        }
    }

    #[test]
    fn resource_limits_are_the_documented_contract_numbers() {
        // Числа закреплены: их же зеркалит экстеншен (`limits.ts`).
        assert_eq!(MAX_RESOURCE_FETCH_BYTES, 134_217_728);
        assert_eq!(crate::limits::MAX_CONCURRENT_RESOURCE_FETCHES, 8);
    }
}
