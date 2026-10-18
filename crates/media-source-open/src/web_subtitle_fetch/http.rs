//! Один HTTP-запрос за файлом субтитров с ручным (ограниченным) редиректом.
//!
//! Отличие «провал формата» от «провал всего запроса» принципиально: формат
//! можно заменить запасным (srv3 -> vtt), а 429 или обрыв сети нельзя — тот же
//! лимит YouTube ответит так же и на запасной формат.

use super::types::{WebSubtitleFetchError, WebSubtitleFetchLimits};
use source_core::{
    CancellationToken, HttpBoundedFetchHop, HttpBoundedFetchKind, HttpBoundedFetchRequest,
    HttpRequestTarget, HttpScheme, HttpSourceSession, SourceError,
};

/// Сколько редиректов подряд разрешено (обычно их нет вовсе).
const MAX_REDIRECT_HOPS: usize = 3;

/// Числовые HTTP-статусы, на которые есть отдельная реакция.
const FORBIDDEN: u16 = 403;
const NOT_FOUND: u16 = 404;
const GONE: u16 = 410;
const TOO_MANY_REQUESTS: u16 = 429;

/// Сбой, после которого уместно попробовать запасной формат той же дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormatFailure {
    /// 404: такого формата нет.
    NotFound,
    /// 410: ресурс «ушёл»; если так у всех форматов, вероятнее всего протухла подпись.
    Gone,
    /// Пустое тело.
    EmptyBody,
    /// Тело не похоже на заявленный формат (например, HTML вместо XML).
    WrongBody,
}

/// Исход одного запроса.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RequestFailure {
    /// Можно пробовать запасной формат.
    Format(FormatFailure),
    /// Запасной формат не поможет: вернуть ошибку как есть.
    Fatal(WebSubtitleFetchError),
}

/// Скачивает тело по подписанной цели через уже созданную HTTP-сессию.
pub(super) fn get_body(
    session: &HttpSourceSession,
    target: &HttpRequestTarget,
    limits: WebSubtitleFetchLimits,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, RequestFailure> {
    let mut current_target = target.clone();
    for _ in 0..=MAX_REDIRECT_HOPS {
        let request = HttpBoundedFetchRequest::full(
            current_target.clone(),
            Vec::new(), // заголовки не нужны: подпись вшита в query
            limits.max_bytes(),
            HttpBoundedFetchKind::Metadata,
        );
        match session.fetch_bounded_single_hop(request, cancellation) {
            Ok(HttpBoundedFetchHop::Complete(response)) => return Ok(response.into_bytes()),
            Ok(HttpBoundedFetchHop::Redirect(hop)) => {
                // Понижение https -> http раскрыло бы подписанную ссылку в открытом виде.
                if current_target.scheme() == HttpScheme::Https
                    && hop.target().scheme() == HttpScheme::Http
                {
                    return Err(RequestFailure::Fatal(
                        WebSubtitleFetchError::InvalidResponse,
                    ));
                }
                current_target = hop.target().clone();
            }
            Err(error) => return Err(classify_source_error(&error, limits)),
        }
    }
    // Слишком длинная цепочка редиректов.
    Err(RequestFailure::Fatal(
        WebSubtitleFetchError::InvalidResponse,
    ))
}

/// Переводит ошибку транспорта в типизированный исход без сырого текста ошибки.
fn classify_source_error(error: &SourceError, limits: WebSubtitleFetchLimits) -> RequestFailure {
    match error {
        SourceError::Cancelled | SourceError::SourceClosed => {
            RequestFailure::Fatal(WebSubtitleFetchError::Cancelled)
        }
        SourceError::HttpTimeout { .. } => RequestFailure::Fatal(WebSubtitleFetchError::Timeout),
        SourceError::HttpRequest { .. }
        | SourceError::HttpBodyRead { .. }
        | SourceError::UnexpectedEof { .. }
        | SourceError::HttpClientBuild { .. } => {
            RequestFailure::Fatal(WebSubtitleFetchError::NetworkUnavailable)
        }
        SourceError::HttpBodyTooLarge { .. } => {
            RequestFailure::Fatal(WebSubtitleFetchError::TooLarge {
                limit_bytes: limits.max_bytes().get(),
            })
        }
        SourceError::HttpStatus {
            status,
            retry_after,
            ..
        } => match status.as_u16() {
            NOT_FOUND => RequestFailure::Format(FormatFailure::NotFound),
            GONE => RequestFailure::Format(FormatFailure::Gone),
            FORBIDDEN => RequestFailure::Fatal(WebSubtitleFetchError::LinkExpired),
            TOO_MANY_REQUESTS => RequestFailure::Fatal(WebSubtitleFetchError::RateLimited {
                retry_after: retry_after.delay(),
            }),
            other => {
                RequestFailure::Fatal(WebSubtitleFetchError::UnexpectedStatus { status: other })
            }
        },
        _ => RequestFailure::Fatal(WebSubtitleFetchError::InvalidResponse),
    }
}
