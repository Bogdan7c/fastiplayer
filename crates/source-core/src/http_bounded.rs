//! Bounded full-resource и exact Range GET поверх единственной HTTP session.

use std::fmt;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue, RANGE};

use crate::http::{
    ByteRange, build_header_map, map_reqwest_error, validate_content_range, validators_from_headers,
};
use crate::http_diagnostics::{
    HttpRequestAttemptDiagnostics, HttpRequestDiagnosticBounds, HttpRequestDiagnosticError,
};
use crate::http_retry_after::retry_after_from_headers;
use crate::http_session::parse_redirect_headers;
use crate::{
    CancellationToken, HttpHeader, HttpRedirectHop, HttpRequestTarget, HttpResourceDiagnostics,
    HttpResourcePurpose, HttpSourceSession, SecretHttpUrl, SourceError, SourceResult,
    SourceValidators,
};

/// Optional exact byte range для одного bounded resource request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpBoundedByteRange {
    /// Первый byte ресурса.
    start: u64,
    /// Exact ненулевая длина response body.
    length: NonZeroUsize,
}

impl HttpBoundedByteRange {
    /// Создаёт checked range и заранее отвергает overflow конечного offset-а.
    pub fn new(start: u64, length: NonZeroUsize) -> SourceResult<Self> {
        let length_minus_one =
            u64::try_from(length.get() - 1).map_err(|_| SourceError::InvalidConfig {
                field: "http_bounded_byte_range.length",
                message: "range length does not fit u64".to_owned(),
            })?;
        start
            .checked_add(length_minus_one)
            .ok_or_else(|| SourceError::InvalidConfig {
                field: "http_bounded_byte_range",
                message: "range end overflows u64".to_owned(),
            })?;
        Ok(Self { start, length })
    }

    /// Возвращает первый byte ресурса.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Возвращает exact длину range-а.
    #[must_use]
    pub const fn length(self) -> NonZeroUsize {
        self.length
    }
}

/// Intent одного generic bounded resource request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpBoundedFetchKind {
    /// Небольшой metadata/index document.
    Metadata,
    /// Encoded media bytes.
    Media,
}

impl HttpBoundedFetchKind {
    const fn operation(self) -> &'static str {
        match self {
            Self::Metadata => "bounded-metadata-fetch",
            Self::Media => "bounded-media-fetch",
        }
    }

    /// Даёт neutral fallback purpose для non-adaptive caller-ов.
    const fn default_resource_purpose(self) -> HttpResourcePurpose {
        match self {
            Self::Metadata => HttpResourcePurpose::GenericMetadata,
            Self::Media => HttpResourcePurpose::GenericMedia,
        }
    }
}

/// Один GET request bounded metadata/media resource-а.
pub struct HttpBoundedFetchRequest {
    /// Exact validated target.
    target: HttpRequestTarget,
    /// Уже проверенные caller-owned headers.
    headers: Vec<HttpHeader>,
    /// Optional exact Range; отсутствие означает полный resource.
    byte_range: Option<HttpBoundedByteRange>,
    /// Верхняя граница buffered body.
    maximum_body_bytes: NonZeroUsize,
    /// Стабильная operation category для secret-safe diagnostics.
    kind: HttpBoundedFetchKind,
    /// Logical resource correlation сохраняется через redirect/retry без locator-а.
    diagnostics: HttpResourceDiagnostics,
}

impl HttpBoundedFetchRequest {
    /// Создаёт полный bounded GET.
    #[must_use]
    pub fn full(
        target: HttpRequestTarget,
        headers: Vec<HttpHeader>,
        maximum_body_bytes: NonZeroUsize,
        kind: HttpBoundedFetchKind,
    ) -> Self {
        Self {
            target,
            headers,
            byte_range: None,
            maximum_body_bytes,
            kind,
            diagnostics: HttpResourceDiagnostics::started(kind.default_resource_purpose()),
        }
    }

    /// Создаёт exact bounded Range GET.
    #[must_use]
    pub fn range(
        target: HttpRequestTarget,
        headers: Vec<HttpHeader>,
        byte_range: HttpBoundedByteRange,
        kind: HttpBoundedFetchKind,
    ) -> Self {
        Self {
            target,
            headers,
            byte_range: Some(byte_range),
            maximum_body_bytes: byte_range.length,
            kind,
            diagnostics: HttpResourceDiagnostics::started(kind.default_resource_purpose()),
        }
    }

