//! Service-owned classification для stable direct HTTP(S)/FTP(S) media locator-ов.
//!
//! Crate владеет только URL policy, checked physical target-ом и secret-safe
//! stable locator-ом. Transport, demux, player и UI runtime остаются у app
//! composition root-а и не проникают в этот service boundary.

use demux_api::{DemuxInputCapability, DemuxRegistry, DemuxSourceExtension};
use thiserror::Error;
use url::Url;
use web_media_transport_api::TransportRequestTarget;

mod locator;

pub use locator::DirectMediaUrl;

/// Extension direct resource-а, подтверждённый фактическим demux registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMediaExtension(DemuxSourceExtension);

impl DirectMediaExtension {
    /// Возвращает exact normalized extension hint для neutral demux probe.
    #[must_use]
    pub fn as_extension_hint(&self) -> &str {
        self.0.as_str()
    }

    /// Принимает extension только если registry умеет обе progressive формы.
    fn from_path_extension(
        extension: &str,
        demux_registry: &DemuxRegistry,
    ) -> Result<Option<Self>, demux_api::DemuxIdentityError> {
        let normalized = DemuxSourceExtension::new(extension.to_ascii_lowercase())?;
        let supports_seekable =
            demux_registry.supports_extension(&normalized, DemuxInputCapability::SeekableBytes);
        let supports_streaming =
            demux_registry.supports_extension(&normalized, DemuxInputCapability::StreamingBytes);

        Ok((supports_seekable && supports_streaming).then_some(Self(normalized)))
    }
}

/// Причина, почему URL не входит в direct progressive policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DirectMediaUrlUnsupportedReason {
    /// Direct progressive ingress принимает только HTTP(S) и FTP(S).
    #[error("protocol не поддерживается; direct media принимает http(s) и ftp(s)")]
    UnsupportedProtocol,

    /// Authority-style URL обязан содержать host.
    #[error("direct media URL не содержит host")]
    MissingHost,

    /// URL path обязан содержать явный extension.
    #[error("URL path не содержит media extension")]
    MissingExtension,

    /// Manifest extensions принадлежат отдельному native manifest ingress.
    #[error("manifest URL не открывается как progressive resource")]
    ManifestUnsupported,

    /// Extension не зарегистрирован для seekable и streaming byte input.
    #[error("URL extension не поддерживается production demux registry")]
    UnsupportedExtension,
}

/// Типизированные secret-safe ошибки direct locator classification.
#[derive(Debug, Error)]
pub enum DirectMediaOpenError {
    /// URL parser не смог разобрать аргумент как absolute URL.
    #[error("некорректный direct media URL")]
    InvalidUrl {
        /// Ошибка `url` crate не публикует исходную secret строку.
        #[source]
        source: url::ParseError,
    },

    /// URL синтаксически валиден, но не входит в direct policy.
    #[error("unsupported direct media URL: {reason}")]
    UnsupportedUrl {
        /// Стабильная причина rejection-а.
        reason: DirectMediaUrlUnsupportedReason,
    },

    /// Registry extension identity нарушила canonical demux grammar.
    #[error("direct media extension capability имеет некорректную identity")]
    DemuxCapabilityIdentity {
        /// Neutral identity error не содержит locator payload.
        #[source]
        source: demux_api::DemuxIdentityError,
    },

    /// HTTP(S) locator нарушил checked request-target policy.
    #[error("direct HTTP media target не прошёл request policy")]
    HttpTarget {
        /// Secret-safe typed target error.
        #[source]
        source: source_core::HttpRequestTargetError,
    },

    /// FTP(S) locator нарушил checked request-target policy.
    #[error("direct FTP media target не прошёл request policy")]
    FtpTarget {
        /// Secret-safe typed target error.
        #[source]
        source: source_core::FtpRequestTargetError,
    },
}

/// Проверяет, выглядит ли строка как URL с authority-style scheme.
#[must_use]
pub fn looks_like_url(argument: &str) -> bool {
    argument.contains("://")
}

