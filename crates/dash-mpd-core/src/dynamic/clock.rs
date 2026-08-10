//! Pure UTC timing model без network ownership и secret-bearing diagnostics.

use std::fmt;

use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;

/// Inline UTC synchronization scheme из ISO/IEC 23009-1.
pub const DASH_DIRECT_UTC_SCHEME: &str = "urn:mpeg:dash:utc:direct:2014";

/// HTTP XSDATE synchronization scheme из ISO/IEC 23009-1.
pub const DASH_HTTP_XSDATE_UTC_SCHEME: &str = "urn:mpeg:dash:utc:http-xsdate:2014";

/// UTC timestamp без исходного XML/HTTP текста и без floating-point арифметики.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashUtcTimestamp {
    unix_nanoseconds: i128,
}

impl DashUtcTimestamp {
    /// Создаёт timestamp из clock/test boundary.
    #[must_use]
    pub const fn from_unix_nanoseconds(unix_nanoseconds: i128) -> Self {
        Self { unix_nanoseconds }
    }

    /// Разбирает bounded HTTP XSDATE body, разрешая только внешние ASCII-пробелы.
    pub fn parse_xs_datetime_response(
        response_body: &[u8],
    ) -> Result<Self, DashUtcTimestampParseError> {
        let response_text = std::str::from_utf8(response_body)
            .map_err(|_| DashUtcTimestampParseError::InvalidEncoding)?;
        let timestamp_text =
            response_text.trim_matches(|character: char| character.is_ascii_whitespace());
        if timestamp_text.is_empty() {
            return Err(DashUtcTimestampParseError::InvalidTimestamp);
        }
        Self::parse_iso8601(timestamp_text)
    }

    /// Возвращает точное число nanoseconds относительно Unix epoch.
    #[must_use]
    pub const fn unix_nanoseconds(self) -> i128 {
        self.unix_nanoseconds
    }

    /// Разбирает один ISO 8601 timestamp без сохранения исходного текста.
    pub(super) fn parse_iso8601(value: &str) -> Result<Self, DashUtcTimestampParseError> {
        let parsed = OffsetDateTime::parse(value, &Iso8601::DEFAULT)
            .map_err(|_| DashUtcTimestampParseError::InvalidTimestamp)?;
        Ok(Self::from_unix_nanoseconds(parsed.unix_timestamp_nanos()))
    }
}

impl fmt::Debug for DashUtcTimestamp {
    /// Не отражает исходный UTC payload в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DashUtcTimestamp(<redacted>)")
    }
}

/// Secret-safe ошибка разбора inline либо HTTP XSDATE timestamp-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashUtcTimestampParseError {
    /// HTTP body не является UTF-8.
    #[error("DASH UTC response encoding is invalid")]
    InvalidEncoding,
    /// Payload не содержит ровно один допустимый ISO 8601 timestamp.
    #[error("DASH UTC timestamp is invalid")]
    InvalidTimestamp,
}

/// Bounded URI reference внешнего UTC clock resource-а.
#[derive(Clone, PartialEq, Eq)]
pub struct DashUtcTimingResource {
    reference: String,
}

impl DashUtcTimingResource {
    /// Проверяет lexical URI reference до network-specific resolution.
    pub(super) fn new(reference: String) -> Result<Self, DashUtcTimingResourceError> {
        if reference.is_empty()
            || reference
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(DashUtcTimingResourceError);
        }
        Ok(Self { reference })
    }

    /// Возвращает exact reference только transport owner-у для разрешения относительно MPD.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

impl fmt::Debug for DashUtcTimingResource {
    /// Clock locator не попадает в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DashUtcTimingResource(<redacted>)")
    }
}

/// Lexical ошибка clock URI reference без исходного locator-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("DASH UTC timing resource reference is invalid")]
pub(super) struct DashUtcTimingResourceError;

/// Pure clock descriptor; HTTP I/O остаётся ответственностью runtime provider-а.
#[derive(Clone, PartialEq, Eq)]
pub enum DashUtcTiming {
    /// Inline UTC sample, наблюдаемый вместе с MPD response-ом.
    Direct(DashUtcTimestamp),
    /// Отдельный bounded HTTP XSDATE resource без унаследованных source secrets.
    HttpXsDate(DashUtcTimingResource),
}

impl DashUtcTiming {
    /// Возвращает inline sample только для unit/pure timing consumers.
    #[must_use]
    pub const fn direct_timestamp(&self) -> Option<DashUtcTimestamp> {
        match self {
            Self::Direct(timestamp) => Some(*timestamp),
            Self::HttpXsDate(_) => None,
        }
    }
}

impl fmt::Debug for DashUtcTiming {
    /// Ни inline timestamp, ни HTTP reference не попадают в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(_) => formatter.write_str("DashUtcTiming::Direct(<redacted>)"),
            Self::HttpXsDate(_) => formatter.write_str("DashUtcTiming::HttpXsDate(<redacted>)"),
        }
    }
}
