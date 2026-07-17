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
}

impl YtDlpMediaLocator {
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

/// Ошибка pure parse/classification без отражения исходного input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum YtDlpLocatorParseError {
    /// Строка не является absolute URL.
    #[error("некорректный URL для media service")]
    InvalidSyntax,

    /// Схема URL не является HTTP(S), поэтому adapter не имеет права её открывать.
    #[error("media service поддерживает только HTTP(S) URL")]
    UnsupportedScheme,
}

/// Классифицирует generic absolute HTTP(S) URL без network I/O.
pub fn parse_yt_dlp_media_locator(
    argument: &str,
) -> Result<YtDlpMediaLocator, YtDlpLocatorParseError> {
    let parsed_url = Url::parse(argument).map_err(|_| YtDlpLocatorParseError::InvalidSyntax)?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(YtDlpLocatorParseError::UnsupportedScheme);
    }
    if parsed_url.host().is_none() {
        return Err(YtDlpLocatorParseError::InvalidSyntax);
    }

    let safe_label = yt_dlp_safe_label(&parsed_url);

    Ok(YtDlpMediaLocator {
        exact_secret_identity: argument.to_owned(),
        safe_label,
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
    use super::{YtDlpLocatorParseError, parse_yt_dlp_media_locator};

    #[test]
    fn generic_url_preserves_exact_identity_without_query_normalization() {
        let exact_url =
            "https://media.example.test/watch/%2Fopaque?token=a%2Bb&utm_source=keep#chapter";
        let locator =
            parse_yt_dlp_media_locator(exact_url).expect("generic HTTP(S) URL должен пройти parse");

        assert_eq!(locator.expose_secret_for_persistence(), exact_url);
        assert_eq!(locator.expose_secret_for_open(), exact_url);
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
    fn generic_and_youtube_http_urls_are_accepted_but_other_schemes_are_rejected() {
        assert!(parse_yt_dlp_media_locator("https://video.example.test/watch/42").is_ok());
        assert!(parse_yt_dlp_media_locator("https://www.youtube.com/watch?v=42").is_ok());
        assert_eq!(
            parse_yt_dlp_media_locator("rtsp://video.example.test/live")
                .expect_err("не-HTTP схема должна быть отклонена"),
            YtDlpLocatorParseError::UnsupportedScheme
        );
    }
}