/// Классифицирует stable resource без network I/O и без отдельного allowlist-а.
pub fn parse_direct_media_url(
    argument: &str,
    demux_registry: &DemuxRegistry,
) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    let parsed_url =
        Url::parse(argument).map_err(|source| DirectMediaOpenError::InvalidUrl { source })?;
    direct_media_url_from_parsed(argument, parsed_url, demux_registry)
}

/// Проверяет scheme/host/extension и строит checked physical request target.
fn direct_media_url_from_parsed(
    original_argument: &str,
    parsed_url: Url,
    demux_registry: &DemuxRegistry,
) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    if !matches!(parsed_url.scheme(), "http" | "https" | "ftp" | "ftps") {
        return Err(unsupported_url(
            DirectMediaUrlUnsupportedReason::UnsupportedProtocol,
        ));
    }
    if parsed_url.host_str().is_none() {
        return Err(unsupported_url(
            DirectMediaUrlUnsupportedReason::MissingHost,
        ));
    }

    let extension = path_extension(&parsed_url)
        .ok_or_else(|| unsupported_url(DirectMediaUrlUnsupportedReason::MissingExtension))?;
    let media_extension = DirectMediaExtension::from_path_extension(extension, demux_registry)
        .map_err(|source| DirectMediaOpenError::DemuxCapabilityIdentity { source })?
        .ok_or_else(|| unsupported_url(unsupported_extension_reason(extension)))?;
    let safe_label = direct_media_safe_label(&parsed_url, &media_extension);
    let requires_sensitive_persistence_acknowledgement = !parsed_url.username().is_empty()
        || parsed_url.password().is_some()
        || parsed_url.query().is_some_and(|query| !query.is_empty());

    let request_target = match parsed_url.scheme() {
        "http" | "https" => source_core::HttpRequestTarget::parse_exact(original_argument)
            .map(TransportRequestTarget::from_http)
            .map_err(|source| DirectMediaOpenError::HttpTarget { source })?,
        "ftp" | "ftps" => source_core::FtpRequestTarget::parse_exact(original_argument)
            .map(TransportRequestTarget::from_ftp)
            .map_err(|source| DirectMediaOpenError::FtpTarget { source })?,
        _ => unreachable!("scheme был проверен до target parsing"),
    };

    Ok(DirectMediaUrl::new(
        request_target,
        media_extension,
        safe_label,
        requires_sensitive_persistence_acknowledgement,
    ))
}

/// Извлекает extension только из последнего path segment-а.
fn path_extension(parsed_url: &Url) -> Option<&str> {
    let last_segment = parsed_url
        .path_segments()?
        .rfind(|segment| !segment.is_empty())?;
    let (_, extension) = last_segment.rsplit_once('.')?;
    (!extension.is_empty()).then_some(extension)
}

/// Создаёт typed unsupported URL error без locator payload.
fn unsupported_url(reason: DirectMediaUrlUnsupportedReason) -> DirectMediaOpenError {
    DirectMediaOpenError::UnsupportedUrl { reason }
}

/// Строит bounded label только из scheme, host, optional port и extension.
fn direct_media_safe_label(url: &Url, extension: &DirectMediaExtension) -> String {
    const MAX_HOST_LABEL_BYTES: usize = 253;
    let host = url
        .host_str()
        .filter(|host| host.len() <= MAX_HOST_LABEL_BYTES)
        .unwrap_or("<redacted-host>");
    let port = url
        .port()
        .map_or_else(String::new, |value| format!(":{value}"));
    format!(
        "direct media {}://{host}{port}/<redacted>.{}",
        url.scheme(),
        extension.as_extension_hint()
    )
}

/// Отделяет manifest extensions от обычного unsupported extension.
fn unsupported_extension_reason(extension: &str) -> DirectMediaUrlUnsupportedReason {
    if extension.eq_ignore_ascii_case("m3u8") || extension.eq_ignore_ascii_case("mpd") {
        DirectMediaUrlUnsupportedReason::ManifestUnsupported
    } else {
        DirectMediaUrlUnsupportedReason::UnsupportedExtension
    }
}

#[cfg(test)]
mod tests;
