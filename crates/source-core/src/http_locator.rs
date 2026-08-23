use std::fmt;

/// Секретосодержащая identity HTTP source-а.
///
/// Тип намеренно не предоставляет обычный строковый accessor: transport-код
/// получает исходное значение только в точке реального HTTP open/read.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretHttpUrl(String);

impl SecretHttpUrl {
    /// Принимает уже проверенную service-owner-ом identity для HTTP открытия.
    #[must_use]
    pub fn from_secret_for_open(secret_url: impl Into<String>) -> Self {
        Self(secret_url.into())
    }

    /// Раскрывает identity исключительно для выполнения HTTP запроса.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        &self.0
    }

    /// Строит стабильный opaque hash без выдачи raw identity вызывающему коду.
    pub(crate) fn stable_identity_hash(&self) -> u64 {
        const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

        self.0
            .as_bytes()
            .iter()
            .fold(FNV_OFFSET_BASIS, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
            })
    }
}

impl fmt::Debug for SecretHttpUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretHttpUrl(<redacted>)")
    }
}

impl fmt::Display for SecretHttpUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HTTP source <redacted>")
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use crate::SourceError;

    use super::SecretHttpUrl;

    #[test]
    fn formatting_redacts_userinfo_query_and_fragment() {
        let locator = SecretHttpUrl::from_secret_for_open(
            "https://user:password@example.test/video.mp4?token=secret#private",
        );

        assert_eq!(format!("{locator}"), "HTTP source <redacted>");
        assert_eq!(format!("{locator:?}"), "SecretHttpUrl(<redacted>)");
    }

    #[test]
    fn open_exposure_preserves_exact_identity() {
        let secret = "https://example.test/video.mp4?signature=a%2Bb&part=1+2";
        let locator = SecretHttpUrl::from_secret_for_open(secret);

        assert_eq!(locator.expose_secret_for_open(), secret);
    }

    #[test]
    fn source_error_chain_context_uses_only_redacted_locator() {
        let error = SourceError::HttpStatus {
            operation: "range-probe",
            url: SecretHttpUrl::from_secret_for_open(
                "https://user:password@example.test/video?token=secret",
            ),
            status: StatusCode::FORBIDDEN,
            retry_after: crate::HttpRetryAfter::Unavailable,
        };
        let formatted = format!("{error:?} {error}");

        assert!(!formatted.contains("password"));
        assert!(!formatted.contains("secret"));
        assert!(!formatted.contains("example.test"));
        assert!(formatted.contains("redacted"));
    }

    #[test]
    fn identity_hash_is_stable_without_raw_string_exposure() {
        let first = SecretHttpUrl::from_secret_for_open("https://example.test?v=secret");
        let second = SecretHttpUrl::from_secret_for_open("https://example.test?v=secret");
        let different = SecretHttpUrl::from_secret_for_open("https://example.test?v=other");

        assert_eq!(first.stable_identity_hash(), second.stable_identity_hash());
        assert_ne!(
            first.stable_identity_hash(),
            different.stable_identity_hash()
        );
    }
}
