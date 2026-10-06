//! Пользовательская причина неудачного открытия web-media (UX сессия 08).
//!
//! Все web-пути (прямой HTTP, yt-dlp, native HLS/DASH/HDS/Smooth) возвращают
//! `anyhow::Error`, внутри цепочки которого лежат типизированные ошибки владельцев:
//! `YtDlpServiceError`, `TransportOpenError`/`ProviderOpenError`, `SourceError`,
//! `DemuxOpenError`, причины content probe и т.д. Здесь — единственное место, где эта
//! цепочка переводится в одну `Copy`-причину без строк: классификация идёт только
//! через `downcast_ref` звеньев, тексты ошибок не разбираются.
//!
//! Модуль ничего не форматирует для экрана: тексты живут в приложении
//! (`app-egui::web_open_message`). Причина не содержит URL, хоста и текста ошибки.

use dash_mpd_core::DashDynamicMpdError;
use demux_api::{DemuxFactoryOpenError, DemuxOpenError};
use service_ytdlp::{YtDlpRejectionReason, YtDlpServiceError};
use source_core::SourceError;
use web_media_transport_api::{
    AuthenticationFailure, ProviderOpenError, TransportFailure, TransportOpenError,
};

use crate::web_media_open::{ComponentVariantFinalizationError, ContentProbeRejection};

/// Почему web-ссылка не открылась — в терминах пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOpenFailureReason {
    /// Программа `yt-dlp` не установлена или не найдена в `PATH`.
    ExtractorNotInstalled,
    /// Загрузка через `yt-dlp` выключена в настройках.
    ExtractorDisabled,
    /// `yt-dlp` не уложился в таймаут.
    ExtractorTimedOut,
    /// `yt-dlp` упал или вернул ответ, который нельзя разобрать.
    ExtractorFailed,
    /// Видео приватное.
    SitePrivateMedia,
    /// Сайт требует входа в аккаунт (возраст, проверка «не бот», подписка).
    SiteLoginRequired,
    /// Видео недоступно в регионе пользователя.
    SiteGeoRestricted,
    /// Видео удалено или не существует.
    SiteMediaUnavailable,
    /// Сайт не поддерживается `yt-dlp`.
    SiteUnsupported,
    /// Сайт отказал, но причина не распознана.
    SiteRejected,
    /// Ссылка ведёт на подборку, а не на одно видео.
    CollectionLink,
    /// Ни один вариант видео/звука плеер воспроизвести не может.
    NoPlayableFormat,
    /// HTTP 404: страницы/файла нет.
    NotFound,
    /// HTTP 410 или истёкшая подписанная ссылка: ссылка устарела.
    Gone,
    /// Сервер требует логин и пароль (HTTP 401/407).
    AuthenticationRequired,
    /// Сервер запретил доступ (HTTP 403).
    AccessDenied,
    /// Сервер ограничил частоту запросов (HTTP 429).
    RateLimited,
    /// Ошибка на стороне сервера (HTTP 5xx).
    ServerError,
    /// Нет соединения с сервером (DNS, отказ соединения, TLS).
    NoConnection,
    /// Сервер слишком долго не отвечает.
    TimedOut,
    /// Соединение оборвалось посреди передачи.
    ConnectionInterrupted,
    /// Сервер ответил некорректно (битый ответ, запрещённый redirect).
    InvalidServerResponse,
    /// По ссылке нет распознаваемого видео или звука.
    UnrecognizedFormat,
    /// Тип ссылки не поддерживается.
    UnsupportedLink,
    /// Профиль прямой трансляции намеренно не поддерживается.
    UnsupportedLiveProfile,
    /// Причина не распознана ни одним владельцем.
    Unclassified,
}

/// Классифицирует ошибку web-open по типизированным звеньям её цепочки.
///
/// Звенья обходятся от внешнего к корню (`anyhow::Error::chain`); побеждает первое
/// звено со смыслом для пользователя. Звенья без смысла (обёртки, отмена, контекст)
/// пропускаются, чтобы причина нашлась глубже. Если глубже ничего нет, но backend
/// demux-а отказал открыть полученные байты — `UnrecognizedFormat`; иначе
/// `Unclassified`. Отмену вызывающий код обрабатывает сам до классификации.
#[must_use]
pub fn classify_web_open_failure(error: &anyhow::Error) -> WebOpenFailureReason {
    error
        .chain()
        .find_map(classify_error_link)
        .unwrap_or_else(|| fallback_reason_without_typed_cause(error))
}

