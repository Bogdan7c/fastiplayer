//! Incremental bounded response body и его one-shot HTTP lifecycle.

use std::fmt;
use std::io;
use std::time::Duration;

use bytes::Bytes;

use super::{
    CancellationToken, HttpBoundedByteRange, HttpBoundedFetchKind, HttpBoundedFetchRequest,
    HttpRangeResponseMetadata, HttpRedirectHop, HttpRequestAttemptDiagnostics,
    HttpRequestDiagnosticError, HttpSourceSession, SecretHttpUrl, SourceError, SourceResult,
    ValidatedBoundedResponse, map_reqwest_error, prepare_bounded_request,
    validate_bounded_response,
};

/// Результат открытия одного incremental bounded HTTP hop-а.
pub enum HttpBoundedStreamingFetchHop {
    /// Redirect body не читается до решения общей adaptive policy.
    Redirect(HttpRedirectHop),
    /// Validated response body доступен pull-based chunks без полной буферизации.
    Body(HttpBoundedStreamingBody),
}

impl fmt::Debug for HttpBoundedStreamingFetchHop {
    /// Не раскрывает request target, response URL или header values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect(redirect) => formatter.debug_tuple("Redirect").field(redirect).finish(),
            Self::Body(body) => formatter.debug_tuple("Body").field(body).finish(),
        }
    }
}

/// Один открытый response body с точным byte accounting и настоящим HTTP EOF.
pub struct HttpBoundedStreamingBody {
    /// Async response остаётся owner-ом connection/body lifecycle-а.
    response: reqwest::Response,
    /// Secret-safe URL используется только внутри typed transport errors.
    secret_url: SecretHttpUrl,
    /// Operation category сохраняет прежнюю error vocabulary.
    kind: HttpBoundedFetchKind,
    /// Optional exact Range нужен для strict EOF length validation.
    expected_range: Option<HttpBoundedByteRange>,
    /// Общий resource bound действует на сумму всех полученных chunks.
    maximum_body_bytes: usize,
    /// Фактически принятые bytes без хранения полного body.
    received_body_bytes: usize,
    /// Validated Range metadata принадлежит transport owner-у.
    range_metadata: Option<HttpRangeResponseMetadata>,
    /// Максимум времени одного actively-polled чтения следующего body chunk-а.
    read_timeout: Duration,
    /// Повторный read после доказанного EOF остаётся idempotent.
    complete: bool,
    /// One-shot lifecycle физического request-а.
    diagnostics: Box<HttpRequestAttemptDiagnostics>,
}

impl fmt::Debug for HttpBoundedStreamingBody {
    /// Diagnostics показывают только accounting и не раскрывают URL/headers.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpBoundedStreamingBody")
            .field("kind", &self.kind)
            .field("expected_range", &self.expected_range)
            .field("maximum_body_bytes", &self.maximum_body_bytes)
            .field("received_body_bytes", &self.received_body_bytes)
            .field("range_metadata", &self.range_metadata)
            .field("complete", &self.complete)
            .field("request_id", &self.diagnostics.attempt_id())
            .finish()
    }
}

impl HttpBoundedStreamingBody {
    /// Возвращает opaque id физического request attempt-а.
    #[must_use]
    pub const fn request_attempt_id(&self) -> crate::HttpRequestAttemptId {
        self.diagnostics.attempt_id()
    }

    /// Читает следующий wire chunk; `None` означает только validated HTTP EOF.
    pub async fn next_chunk(
        &mut self,
        cancellation: &CancellationToken,
    ) -> SourceResult<Option<Bytes>> {
        if self.complete {
            return Ok(None);
        }
        if cancellation.is_cancelled() {
            self.diagnostics.record_cancelled();
            return Err(SourceError::Cancelled);
        }
        let next_chunk = match tokio::time::timeout(self.read_timeout, self.response.chunk()).await
        {
            Err(_) => {
                self.diagnostics.record_error(
                    HttpRequestDiagnosticError::Timeout,
                    self.received_body_bytes,
                );
                return Err(SourceError::HttpTimeout {
                    operation: self.kind.operation(),
                    url: self.secret_url.clone(),
                });
            }
            Ok(Err(source)) => {
                self.diagnostics.record_error(
                    HttpRequestDiagnosticError::BodyRead,
                    self.received_body_bytes,
                );
                return Err(SourceError::HttpBodyRead {
                    operation: self.kind.operation(),
                    url: self.secret_url.clone(),
                    source: io::Error::other(source.without_url()),
                });
            }
            Ok(Ok(next_chunk)) => next_chunk,
        };
        if cancellation.is_cancelled() {
            self.diagnostics.record_cancelled();
            return Err(SourceError::Cancelled);
        }
        let Some(chunk) = next_chunk else {
            if let Some(expected_range) = self.expected_range
                && self.received_body_bytes != expected_range.length.get()
            {
                self.diagnostics.record_error(
                    HttpRequestDiagnosticError::UnexpectedEof,
                    self.received_body_bytes,
                );
                return Err(SourceError::UnexpectedEof {
                    offset: expected_range.start,
                    expected_bytes: expected_range.length.get(),
                    actual_bytes: self.received_body_bytes,
                });
            }
            self.complete = true;
            self.diagnostics.record_complete(self.received_body_bytes);
            return Ok(None);
        };
        let Some(received_body_bytes) = self.received_body_bytes.checked_add(chunk.len()) else {
            self.diagnostics.record_error(
                HttpRequestDiagnosticError::BodyTooLarge,
                self.received_body_bytes,
            );
            return Err(SourceError::HttpBodyTooLarge {
                operation: self.kind.operation(),
                url: self.secret_url.clone(),
                maximum_bytes: self.maximum_body_bytes,
            });
        };
        if received_body_bytes > self.maximum_body_bytes {
            self.diagnostics.record_error(
                HttpRequestDiagnosticError::BodyTooLarge,
                self.received_body_bytes,
            );
            return Err(SourceError::HttpBodyTooLarge {
                operation: self.kind.operation(),
                url: self.secret_url.clone(),
                maximum_bytes: self.maximum_body_bytes,
            });
        }
        self.received_body_bytes = received_body_bytes;
        self.diagnostics
            .record_body_chunk(chunk.len(), received_body_bytes);
        Ok(Some(chunk))
    }

