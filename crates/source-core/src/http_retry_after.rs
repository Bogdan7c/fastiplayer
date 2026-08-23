//! Secret-safe typed projection HTTP `Retry-After` для higher-level retry policy.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;

/// Нейтральная подсказка сервера о минимальной задержке перед следующим запросом.
///
/// Тип намеренно не хранит исходное значение header-а: source boundary передаёт наружу
/// только проверенную длительность и не расширяет secret/error surfaces сырым payload-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRetryAfter {
    /// Header отсутствовал либо не соответствовал поддерживаемому HTTP синтаксису.
    Unavailable,
    /// Валидный delta-seconds или HTTP-date спроецирован в задержку от момента ответа.
    Delay(Duration),
}

impl HttpRetryAfter {
    /// Возвращает проверенную задержку, если сервер передал понятную подсказку.
    #[must_use]
    pub const fn delay(self) -> Option<Duration> {
        match self {
            Self::Unavailable => None,
            Self::Delay(delay) => Some(delay),
        }
    }
}

/// Извлекает `Retry-After` в момент получения response headers.
pub(crate) fn retry_after_from_headers(
    headers: &HeaderMap,
    observed_at: SystemTime,
) -> HttpRetryAfter {
    let Some(header) = headers.get(RETRY_AFTER) else {
        return HttpRetryAfter::Unavailable;
    };
    let Ok(header_text) = header.to_str() else {
        return HttpRetryAfter::Unavailable;
    };
    parse_retry_after(header_text.trim(), observed_at)
}

/// Разбирает обе нормативные формы: целое delta-seconds и IMF-fixdate.
fn parse_retry_after(header_text: &str, observed_at: SystemTime) -> HttpRetryAfter {
    if let Ok(delay_seconds) = header_text.parse::<u64>() {
        return HttpRetryAfter::Delay(Duration::from_secs(delay_seconds));
    }

    let Ok(retry_at) = OffsetDateTime::parse(header_text, &Rfc2822) else {
        return HttpRetryAfter::Unavailable;
    };
    let Ok(retry_at_seconds) = u64::try_from(retry_at.unix_timestamp()) else {
        return HttpRetryAfter::Delay(Duration::ZERO);
    };
    let Some(retry_at_system_time) = UNIX_EPOCH.checked_add(Duration::from_secs(retry_at_seconds))
    else {
        return HttpRetryAfter::Unavailable;
    };
    let delay = retry_at_system_time
        .duration_since(observed_at)
        .unwrap_or(Duration::ZERO);
    HttpRetryAfter::Delay(delay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    /// Delta-seconds не зависит от wall clock и сохраняется точно.
    #[test]
    fn delta_seconds_is_preserved_exactly() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("2"));

        let retry_after = retry_after_from_headers(&headers, UNIX_EPOCH);

        assert_eq!(retry_after, HttpRetryAfter::Delay(Duration::from_secs(2)));
    }

    /// IMF-fixdate вычисляется относительно явно переданного момента ответа.
    #[test]
    fn http_date_is_projected_to_relative_delay() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
        );
        let observed_at = UNIX_EPOCH + Duration::from_secs(784_111_775);

        let retry_after = retry_after_from_headers(&headers, observed_at);

        assert_eq!(retry_after, HttpRetryAfter::Delay(Duration::from_secs(2)));
    }

    /// Истёкшая абсолютная дата не создаёт отрицательную или огромную задержку.
    #[test]
    fn past_http_date_becomes_zero_delay() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
        );
        let observed_at = UNIX_EPOCH + Duration::from_secs(784_111_778);

        let retry_after = retry_after_from_headers(&headers, observed_at);

        assert_eq!(retry_after, HttpRetryAfter::Delay(Duration::ZERO));
    }

    /// Повреждённый server header безопасно возвращает отсутствие hint-а.
    #[test]
    fn malformed_header_is_unavailable() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("definitely-not-a-delay"),
        );

        let retry_after = retry_after_from_headers(&headers, UNIX_EPOCH);

        assert_eq!(retry_after, HttpRetryAfter::Unavailable);
    }
}
