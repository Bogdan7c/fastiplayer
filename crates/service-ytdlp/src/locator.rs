use std::fmt;

use thiserror::Error;
use url::Url;

/// Стабильный locator страницы media, которым владеет generic `yt-dlp` adapter.
///
/// Внутри хранится исходная строка byte-for-byte. Это важно для signed URL:
/// повторная сериализация через `url::Url` могла бы изменить percent-encoding
/// или порядок query-параметров и тем самым сломать transport identity.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpMediaLocator {
    exact_secret_identity: String,
    safe_label: String,
    input_scheme: YtDlpInputScheme,
    requires_sensitive_durable_locator_acknowledgement: bool,
}

impl YtDlpMediaLocator {
    /// Возвращает exact typed input scheme без смешивания с transport availability.
    #[must_use]
    pub const fn input_scheme(&self) -> YtDlpInputScheme {
        self.input_scheme
    }

    /// Раскрывает exact identity только для service open/refresh.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        &self.exact_secret_identity
    }

    /// Раскрывает exact identity только для persistence.
    #[must_use]
    pub fn expose_secret_for_persistence(&self) -> &str {
        &self.exact_secret_identity
    }

    /// Возвращает bounded label без userinfo/path/query/fragment.
    #[must_use]
    pub fn safe_label(&self) -> &str {
        &self.safe_label
    }

    /// Сообщает persistence owner-у, что exact durable locator требует подтверждения.
    #[must_use]
    pub const fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.requires_sensitive_durable_locator_acknowledgement
    }

    /// Сообщает export owner-у, что portable document потребует подтверждения.
    #[must_use]
    pub const fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.requires_sensitive_durable_locator_acknowledgement
    }
}

impl fmt::Debug for YtDlpMediaLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpMediaLocator")
            .field("safe_label", &self.safe_label)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for YtDlpMediaLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.safe_label)
    }
}

/// Exact top-level input schemes, подтверждённые compatibility profile S00.
///
/// Этот enum описывает только syntax/admission vocabulary. Наличие transport
/// provider-а проверяет composition root, поэтому pure parser не обещает, что
/// FTP(S) или RTMP уже можно открыть в текущей сборке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YtDlpInputScheme {
    /// Обычный HTTP input; direct-media classifier получает первый приоритет.
    Http,

    /// Защищённый HTTP input; direct-media classifier получает первый приоритет.
    Https,

    /// FTP input из exact S00 target row.
    Ftp,

    /// FTPS input из exact S00 target row.
    Ftps,

    /// Обычный RTMP input из initial S00 candidate set.
    Rtmp,

    /// Шифрованный RTMPE input из initial S00 candidate set.
    Rtmpe,
}

impl YtDlpInputScheme {
    /// Возвращает canonical scheme spelling только для безопасных diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
            Self::Rtmp => "rtmp",
            Self::Rtmpe => "rtmpe",
        }
    }

    /// HTTP(S) остаются always-admitted generic yt-dlp fallback-ом.
    #[must_use]
    pub const fn is_http_fallback(self) -> bool {
        matches!(self, Self::Http | Self::Https)
    }

    fn parse_exact_approved(parsed_scheme: &str) -> Option<Self> {
        match parsed_scheme {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "ftp" => Some(Self::Ftp),
            "ftps" => Some(Self::Ftps),
            "rtmp" => Some(Self::Rtmp),
            "rtmpe" => Some(Self::Rtmpe),
            _ => None,
        }
    }
}

/// Ошибка pure parse/classification без отражения исходного input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum YtDlpLocatorParseError {
    /// Строка не является absolute URL.
    #[error("некорректный URL для media service")]
    InvalidSyntax,

    /// Схема URL отсутствует в exact S00-approved input set.
    #[error("URL scheme не входит в утверждённый media-service profile")]
    UnsupportedScheme,
}

/// Классифицирует exact S00-approved absolute URL без network I/O.
pub fn parse_yt_dlp_media_locator(
    argument: &str,
) -> Result<YtDlpMediaLocator, YtDlpLocatorParseError> {
    let parsed_url = Url::parse(argument).map_err(|_| YtDlpLocatorParseError::InvalidSyntax)?;
    let input_scheme = YtDlpInputScheme::parse_exact_approved(parsed_url.scheme())
        .ok_or(YtDlpLocatorParseError::UnsupportedScheme)?;
    if parsed_url.host().is_none() {
        return Err(YtDlpLocatorParseError::InvalidSyntax);
    }

    let safe_label = yt_dlp_safe_label(&parsed_url);
    let requires_sensitive_durable_locator_acknowledgement = !parsed_url.username().is_empty()
        || parsed_url.password().is_some()
        || parsed_url.query().is_some_and(|query| !query.is_empty());

    Ok(YtDlpMediaLocator {
        exact_secret_identity: argument.to_owned(),
        safe_label,
        input_scheme,
        requires_sensitive_durable_locator_acknowledgement,
    })
}

/// Exact direct-stream URL, который может содержать подпись и expiry query.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpDirectStreamUrl(String);

impl YtDlpDirectStreamUrl {
    /// Принимает exact signed identity для последующего HTTP open.
    #[must_use]
    pub fn from_secret_for_open(secret_url: impl Into<String>) -> Self {
        Self(secret_url.into())
    }