    /// Привязывает request к уже начатому logical resource-у.
    #[must_use]
    pub fn with_resource_diagnostics(mut self, diagnostics: HttpResourceDiagnostics) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Формирует bounded shape без request target-а и headers.
    const fn diagnostic_bounds(&self) -> HttpRequestDiagnosticBounds {
        HttpRequestDiagnosticBounds {
            range_start: match self.byte_range {
                Some(byte_range) => Some(byte_range.start),
                None => None,
            },
            requested_bytes: self.maximum_body_bytes.get(),
        }
    }
}

impl fmt::Debug for HttpBoundedFetchRequest {
    /// Не раскрывает target или значения headers.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpBoundedFetchRequest")
            .field("target", &self.target)
            .field("header_count", &self.headers.len())
            .field("byte_range", &self.byte_range)
            .field("maximum_body_bytes", &self.maximum_body_bytes)
            .field("kind", &self.kind)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

/// Успешно прочитанный bounded HTTP resource.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRangeResponseMetadata {
    /// Полная длина representation из validated `Content-Range`, если server её сообщил.
    total_resource_bytes: Option<u64>,
    /// Exact representation validators; значения доступны только transport owner-у.
    validators: SourceValidators,
}

impl HttpRangeResponseMetadata {
    /// Возвращает доказанную полную длину representation.
    #[must_use]
    pub const fn total_resource_bytes(&self) -> Option<u64> {
        self.total_resource_bytes
    }

    /// Передаёт validators следующему secret-aware transport boundary.
    #[must_use]
    pub fn validators(&self) -> SourceValidators {
        self.validators.clone()
    }
}

impl fmt::Debug for HttpRangeResponseMetadata {
    /// Не раскрывает значения ETag и Last-Modified.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRangeResponseMetadata")
            .field("total_resource_bytes", &self.total_resource_bytes)
            .field("has_etag", &self.validators.etag.is_some())
            .field(
                "has_last_modified",
                &self.validators.last_modified.is_some(),
            )
            .finish()
    }
}

/// Успешно прочитанный bounded HTTP resource.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpBoundedResponse {
    /// Exact body без container/metadata parsing.
    bytes: Vec<u8>,
    /// Validated wire metadata только для exact Range response.
    range_metadata: Option<HttpRangeResponseMetadata>,
}

impl HttpBoundedResponse {
    /// Возвращает Range metadata, не раскрывая header values через formatting.
    #[must_use]
    pub const fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        self.range_metadata.as_ref()
    }

    /// Передаёт exact body transport owner-у.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Debug for HttpBoundedResponse {
    /// Показывает только размер body и safe Range metadata.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpBoundedResponse")
            .field("body_bytes", &self.bytes.len())
            .field("range_metadata", &self.range_metadata)
            .finish()
    }
}

/// Результат одного bounded resource hop-а до redirect policy решения.
#[derive(Debug)]
pub enum HttpBoundedFetchHop {
    /// Сервер потребовал следующий policy-controlled hop.
    Redirect(HttpRedirectHop),
    /// Сервер вернул допустимый bounded body.
    Complete(HttpBoundedResponse),
}

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

/// Общая status/header validation blocking и async response frontends.
enum ValidatedBoundedResponse {
    /// Redirect body не читается до решения transport policy.
    Redirect(HttpRedirectHop),
    /// Допустимый body с optional exact Range metadata.
    Body(Option<HttpRangeResponseMetadata>),
}

/// Проецирует typed request в secret-safe URL и reqwest headers один раз.
fn prepare_bounded_request(
    request: &HttpBoundedFetchRequest,
) -> SourceResult<(SecretHttpUrl, HeaderMap)> {
    let secret_url =
        SecretHttpUrl::from_secret_for_open(request.target.expose_secret_for_request().to_owned());
    let mut request_headers = build_header_map(&request.headers)?;
    request_headers.remove(RANGE);
    if let Some(byte_range) = request.byte_range {
        let range = ByteRange::new(byte_range.start, byte_range.length.get());
        request_headers.insert(
            RANGE,
            HeaderValue::from_str(&range.header_value()).map_err(|_| {
                SourceError::InvalidHttpHeaderValue {
                    name: "Range".to_owned(),
                }
            })?,
        );
    }
    Ok((secret_url, request_headers))
}

