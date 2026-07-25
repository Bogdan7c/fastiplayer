//! Секретосодержащая identity progressive FTP(S) source-а.

use std::fmt;

/// Exact FTP(S) locator без обычного строкового accessor-а.
///
/// Raw identity раскрывается только в точке реального FTP open/login/RETR.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretFtpUrl(String);

impl SecretFtpUrl {
    /// Принимает уже проверенную identity для FTP открытия.
    #[must_use]
    pub fn from_secret_for_open(secret_url: impl Into<String>) -> Self {
        Self(secret_url.into())
    }

    /// Раскрывает identity исключительно для выполнения FTP команды.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        &self.0
    }

    /// Строит стабильный opaque hash без выдачи raw identity.
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

impl fmt::Debug for SecretFtpUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretFtpUrl(<redacted>)")
    }
}

impl fmt::Display for SecretFtpUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FTP source <redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::SecretFtpUrl;

    #[test]
    fn formatting_redacts_userinfo_path_and_query() {
        let locator = SecretFtpUrl::from_secret_for_open(
            "ftps://user:password@media.invalid/private/video.webm?token=secret",
        );

        assert_eq!(format!("{locator}"), "FTP source <redacted>");
        assert_eq!(format!("{locator:?}"), "SecretFtpUrl(<redacted>)");
        let formatted = format!("{locator:?} {locator}");
        for secret in ["user", "password", "private", "token", "secret"] {
            assert!(!formatted.contains(secret));
        }
    }

    #[test]
    fn open_exposure_preserves_exact_identity() {
        let secret = "ftp://media.invalid/video.webm";
        let locator = SecretFtpUrl::from_secret_for_open(secret);
        assert_eq!(locator.expose_secret_for_open(), secret);
    }

    #[test]
    fn identity_hash_is_stable_without_raw_string_exposure() {
        let first = SecretFtpUrl::from_secret_for_open("ftp://media.invalid/a");
        let second = SecretFtpUrl::from_secret_for_open("ftp://media.invalid/a");
        let different = SecretFtpUrl::from_secret_for_open("ftp://media.invalid/b");
        assert_eq!(first.stable_identity_hash(), second.stable_identity_hash());
        assert_ne!(
            first.stable_identity_hash(),
            different.stable_identity_hash()
        );
    }
}