    /// Раскрывает signed identity только transport boundary.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for YtDlpDirectStreamUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("YtDlpDirectStreamUrl(<redacted>)")
    }
}

impl fmt::Display for YtDlpDirectStreamUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("YtDlp direct stream <redacted>")
    }
}

fn yt_dlp_safe_label(url: &Url) -> String {
    let host = url.host_str().unwrap_or("unknown-host");
    format!("yt-dlp media ({host})")
}

#[cfg(test)]
mod tests {
    use super::{YtDlpInputScheme, YtDlpLocatorParseError, parse_yt_dlp_media_locator};

    #[test]
    fn generic_url_preserves_exact_identity_without_query_normalization() {
        let exact_url =
            "https://media.example.test/watch/%2Fopaque?token=a%2Bb&utm_source=keep#chapter";
        let locator =
            parse_yt_dlp_media_locator(exact_url).expect("generic HTTP(S) URL должен пройти parse");

        assert_eq!(locator.expose_secret_for_persistence(), exact_url);
        assert_eq!(locator.expose_secret_for_open(), exact_url);
        assert!(
            locator.requires_sensitive_export_acknowledgement(),
            "query-bearing exact identity обязана пройти aggregated acknowledgement"
        );
        assert!(
            locator.requires_sensitive_persistence_acknowledgement(),
            "durable persistence использует ту же aggregated acknowledgement policy"
        );
    }

    #[test]
    fn public_generic_url_does_not_require_sensitive_acknowledgement() {
        let locator = parse_yt_dlp_media_locator("https://media.example.test/watch/123")
            .expect("public generic URL должен пройти parse");

        assert!(!locator.requires_sensitive_export_acknowledgement());
    }

    #[test]
    fn formatting_never_exposes_userinfo_query_or_path() {
        let locator = parse_yt_dlp_media_locator(
            "https://user:password@youtu.be/private-id?v=secret&unknown=keep#fragment",
        )
        .expect("generic HTTP(S) URL должен пройти parse");

        assert_eq!(format!("{locator}"), "yt-dlp media (youtu.be)");
        assert!(!format!("{locator:?}").contains("secret"));
        assert!(!format!("{locator:?}").contains("password"));
        assert!(!format!("{locator:?}").contains("private-id"));
    }

    #[test]
    fn invalid_syntax_error_does_not_reflect_input() {
        let secret_input = "https://user:password@[invalid]?token=secret";
        let error = parse_yt_dlp_media_locator(secret_input)
            .expect_err("invalid syntax должна возвращать typed error");

        assert_eq!(error, YtDlpLocatorParseError::InvalidSyntax);
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(!format!("{error:?} {error}").contains("password"));
    }

    #[test]
    fn exact_s00_input_schemes_are_preserved_without_alias_normalization() {
        assert!(parse_yt_dlp_media_locator("https://video.example.test/watch/42").is_ok());
        assert!(parse_yt_dlp_media_locator("https://www.youtube.com/watch?v=42").is_ok());

        for (raw_locator, expected_scheme) in [
            ("ftp://media.example.test/video.webm", YtDlpInputScheme::Ftp),
            (
                "ftps://media.example.test/video.webm",
                YtDlpInputScheme::Ftps,
            ),
            ("rtmp://media.example.test/live", YtDlpInputScheme::Rtmp),
            ("rtmpe://media.example.test/live", YtDlpInputScheme::Rtmpe),
        ] {
            let locator = parse_yt_dlp_media_locator(raw_locator)
                .expect("exact S00-approved scheme должна пройти pure parse");
            assert_eq!(locator.input_scheme(), expected_scheme);
            assert_eq!(locator.expose_secret_for_persistence(), raw_locator);
        }
    }

    #[test]
    fn unapproved_rtmp_aliases_and_excluded_schemes_are_typed_rejected() {
        for (raw_locator, expected_error) in [
            (
                "rtmps://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "rtmpt://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "rtmpte://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "rtmp_ffmpeg://media.example.test/live",
                YtDlpLocatorParseError::InvalidSyntax,
            ),
            (
                "file:///home/user/video.mp4",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "rtsp://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "rtp://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "mms://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
            (
                "unknown://media.example.test/live",
                YtDlpLocatorParseError::UnsupportedScheme,
            ),
        ] {
            assert_eq!(
                parse_yt_dlp_media_locator(raw_locator)
                    .expect_err("scheme вне exact profile должна быть отклонена"),
                expected_error
            );
        }
    }

    #[test]
    fn extended_scheme_formatting_redacts_credentials_path_and_query() {
        let locator = parse_yt_dlp_media_locator(
            "ftp://user:password@media.example.test/private/video.webm?token=secret",
        )
        .expect("FTP locator из approved parser vocabulary");

        assert_eq!(format!("{locator}"), "yt-dlp media (media.example.test)");
        let formatted = format!("{locator:?}");
        for secret in ["user", "password", "private", "token", "secret"] {
            assert!(!formatted.contains(secret));
        }
        assert!(locator.requires_sensitive_persistence_acknowledgement());
    }
}
