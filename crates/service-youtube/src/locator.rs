use std::fmt;

use thiserror::Error;
use url::Url;

/// Стабильный locator страницы media, которым владеет YouTube adapter.
#[derive(Clone, PartialEq, Eq)]
pub struct YoutubeMediaLocator {
    normalized_secret_identity: String,
    safe_label: String,
}

impl YoutubeMediaLocator {
    /// Раскрывает нормализованную identity только для service open/refresh.
    #[must_use]
    pub fn expose_secret_for_open(&self) -> &str {
        &self.normalized_secret_identity
    }

    /// Раскрывает нормализованную identity только для persistence.
    #[must_use]
    pub fn expose_secret_for_persistence(&self) -> &str {
        &self.normalized_secret_identity
    }

    /// Возвращает bounded label без userinfo/path/query/fragment.
    #[must_use]
    pub fn safe_label(&self) -> &str {
        &self.safe_label
    }
}

impl fmt::Debug for YoutubeMediaLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YoutubeMediaLocator")
            .field("safe_label", &self.safe_label)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for YoutubeMediaLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.safe_label)
    }
}

/// Ошибка pure parse/classification без отражения исходного input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum YoutubeLocatorParseError {
    /// Строка не является absolute URL.
    #[error("некорректный URL для media service")]
    InvalidSyntax,

    /// URL синтаксически валиден, но принадлежит другому service adapter-у.
    #[error("URL не поддерживается YouTube adapter")]
    UnsupportedService,
}

/// Классифицирует и нормализует URL без network I/O.
pub fn parse_youtube_media_locator(
    argument: &str,
) -> Result<YoutubeMediaLocator, YoutubeLocatorParseError> {
    let mut parsed_url =
        Url::parse(argument).map_err(|_| YoutubeLocatorParseError::InvalidSyntax)?;
    if !matches!(parsed_url.scheme(), "http" | "https") || !is_youtube_host(&parsed_url) {
        return Err(YoutubeLocatorParseError::UnsupportedService);
    }

    remove_service_owned_tracking_parameters(&mut parsed_url);
    let safe_label = youtube_safe_label(&parsed_url);

    Ok(YoutubeMediaLocator {
        normalized_secret_identity: parsed_url.into(),
        safe_label,
    })
}

/// Exact direct-stream URL, который может содержать подпись и expiry query.
#[derive(Clone, PartialEq, Eq)]
pub struct YoutubeDirectStreamUrl(String);

impl YoutubeDirectStreamUrl {
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

impl fmt::Debug for YoutubeDirectStreamUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("YoutubeDirectStreamUrl(<redacted>)")
    }
}

impl fmt::Display for YoutubeDirectStreamUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("YouTube direct stream <redacted>")
    }
}

fn is_youtube_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();

    matches!(
        normalized_host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    )
}

fn remove_service_owned_tracking_parameters(url: &mut Url) {
    let retained_pairs = url
        .query_pairs()
        .filter(|(name, _)| !is_service_owned_tracking_parameter(name))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    let original_pair_count = url.query_pairs().count();
    if retained_pairs.len() == original_pair_count {
        return;
    }

    let mut query = url.query_pairs_mut();
    query.clear();
    query.extend_pairs(retained_pairs);
}

fn is_service_owned_tracking_parameter(name: &str) -> bool {
    name == "si" || name == "feature" || name.starts_with("utm_")
}

fn youtube_safe_label(url: &Url) -> String {
    let host = url.host_str().unwrap_or("youtube");
    format!("YouTube media ({host})")
}

#[cfg(test)]
mod tests {
    use super::{YoutubeLocatorParseError, parse_youtube_media_locator};

    #[test]
    fn normalization_removes_only_owned_tracking_and_is_idempotent() {
        let locator = parse_youtube_media_locator(
            "https://www.youtube.com/watch?v=id&t=3&start=4&end=5&list=L&index=2&si=secret&feature=share&utm_source=x&future=value",
        )
        .expect("known YouTube URL должен пройти parse");
        let normalized = locator.expose_secret_for_persistence();

        assert_eq!(
            normalized,
            "https://www.youtube.com/watch?v=id&t=3&start=4&end=5&list=L&index=2&future=value"
        );
        let reparsed = parse_youtube_media_locator(normalized)
            .expect("normalized identity должна повторно проходить parse");
        assert_eq!(reparsed.expose_secret_for_persistence(), normalized);
    }

    #[test]
    fn formatting_never_exposes_userinfo_query_or_path() {
        let locator = parse_youtube_media_locator(
            "https://user:password@youtu.be/private-id?v=secret&unknown=keep#fragment",
        )
        .expect("allowlisted URL должен пройти parse");

        assert_eq!(format!("{locator}"), "YouTube media (youtu.be)");
        assert!(!format!("{locator:?}").contains("secret"));
        assert!(!format!("{locator:?}").contains("password"));
        assert!(!format!("{locator:?}").contains("private-id"));
    }

    #[test]
    fn invalid_syntax_error_does_not_reflect_input() {
        let secret_input = "https://user:password@[invalid]?token=secret";
        let error = parse_youtube_media_locator(secret_input)
            .expect_err("invalid syntax должна возвращать typed error");

        assert_eq!(error, YoutubeLocatorParseError::InvalidSyntax);
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(!format!("{error:?} {error}").contains("password"));
    }
}