    /// Возвращает число bytes, уже прошедших resource bound.
    #[must_use]
    pub const fn received_body_bytes(&self) -> usize {
        self.received_body_bytes
    }

    /// Возвращает validated metadata exact Range response-а.
    #[must_use]
    pub fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        self.range_metadata.as_ref()
    }
}

impl HttpSourceSession {
    /// Открывает один bounded hop и возвращает body до его полной загрузки.
    ///
    /// Caller владеет async execution и может drop-нуть `next_chunk` future и
    /// response при supersede. Status/redirect/Range/bound errors совпадают с
    /// существующими fully-buffered frontend-ами.
    pub async fn open_bounded_single_hop_stream(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<HttpBoundedStreamingFetchHop> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let (secret_url, request_headers) = prepare_bounded_request(&request)?;
        let async_client = self.async_client()?;
        let read_timeout = self.async_read_timeout();
        let mut diagnostics = HttpRequestAttemptDiagnostics::started(
            request.diagnostics,
            request.kind.operation(),
            request.diagnostic_bounds(),
        );
        let response = match tokio::time::timeout(
            read_timeout,
            async_client
                .get(request.target.expose_secret_for_request())
                .headers(request_headers)
                .send(),
        )
        .await
        {
            Err(_) => {
                diagnostics.record_error(HttpRequestDiagnosticError::Timeout, 0);
                return Err(SourceError::HttpTimeout {
                    operation: request.kind.operation(),
                    url: secret_url.clone(),
                });
            }
            Ok(Err(source)) => {
                diagnostics.record_error(HttpRequestDiagnosticError::Request, 0);
                return Err(map_reqwest_error(
                    request.kind.operation(),
                    &secret_url,
                    source,
                ));
            }
            Ok(Ok(response)) => response,
        };
        diagnostics.record_headers_ready(response.status().as_u16());
        if cancellation.is_cancelled() {
            diagnostics.record_cancelled();
            return Err(SourceError::Cancelled);
        }
        let validated_response =
            validate_bounded_response(&request, &secret_url, response.status(), response.headers());
        let range_metadata = match validated_response {
            Err(error) => {
                diagnostics.record_error(HttpRequestDiagnosticError::ResponsePolicy, 0);
                return Err(error);
            }
            Ok(ValidatedBoundedResponse::Redirect(redirect)) => {
                diagnostics.record_redirect();
                return Ok(HttpBoundedStreamingFetchHop::Redirect(redirect));
            }
            Ok(ValidatedBoundedResponse::Body(range_metadata)) => range_metadata,
        };
        let maximum_body_bytes = request.maximum_body_bytes.get();
        let maximum_body_bytes_u64 = u64::try_from(maximum_body_bytes).unwrap_or(u64::MAX);
        if response
            .content_length()
            .is_some_and(|content_length| content_length > maximum_body_bytes_u64)
        {
            diagnostics.record_error(HttpRequestDiagnosticError::BodyTooLarge, 0);
            return Err(SourceError::HttpBodyTooLarge {
                operation: request.kind.operation(),
                url: secret_url,
                maximum_bytes: maximum_body_bytes,
            });
        }
        Ok(HttpBoundedStreamingFetchHop::Body(
            HttpBoundedStreamingBody {
                response,
                secret_url,
                kind: request.kind,
                expected_range: request.byte_range,
                maximum_body_bytes,
                received_body_bytes: 0,
                range_metadata,
                read_timeout,
                complete: false,
                diagnostics: Box::new(diagnostics),
            },
        ))
    }
}