/// Сохраняет единые redirect/status/Range инварианты для обоих I/O frontends.
fn validate_bounded_response(
    request: &HttpBoundedFetchRequest,
    secret_url: &SecretHttpUrl,
    response_status: StatusCode,
    response_headers: &HeaderMap,
) -> SourceResult<ValidatedBoundedResponse> {
    if response_status.is_redirection() {
        return parse_redirect_headers(
            &request.target,
            secret_url,
            response_status,
            response_headers,
        )
        .map(ValidatedBoundedResponse::Redirect);
    }

    let range_metadata = match request.byte_range {
        None if response_status != StatusCode::OK => {
            return Err(SourceError::HttpStatus {
                operation: request.kind.operation(),
                url: secret_url.clone(),
                status: response_status,
                retry_after: retry_after_from_headers(response_headers, SystemTime::now()),
            });
        }
        Some(_) if response_status == StatusCode::OK => {
            return Err(SourceError::HttpRangeUnsupported {
                reason: crate::NotSeekableReason::HttpRangeStatus {
                    status: response_status.as_u16(),
                },
            });
        }
        Some(_) if response_status != StatusCode::PARTIAL_CONTENT => {
            return Err(SourceError::HttpStatus {
                operation: request.kind.operation(),
                url: secret_url.clone(),
                status: response_status,
                retry_after: retry_after_from_headers(response_headers, SystemTime::now()),
            });
        }
        Some(byte_range) => {
            let range = ByteRange::new(byte_range.start, byte_range.length.get());
            let parsed_range = validate_content_range(secret_url, response_headers, &range)?;
            Some(HttpRangeResponseMetadata {
                total_resource_bytes: parsed_range.total_length,
                validators: validators_from_headers(response_headers),
            })
        }
        None => None,
    };
    Ok(ValidatedBoundedResponse::Body(range_metadata))
}

/// Проверяет exact Range accounting и собирает успешный hop без I/O знаний.
fn finish_bounded_response(
    request: &HttpBoundedFetchRequest,
    bytes: Vec<u8>,
    range_metadata: Option<HttpRangeResponseMetadata>,
) -> SourceResult<HttpBoundedFetchHop> {
    if let Some(byte_range) = request.byte_range
        && bytes.len() != byte_range.length.get()
    {
        return Err(SourceError::UnexpectedEof {
            offset: byte_range.start,
            expected_bytes: byte_range.length.get(),
            actual_bytes: bytes.len(),
        });
    }
    Ok(HttpBoundedFetchHop::Complete(HttpBoundedResponse {
        bytes,
        range_metadata,
    }))
}

impl HttpSourceSession {
    /// Выполняет ровно один bounded GET hop для metadata либо media bytes.
    pub fn fetch_bounded_single_hop(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<HttpBoundedFetchHop> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let (secret_url, request_headers) = prepare_bounded_request(&request)?;
        let mut diagnostics = HttpRequestAttemptDiagnostics::started(
            request.diagnostics,
            request.kind.operation(),
            request.diagnostic_bounds(),
        );
        let mut response = match self
            .client
            .get(request.target.expose_secret_for_request())
            .headers(request_headers)
            .send()
        {
            Ok(response) => response,
            Err(source) => {
                diagnostics.record_error(HttpRequestDiagnosticError::Request, 0);
                return Err(map_reqwest_error(
                    request.kind.operation(),
                    &secret_url,
                    source,
                ));
            }
        };
        diagnostics.record_headers_ready(response.status().as_u16());
        if cancellation.is_cancelled() {
            diagnostics.record_cancelled();
            return Err(SourceError::Cancelled);
        }

        let range_metadata = match validate_bounded_response(
            &request,
            &secret_url,
            response.status(),
            response.headers(),
        ) {
            Err(error) => {
                diagnostics.record_error(HttpRequestDiagnosticError::ResponsePolicy, 0);
                return Err(error);
            }
            Ok(ValidatedBoundedResponse::Redirect(redirect)) => {
                diagnostics.record_redirect();
                return Ok(HttpBoundedFetchHop::Redirect(redirect));
            }
            Ok(ValidatedBoundedResponse::Body(range_metadata)) => range_metadata,
        };

        let maximum_body_bytes = request.maximum_body_bytes.get();
        let mut bytes = Vec::with_capacity(maximum_body_bytes.min(64 * 1024));
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            if cancellation.is_cancelled() {
                diagnostics.record_cancelled();
                return Err(SourceError::Cancelled);
            }
            let read_bytes = match response.read(&mut chunk) {
                Ok(read_bytes) => read_bytes,
                Err(source) => {
                    diagnostics.record_error(HttpRequestDiagnosticError::BodyRead, bytes.len());
                    return Err(SourceError::HttpBodyRead {
                        operation: request.kind.operation(),
                        url: secret_url.clone(),
                        source,
                    });
                }
            };
            if read_bytes == 0 {
                break;
            }
            if bytes.len().saturating_add(read_bytes) > maximum_body_bytes {
                diagnostics.record_error(HttpRequestDiagnosticError::BodyTooLarge, bytes.len());
                return Err(SourceError::HttpBodyTooLarge {
                    operation: request.kind.operation(),
                    url: secret_url,
                    maximum_bytes: maximum_body_bytes,
                });
            }
            bytes.extend_from_slice(&chunk[..read_bytes]);
            diagnostics.record_body_chunk(read_bytes, bytes.len());
        }

