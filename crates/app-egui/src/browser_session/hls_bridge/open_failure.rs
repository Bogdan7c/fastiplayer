//! Почему HLS-поколение не открылось — в классах отказа протокола (`stream_rejected`).
//!
//! Модель отказов та же, что у файла `<video src>` (`super::super::rejection`): отказ —
//! свойство поколения, странице уходит `stream_rejected{reason}`, зритель окна видит
//! короткий текст (`rejection_notice`), а сеанс живёт. Классы:
//!
//! | что случилось | класс |
//! |---|---|
//! | корень/ресурс не получен: отказ фона, 401/403/404, сеть после повторов, молчание, слишком большой, адрес не номерок | `file_unavailable` |
//! | по номерку пришёл не HLS (HTML-страница), битый плейлист | `unparsable_media` |
//! | HLS-профиль, который плеер не берёт (в том числе SAMPLE-AES и прочий DRM — движок не отличает его от остальных профилей), ни одного играбельного варианта, кодек | `unsupported_codec` |
//! | остальное (дефект сборки, нарушенный инвариант) | `playback_failed` |
//!
//! Классифицируются только ТИПЫ звеньев цепочки ошибки (`downcast_ref`), тексты не
//! разбираются и в журнал не идут (в них могут быть данные страницы).

use anyhow::Error;
use browser_bridge_protocol::session::player::StreamRejection;
use media_source_open::web_open_failure::{WebOpenFailureReason, classify_web_open_failure};
use source_core::SourceError;
use web_media_core::WebMediaFallbackTrigger;

/// Отказ открытия: что ответить странице и что записать в журнал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsOpenRejection {
    /// Причина для `stream_rejected`.
    pub(crate) reason: StreamRejection,
    /// Короткая стабильная метка для журнала (без данных страницы).
    pub(crate) class: &'static str,
}

impl HlsOpenRejection {
    /// Отказ этого класса с этой меткой.
    const fn new(reason: StreamRejection, class: &'static str) -> Self {
        Self { reason, class }
    }
}

/// Native HLS вернул «нужен извлекатель»: у моста адреса для yt-dlp нет, поэтому это
/// честный отказ по смыслу триггера.
pub(crate) const fn rejection_for_fallback_trigger(
    trigger: WebMediaFallbackTrigger,
) -> HlsOpenRejection {
    match trigger {
        // Корень оказался документом (HTML вместо плейлиста) или битым манифестом.
        WebMediaFallbackTrigger::ProviderDocument | WebMediaFallbackTrigger::MalformedManifest => {
            HlsOpenRejection::new(StreamRejection::UnparsableMedia, "not_hls")
        }
        // Профиль HLS вне поддержанного (LL-HLS, SAMPLE-AES, I-frame-only и т. п.).
        WebMediaFallbackTrigger::UnsupportedNativeProfile => {
            HlsOpenRejection::new(StreamRejection::UnsupportedCodec, "unsupported_profile")
        }
        // 401/403 на корне, истёкший адрес, сеть, обратное давление: ресурс не получить.
        WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial
        | WebMediaFallbackTrigger::ExpiredEndpoint
        | WebMediaFallbackTrigger::NetworkFailure
        | WebMediaFallbackTrigger::Backpressure
        // Отмена до открытия: поколение кончилось (печать) — отвечать почти некому, но
        // честнее «не загрузилось», чем «сломалось».
        | WebMediaFallbackTrigger::Cancellation => {
            HlsOpenRejection::new(StreamRejection::FileUnavailable, "resource_unavailable")
        }
        WebMediaFallbackTrigger::DecoderFailure => {
            HlsOpenRejection::new(StreamRejection::UnsupportedCodec, "decoder_rejected")
        }
        WebMediaFallbackTrigger::RendererFailure | WebMediaFallbackTrigger::InvariantViolation => {
            HlsOpenRejection::new(StreamRejection::PlaybackFailed, "internal")
        }
    }
}

/// Ошибка подготовки native HLS → отказ поколения.
///
/// Сначала — звено `SourceError` (ресурс не пришёл: оно точнее общей классификации,
/// которая отказ фона назвала бы «некорректным ответом сервера»), затем общая
/// классификация web-открытия `media-source-open`.
pub(crate) fn rejection_for_preparation_error(error: &Error) -> HlsOpenRejection {
    if let Some(rejection) = error
        .chain()
        .find_map(|link| link.downcast_ref::<SourceError>())
        .and_then(rejection_for_source_error)
    {
        return rejection;
    }
    rejection_for_web_open_reason(classify_web_open_failure(error))
}

/// Ресурс не получен — по виду ошибки транспорта моста.
const fn rejection_for_source_error(error: &SourceError) -> Option<HlsOpenRejection> {
    match error {
        SourceError::HttpStatus { .. }
        | SourceError::HttpTimeout { .. }
        | SourceError::HttpBodyRead { .. }
        | SourceError::HttpRequest { .. }
        | SourceError::UnexpectedEof { .. }
        | SourceError::HttpBodyTooLarge { .. }
        | SourceError::HttpRequestPolicyRejected { .. }
        | SourceError::Cancelled
        | SourceError::SourceClosed => Some(HlsOpenRejection::new(
            StreamRejection::FileUnavailable,
            "resource_unavailable",
        )),
        // Остальные варианты транспорту моста не свойственны: решает общая классификация.
        _ => None,
    }
}

/// Общая причина web-открытия → класс отказа поколения.
const fn rejection_for_web_open_reason(reason: WebOpenFailureReason) -> HlsOpenRejection {
    match reason {
        WebOpenFailureReason::NoPlayableFormat | WebOpenFailureReason::UnsupportedLiveProfile => {
            HlsOpenRejection::new(StreamRejection::UnsupportedCodec, "no_playable_variant")
        }
        WebOpenFailureReason::UnrecognizedFormat | WebOpenFailureReason::InvalidServerResponse => {
            HlsOpenRejection::new(StreamRejection::UnparsableMedia, "unparsable")
        }
        WebOpenFailureReason::NotFound
        | WebOpenFailureReason::Gone
        | WebOpenFailureReason::AuthenticationRequired
        | WebOpenFailureReason::AccessDenied
        | WebOpenFailureReason::RateLimited
        | WebOpenFailureReason::ServerError
        | WebOpenFailureReason::NoConnection
        | WebOpenFailureReason::TimedOut
        | WebOpenFailureReason::ConnectionInterrupted => {
            HlsOpenRejection::new(StreamRejection::FileUnavailable, "resource_unavailable")
        }
        // Причины yt-dlp и сайта мосту не свойственны, остальное — не класс страницы.
        _ => HlsOpenRejection::new(StreamRejection::PlaybackFailed, "unclassified"),
    }
}

#[cfg(test)]
#[path = "open_failure_tests.rs"]
mod tests;