/// Запасная причина, когда ни одно звено не дало точного смысла.
///
/// Backend demux-а пропускается в основном проходе, чтобы под ним нашлась сетевая
/// причина. Если её нет, значит байты пришли, но плеер их не разобрал (найдено ручной
/// приёмкой UX08: mp4 без faststart с сервера без Range).
fn fallback_reason_without_typed_cause(error: &anyhow::Error) -> WebOpenFailureReason {
    let demux_backend_rejected = error.chain().any(|link| {
        matches!(
            link.downcast_ref::<DemuxOpenError>(),
            Some(DemuxOpenError::FactoryRejected {
                source: DemuxFactoryOpenError::Backend(_),
                ..
            })
        )
    });
    if demux_backend_rejected {
        WebOpenFailureReason::UnrecognizedFormat
    } else {
        WebOpenFailureReason::Unclassified
    }
}

/// Пробует все известные типы владельцев для одного звена цепочки.
fn classify_error_link(link: &(dyn std::error::Error + 'static)) -> Option<WebOpenFailureReason> {
    if let Some(service_error) = link.downcast_ref::<YtDlpServiceError>() {
        return classify_yt_dlp_service_error(service_error);
    }
    if let Some(open_error) = link.downcast_ref::<TransportOpenError>() {
        return classify_transport_open_error(open_error);
    }
    if let Some(provider_error) = link.downcast_ref::<ProviderOpenError>() {
        return classify_provider_open_error(*provider_error);
    }
    if let Some(source_error) = link.downcast_ref::<SourceError>() {
        return classify_source_error(source_error);
    }
    if let Some(demux_error) = link.downcast_ref::<DemuxOpenError>() {
        return classify_demux_open_error(demux_error);
    }
    if let Some(probe_rejection) = link.downcast_ref::<ContentProbeRejection>() {
        return Some(classify_content_probe_rejection(*probe_rejection));
    }
    if link
        .downcast_ref::<ComponentVariantFinalizationError>()
        .is_some()
        || link
            .downcast_ref::<web_media_hds::HdsNoPlayableRendition>()
            .is_some()
    {
        return Some(WebOpenFailureReason::NoPlayableFormat);
    }
    link.downcast_ref::<DashDynamicMpdError>()
        .map(|dash_error| match dash_error {
            DashDynamicMpdError::ProfileExcluded(_) => WebOpenFailureReason::UnsupportedLiveProfile,
            DashDynamicMpdError::Schema(_) => WebOpenFailureReason::InvalidServerResponse,
        })
}

/// Ошибки процесса `yt-dlp` и распознанные причины отказа сайта.
fn classify_yt_dlp_service_error(error: &YtDlpServiceError) -> Option<WebOpenFailureReason> {
    match error {
        YtDlpServiceError::ExecutableNotFound => Some(WebOpenFailureReason::ExtractorNotInstalled),
        YtDlpServiceError::AdapterDisabled => Some(WebOpenFailureReason::ExtractorDisabled),
        YtDlpServiceError::Timeout => Some(WebOpenFailureReason::ExtractorTimedOut),
        YtDlpServiceError::ExtractorRejection { reason, .. } => {
            Some(classify_yt_dlp_rejection(*reason))
        }
        YtDlpServiceError::CollectionUrl => Some(WebOpenFailureReason::CollectionLink),
        YtDlpServiceError::InvalidLocator(_) => Some(WebOpenFailureReason::UnsupportedLink),
        YtDlpServiceError::ProcessFailure { .. }
        | YtDlpServiceError::InvalidExtractorResponse { .. }
        | YtDlpServiceError::StdoutLimitExceeded { .. }
        | YtDlpServiceError::StderrLimitExceeded { .. }
        | YtDlpServiceError::JsonNodeLimitExceeded { .. } => {
            Some(WebOpenFailureReason::ExtractorFailed)
        }
        // Отмена — не ошибка для пользователя; решение принимает вызывающий код.
        YtDlpServiceError::Cancellation => None,
    }
}

/// Причина отказа сайта, распознанная `service-ytdlp` по строкам `ERROR:`.
const fn classify_yt_dlp_rejection(reason: YtDlpRejectionReason) -> WebOpenFailureReason {
    match reason {
        YtDlpRejectionReason::PrivateMedia => WebOpenFailureReason::SitePrivateMedia,
        YtDlpRejectionReason::LoginRequired => WebOpenFailureReason::SiteLoginRequired,
        YtDlpRejectionReason::GeoRestricted => WebOpenFailureReason::SiteGeoRestricted,
        YtDlpRejectionReason::MediaUnavailable => WebOpenFailureReason::SiteMediaUnavailable,
        YtDlpRejectionReason::UnsupportedUrl => WebOpenFailureReason::SiteUnsupported,
        YtDlpRejectionReason::AccessDenied => WebOpenFailureReason::AccessDenied,
        YtDlpRejectionReason::RateLimited => WebOpenFailureReason::RateLimited,
        YtDlpRejectionReason::NetworkUnavailable => WebOpenFailureReason::NoConnection,
        YtDlpRejectionReason::Unclassified => WebOpenFailureReason::SiteRejected,
    }
}

/// Ошибка transport registry (обёртка над ответом provider-а).
fn classify_transport_open_error(error: &TransportOpenError) -> Option<WebOpenFailureReason> {
    match error {
        TransportOpenError::ProviderUnavailable { .. } | TransportOpenError::Unsupported(_) => {
            Some(WebOpenFailureReason::UnsupportedLink)
        }
        TransportOpenError::Authentication(failure) => {
            Some(classify_authentication_failure(*failure))
        }
        TransportOpenError::Transport(failure) => Some(classify_transport_failure(*failure)),
        TransportOpenError::Redirect(_) => Some(WebOpenFailureReason::InvalidServerResponse),
        TransportOpenError::ProviderContract(_) => Some(WebOpenFailureReason::Unclassified),
        TransportOpenError::Cancelled => None,
    }
}

/// Ошибка provider-а без registry-обёртки (встречается в refresh/fallback путях).
const fn classify_provider_open_error(error: ProviderOpenError) -> Option<WebOpenFailureReason> {
    match error {
        ProviderOpenError::Unsupported(_) => Some(WebOpenFailureReason::UnsupportedLink),
        ProviderOpenError::Authentication(failure) => {
            Some(classify_authentication_failure(failure))
        }
        ProviderOpenError::Transport(failure) => Some(classify_transport_failure(failure)),
        ProviderOpenError::Cancelled => None,
    }
}

/// Отказ авторизации: истёкшие credentials — это устаревшая ссылка, остальное — вход.
const fn classify_authentication_failure(failure: AuthenticationFailure) -> WebOpenFailureReason {
    match failure {
        AuthenticationFailure::CredentialsMissing
        | AuthenticationFailure::CredentialsRejected
        | AuthenticationFailure::SecretScopeRejected => {
            WebOpenFailureReason::AuthenticationRequired
        }
        AuthenticationFailure::CredentialsExpired => WebOpenFailureReason::Gone,
    }
}

/// Сетевая категория transport-слоя → причина для пользователя.
const fn classify_transport_failure(failure: TransportFailure) -> WebOpenFailureReason {
    match failure {
        TransportFailure::NetworkUnavailable => WebOpenFailureReason::NoConnection,
        TransportFailure::AccessDenied => WebOpenFailureReason::AccessDenied,
        TransportFailure::Timeout => WebOpenFailureReason::TimedOut,
        TransportFailure::RedirectRejected | TransportFailure::InvalidResponse => {
            WebOpenFailureReason::InvalidServerResponse
        }
        TransportFailure::Interrupted => WebOpenFailureReason::ConnectionInterrupted,
        TransportFailure::NotFound => WebOpenFailureReason::NotFound,
        TransportFailure::Gone => WebOpenFailureReason::Gone,
        TransportFailure::RateLimited => WebOpenFailureReason::RateLimited,
        TransportFailure::ServerError => WebOpenFailureReason::ServerError,
    }
}

/// Сырая ошибка источника байтов (adaptive HLS/DASH/Smooth/HDS путь её сохраняет).
fn classify_source_error(error: &SourceError) -> Option<WebOpenFailureReason> {
    let reason = match error {
        SourceError::Cancelled => return None,
        SourceError::HttpStatus { status, .. } => classify_http_status(status.as_u16()),
        SourceError::HttpTimeout { .. } => WebOpenFailureReason::TimedOut,
        SourceError::HttpRequest { .. }
        | SourceError::HttpClientBuild { .. }
        | SourceError::FtpTransport { .. } => WebOpenFailureReason::NoConnection,
        SourceError::HttpBodyRead { .. } | SourceError::UnexpectedEof { .. } => {
            WebOpenFailureReason::ConnectionInterrupted
        }
        SourceError::InvalidHttpHeaderName { .. }
        | SourceError::InvalidHttpHeaderValue { .. }
        | SourceError::HttpBodyTooLarge { .. }
        | SourceError::InvalidHttpRedirect { .. }
        | SourceError::HttpRangeRedirectRejected { .. }
        | SourceError::HttpRangeUnsupported { .. }
        | SourceError::HttpRequestPolicyRejected { .. }
        | SourceError::HttpRepresentationChanged { .. }
        | SourceError::InvalidContentRange { .. }
        | SourceError::NotSeekable { .. } => WebOpenFailureReason::InvalidServerResponse,
        SourceError::InvalidConfig { .. } | SourceError::LocalIo { .. } => {
            WebOpenFailureReason::Unclassified
        }
    };
    Some(reason)
}

/// HTTP-статус ответа сервера (для путей, где статус ещё не схлопнут provider-ом).
const fn classify_http_status(status_code: u16) -> WebOpenFailureReason {
    match status_code {
        401 | 407 => WebOpenFailureReason::AuthenticationRequired,
        403 => WebOpenFailureReason::AccessDenied,
        404 => WebOpenFailureReason::NotFound,
        410 => WebOpenFailureReason::Gone,
        429 => WebOpenFailureReason::RateLimited,
        500..=599 => WebOpenFailureReason::ServerError,
        _ => WebOpenFailureReason::InvalidServerResponse,
    }
}

/// Demux не узнал содержимое либо его backend упал (причина может лежать глубже).
fn classify_demux_open_error(error: &DemuxOpenError) -> Option<WebOpenFailureReason> {
    match error {
        DemuxOpenError::NoMatch
        | DemuxOpenError::ProbeRejected(_)
        | DemuxOpenError::UnexpectedContainer { .. } => {
            Some(WebOpenFailureReason::UnrecognizedFormat)
        }
        DemuxOpenError::AmbiguousMatch { .. } => Some(WebOpenFailureReason::Unclassified),
        DemuxOpenError::FactoryRejected { source, .. } => match source {
            DemuxFactoryOpenError::UnsupportedInput { .. }
            | DemuxFactoryOpenError::Rejected { .. } => {
                Some(WebOpenFailureReason::UnrecognizedFormat)
            }
            // Backend несёт свою цепочку (часто `SourceError`): ищем причину глубже.
            DemuxFactoryOpenError::Backend(_) | DemuxFactoryOpenError::Cancelled => None,
        },
    }
}

/// Content probe: дорожек нет вовсе — формат не распознан; иначе нечем воспроизвести.
const fn classify_content_probe_rejection(
    rejection: ContentProbeRejection,
) -> WebOpenFailureReason {
    match rejection {
        ContentProbeRejection::NoMediaTracks => WebOpenFailureReason::UnrecognizedFormat,
        ContentProbeRejection::MissingDeclaredVideo
        | ContentProbeRejection::UnexpectedVideo
        | ContentProbeRejection::DeclaredVideoCodecMismatch
        | ContentProbeRejection::UnsupportedVideo
        | ContentProbeRejection::MissingDeclaredAudio
        | ContentProbeRejection::UnexpectedAudio
        | ContentProbeRejection::DeclaredAudioCodecMismatch
        | ContentProbeRejection::UnsupportedAudio
        | ContentProbeRejection::VideoPolicy(_)
        | ContentProbeRejection::NoPlayableAdaptiveVariant => {
            WebOpenFailureReason::NoPlayableFormat
        }
    }
}

#[cfg(test)]
#[path = "web_open_failure/tests.rs"]
mod tests;
