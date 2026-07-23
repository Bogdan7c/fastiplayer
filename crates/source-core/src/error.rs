use std::io;

use reqwest::StatusCode;
use thiserror::Error;

use crate::{HttpRangeRedirectRejection, SecretHttpUrl};

use crate::NotSeekableReason;

/// Результат операций source слоя.
pub type SourceResult<T> = Result<T, SourceError>;

/// Типизированная ошибка чтения bytes из local/http/cache source.
#[derive(Debug, Error)]
pub enum SourceError {
    /// Caller отменил операцию через `CancellationToken`.
    #[error("source operation cancelled")]
    Cancelled,

    /// Значение config не может быть использовано source слоем.
    #[error("некорректное значение config-поля `{field}`: {message}")]
    InvalidConfig {
        /// TOML-путь поля.
        field: &'static str,

        /// Человекочитаемое объяснение ограничения.
        message: String,
    },

    /// Ошибка локальной файловой системы.
    #[error("ошибка локального source `{context}`: {source}")]
    LocalIo {
        /// Операция, которая выполнялась.
        context: &'static str,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// HTTP header name из внешнего service descriptor некорректен.
    #[error("некорректное имя HTTP header `{name}`")]
    InvalidHttpHeaderName {
        /// Имя header-а как пришло из caller-а.
        name: String,
    },

    /// HTTP header value из внешнего service descriptor некорректен.
    #[error("некорректное значение HTTP header `{name}`")]
    InvalidHttpHeaderValue {
        /// Имя header-а, значение которого не прошло validation.
        name: String,
    },

    /// Не удалось создать HTTP client.
    #[error("не удалось создать HTTP client: {source}")]
    HttpClientBuild {
        /// Ошибка reqwest builder-а.
        #[source]
        source: reqwest::Error,
    },

    /// HTTP операция превысила configured timeout.
    #[error("HTTP timeout во время `{operation}` для {url}")]
    HttpTimeout {
        /// Человекочитаемая операция: probe/read.
        operation: &'static str,

        /// URL источника.
        url: SecretHttpUrl,
    },

    /// HTTP request не дошёл до успешного response.
    #[error("HTTP request `{operation}` для {url} не удался: {source}")]
    HttpRequest {
        /// Человекочитаемая операция: probe/read.
        operation: &'static str,

        /// URL источника.
        url: SecretHttpUrl,

        /// Исходная ошибка reqwest.
        #[source]
        source: reqwest::Error,
    },

    /// HTTP body оборвался или не читается.
    #[error("HTTP body `{operation}` для {url} не читается: {source}")]
    HttpBodyRead {
        /// Человекочитаемая операция: probe/read.
        operation: &'static str,

        /// URL источника.
        url: SecretHttpUrl,

        /// Исходная ошибка чтения.
        #[source]
        source: io::Error,
    },

    /// Сервер вернул статус, несовместимый с ожидаемой source-операцией.
    #[error("HTTP source {url} вернул статус {status} во время `{operation}`")]
    HttpStatus {
        /// Человекочитаемая операция: probe/read.
        operation: &'static str,

        /// URL источника.
        url: SecretHttpUrl,

        /// Полученный HTTP status.
        status: StatusCode,
    },

    /// HTTP body превысил caller-owned bounded resource limit.
    #[error("HTTP body `{operation}` для {url} превысил лимит {maximum_bytes} bytes")]
    HttpBodyTooLarge {
        /// Стабильная категория операции без внешнего payload-а.
        operation: &'static str,

        /// Redacted URL источника.
        url: SecretHttpUrl,

        /// Exact caller-owned upper bound.
        maximum_bytes: usize,
    },

    /// Redirect response не содержит безопасно разрешимый HTTP(S) target.
    #[error("HTTP redirect для {url} отклонён source parser-ом: {reason}")]
    InvalidHttpRedirect {
        /// Redacted URL текущего hop-а.
        url: SecretHttpUrl,

        /// Фиксированная категория ошибки без server-provided payload.
        reason: &'static str,
    },

    /// Transport policy отклонила redirect, возникший во время Range read-а.
    #[error("HTTP Range redirect для {url} отклонён: {reason}")]
    HttpRangeRedirectRejected {
        /// Redacted URL текущего hop-а.
        url: SecretHttpUrl,

        /// Стабильная typed причина без target/header/cookie payload-а.
        reason: HttpRangeRedirectRejection,
    },

    /// HTTP source не подтвердил Range чтение через `206 Partial Content`.
    #[error("HTTP source не поддерживает обязательный Range seek: {reason}")]
    HttpRangeUnsupported {
        /// Нормализованная причина отсутствия seek.
        reason: NotSeekableReason,
    },

    /// Header `Content-Range` отсутствует или не соответствует формату bytes range.
    #[error("некорректный Content-Range от HTTP source {url}: {header}")]
    InvalidContentRange {
        /// URL источника.
        url: SecretHttpUrl,

        /// Значение header-а или marker отсутствия.
        header: String,
    },

    /// Источник закончился раньше запрошенного byte range.
    #[error("source вернул {actual_bytes} bytes вместо {expected_bytes} с offset {offset}")]
    UnexpectedEof {
        /// Начальный offset запроса.
        offset: u64,

        /// Сколько bytes ожидалось.
        expected_bytes: usize,

        /// Сколько bytes было получено.
        actual_bytes: usize,
    },

    /// Caller пытается читать seek range из non-seekable source.
    #[error("source не поддерживает seek: {reason}")]
    NotSeekable {
        /// Почему source не seekable.
        reason: NotSeekableReason,
    },
}

impl SourceError {
    /// Возвращает `true`, если range read можно безопасно повторить один раз.
    #[must_use]
    pub(crate) fn is_retryable_range_failure(&self) -> bool {
        match self {
            Self::HttpTimeout { .. } | Self::HttpRequest { .. } | Self::HttpBodyRead { .. } => true,
            Self::UnexpectedEof { .. } => true,
            Self::HttpStatus { status, .. } => status.is_server_error(),
            Self::Cancelled
            | Self::InvalidConfig { .. }
            | Self::LocalIo { .. }
            | Self::InvalidHttpHeaderName { .. }
            | Self::InvalidHttpHeaderValue { .. }
            | Self::HttpClientBuild { .. }
            | Self::HttpBodyTooLarge { .. }
            | Self::InvalidHttpRedirect { .. }
            | Self::HttpRangeRedirectRejected { .. }
            | Self::HttpRangeUnsupported { .. }
            | Self::InvalidContentRange { .. }
            | Self::NotSeekable { .. } => false,
        }
    }
}