        let received_bytes = bytes.len();
        match finish_bounded_response(&request, bytes, range_metadata) {
            Ok(response) => {
                diagnostics.record_complete(received_bytes);
                Ok(response)
            }
            Err(error) => {
                diagnostics.record_error(HttpRequestDiagnosticError::UnexpectedEof, received_bytes);
                Err(error)
            }
        }
    }

    /// Выполняет один bounded GET hop как abortable future.
    ///
    /// Drop returned future уничтожает request/response до configured timeout-а.
    /// Status, Range, body-limit и secret-safe error semantics совпадают с
    /// blocking `fetch_bounded_single_hop`.
    pub async fn fetch_bounded_single_hop_abortable(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<HttpBoundedFetchHop> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let (secret_url, request_headers) = prepare_bounded_request(&request)?;
        let async_client = self.async_client()?;
        let operation_deadline = tokio::time::Instant::now() + self.async_read_timeout();
        let mut diagnostics = HttpRequestAttemptDiagnostics::started(
            request.diagnostics,
            request.kind.operation(),
            request.diagnostic_bounds(),
        );
        let mut response = match tokio::time::timeout_at(
            operation_deadline,
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

        let range_metadata = match validate_bounded_response(
            &request,
            &secret_url,
            response.status(),
            response.headers(),
        ) {
            Err(error) => {
                diagnostics.record_error(HttpRequestDiagnosticError::ResponsePolicy, 0);
                return Err(error);
            }
            Ok(ValidatedBoundedResponse::Redirect(redirect)) => {
                diagnostics.record_redirect();
                return Ok(HttpBoundedFetchHop::Redirect(redirect));
            }
            Ok(ValidatedBoundedResponse::Body(range_metadata)) => range_metadata,
        };

        let maximum_body_bytes = request.maximum_body_bytes.get();
        let mut bytes = Vec::with_capacity(maximum_body_bytes.min(64 * 1024));
        loop {
            if cancellation.is_cancelled() {
                diagnostics.record_cancelled();
                return Err(SourceError::Cancelled);
            }
            let next_chunk =
                match tokio::time::timeout_at(operation_deadline, response.chunk()).await {
                    Err(_) => {
                        diagnostics.record_error(HttpRequestDiagnosticError::Timeout, bytes.len());
                        return Err(SourceError::HttpTimeout {
                            operation: request.kind.operation(),
                            url: secret_url.clone(),
                        });
                    }
                    Ok(Err(source)) => {
                        diagnostics.record_error(HttpRequestDiagnosticError::BodyRead, bytes.len());
                        return Err(SourceError::HttpBodyRead {
                            operation: request.kind.operation(),
                            url: secret_url.clone(),
                            source: io::Error::other(source.without_url()),
                        });
                    }
                    Ok(Ok(next_chunk)) => next_chunk,
                };
            let Some(chunk) = next_chunk else {
                break;
            };
            if bytes.len().saturating_add(chunk.len()) > maximum_body_bytes {
                diagnostics.record_error(HttpRequestDiagnosticError::BodyTooLarge, bytes.len());
                return Err(SourceError::HttpBodyTooLarge {
                    operation: request.kind.operation(),
                    url: secret_url,
                    maximum_bytes: maximum_body_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
            diagnostics.record_body_chunk(chunk.len(), bytes.len());
        }

        let received_bytes = bytes.len();
        match finish_bounded_response(&request, bytes, range_metadata) {
            Ok(response) => {
                diagnostics.record_complete(received_bytes);
                Ok(response)
            }
            Err(error) => {
                diagnostics.record_error(HttpRequestDiagnosticError::UnexpectedEof, received_bytes);
                Err(error)
            }
        }
    }

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
