//! Минимальная поверхность bounded hop-а для внешнего (не reqwest) транспорта.
//!
//! Обычный HTTP выполняет запрос сам внутри `HttpSourceSession`. Внешний транспорт
//! (например, мост в браузер, который качает ресурс по номерку) получает тот же
//! `HttpBoundedFetchRequest`, но исполняет его своими средствами. Ему нужно три вещи:
//! - прочитать, *что* просят (цель, заголовки, диапазон, лимит, категория);
//! - собрать успешный ответ с теми же проверками длины, что у обычного HTTP;
//! - превратить свой сбой в честный `SourceError` без reqwest, сохранив
//!   классификацию «повторяемо / не повторяемо», от которой зависят слои выше.
//!
//! Модуль только добавляет API: поведение обычного HTTP-пути не меняется.

use std::fmt;
use std::io;
use std::num::NonZeroUsize;

use super::{
    HttpBoundedByteRange, HttpBoundedFetchKind, HttpBoundedFetchRequest, HttpBoundedResponse,
};
use crate::{
    HttpHeader, HttpRequestTarget, HttpRetryAfter, SecretHttpUrl, SourceError, SourceResult,
};

/// HTTP-статус для ответов внешнего транспорта.
///
/// Это тот же тип, что уже лежит в публичном поле `SourceError::HttpStatus::status`;
/// реэкспорт избавляет внешний транспорт от прямой зависимости на reqwest/http.
pub use reqwest::StatusCode as HttpStatusCode;

impl HttpBoundedFetchRequest {
    /// Точная цель запроса; раскрывать её можно только в точке реального запроса.
    #[must_use]
    pub const fn target(&self) -> &HttpRequestTarget {
        &self.target
    }

    /// Уже проверенные заголовки запроса (scoped-секреты, без Cookie).
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// Точный диапазон байтов; `None` означает «весь ресурс».
    #[must_use]
    pub const fn byte_range(&self) -> Option<HttpBoundedByteRange> {
        self.byte_range
    }

    /// Верхняя граница тела ответа (для диапазона совпадает с его длиной).
    #[must_use]
    pub const fn maximum_body_bytes(&self) -> NonZeroUsize {
        self.maximum_body_bytes
    }

    /// Категория запроса (метаданные или медиа-байты).
    #[must_use]
    pub const fn kind(&self) -> HttpBoundedFetchKind {
        self.kind
    }

    /// Контекст для сборки secret-safe ошибок внешнего транспорта.
    ///
    /// Контекст переживает сам запрос: потоковое тело внешнего транспорта
    /// может хранить его и строить ошибки на поздних кусках.
    #[must_use]
    pub fn external_failure_context(&self) -> HttpExternalFailureContext {
        HttpExternalFailureContext {
            operation: self.kind.operation(),
            url: SecretHttpUrl::from_secret_for_open(
                self.target.expose_secret_for_request().to_owned(),
            ),
            maximum_body_bytes: self.maximum_body_bytes,
        }
    }
}

impl HttpBoundedResponse {
    /// Собирает успешный ответ из тела, полученного внешним транспортом.
    ///
    /// Проверки те же, что у обычного HTTP: тело не длиннее лимита запроса
    /// (`HttpBodyTooLarge`), а у запроса с диапазоном длина тела ровно равна
    /// длине диапазона (`UnexpectedEof`, повторяемо). Range-метаданных у внешнего
    /// транспорта нет: он не видит `Content-Range`, поэтому они всегда `None`.
    pub fn from_external_body(
        request: &HttpBoundedFetchRequest,
        bytes: Vec<u8>,
    ) -> SourceResult<Self> {
        if bytes.len() > request.maximum_body_bytes.get() {
            return Err(request
                .external_failure_context()
                .failure(HttpExternalTransportFailure::BodyTooLarge));
        }
        if let Some(byte_range) = request.byte_range
            && bytes.len() != byte_range.length().get()
        {
            return Err(SourceError::UnexpectedEof {
                offset: byte_range.start(),
                expected_bytes: byte_range.length().get(),
                actual_bytes: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            range_metadata: None,
        })
    }
}

/// Сбой внешнего транспорта в словаре, который понимают слои выше.
///
/// Каждый вариант отображается ровно в один существующий `SourceError`, поэтому
/// повторы adaptive-слоя и `SourceError::is_transient_network_failure` работают
/// для внешнего транспорта так же, как для обычного HTTP.
#[derive(Debug)]
pub enum HttpExternalTransportFailure {
    /// Связь оборвалась до ответа или посреди тела. Повторяемо (`HttpBodyRead`).
    Interrupted(io::Error),
    /// Транспорт не уложился в своё время ожидания. Повторяемо (`HttpTimeout`).
    TimedOut,
    /// Сервер ответил статусом, который запрос не принимает (`HttpStatus`).
    ///
    /// Повторяемость решает сам статус: 5xx/408/429 — да, 401/403/404/410 — нет.
    Status {
        /// Полученный HTTP-статус.
        status: HttpStatusCode,
        /// Подсказка сервера, когда повторить, если она была.
        retry_after: HttpRetryAfter,
    },
    /// Тело превысило лимит запроса. Не повторяемо (`HttpBodyTooLarge`).
    BodyTooLarge,
}

/// Secret-safe данные запроса, нужные для типизированной ошибки.
#[derive(Clone)]
pub struct HttpExternalFailureContext {
    /// Стабильная категория операции из `HttpBoundedFetchKind`.
    operation: &'static str,
    /// Redacted-адрес: Display/Debug никогда не печатают его содержимое.
    url: SecretHttpUrl,
    /// Лимит тела исходного запроса для `HttpBodyTooLarge`.
    maximum_body_bytes: NonZeroUsize,
}

impl HttpExternalFailureContext {
    /// Превращает сбой внешнего транспорта в существующий вариант `SourceError`.
    #[must_use]
    pub fn failure(&self, failure: HttpExternalTransportFailure) -> SourceError {
        match failure {
            HttpExternalTransportFailure::Interrupted(source) => SourceError::HttpBodyRead {
                operation: self.operation,
                url: self.url.clone(),
                source,
            },
            HttpExternalTransportFailure::TimedOut => SourceError::HttpTimeout {
                operation: self.operation,
                url: self.url.clone(),
            },
            HttpExternalTransportFailure::Status {
                status,
                retry_after,
            } => SourceError::HttpStatus {
                operation: self.operation,
                url: self.url.clone(),
                status,
                retry_after,
            },
            HttpExternalTransportFailure::BodyTooLarge => SourceError::HttpBodyTooLarge {
                operation: self.operation,
                url: self.url.clone(),
                maximum_bytes: self.maximum_body_bytes.get(),
            },
        }
    }
}

impl fmt::Debug for HttpExternalFailureContext {
    /// Адрес не печатается даже в redacted-форме: хватает категории и лимита.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpExternalFailureContext")
            .field("operation", &self.operation)
            .field("maximum_body_bytes", &self.maximum_body_bytes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
