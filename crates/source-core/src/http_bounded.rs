//! Bounded full-resource и exact Range GET поверх единственной HTTP session.

use std::fmt;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::time::SystemTime;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue, RANGE};

use crate::http::{
    ByteRange, build_header_map, map_reqwest_error, validate_content_range, validators_from_headers,
};
use crate::http_retry_after::retry_after_from_headers;
use crate::http_session::parse_redirect_headers;
use crate::{
    CancellationToken, HttpHeader, HttpRedirectHop, HttpRequestTarget, HttpSourceSession,
    SecretHttpUrl, SourceError, SourceResult, SourceValidators,
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
        let mut response = self
            .client
            .get(request.target.expose_secret_for_request())
            .headers(request_headers)
            .send()
            .map_err(|source| map_reqwest_error(request.kind.operation(), &secret_url, source))?;
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let range_metadata = match validate_bounded_response(
            &request,
            &secret_url,
            response.status(),
            response.headers(),
        )? {
            ValidatedBoundedResponse::Redirect(redirect) => {
                return Ok(HttpBoundedFetchHop::Redirect(redirect));
            }
            ValidatedBoundedResponse::Body(range_metadata) => range_metadata,
        };

        let maximum_body_bytes = request.maximum_body_bytes.get();
        let mut bytes = Vec::with_capacity(maximum_body_bytes.min(64 * 1024));
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let read_bytes =
                response
                    .read(&mut chunk)
                    .map_err(|source| SourceError::HttpBodyRead {
                        operation: request.kind.operation(),
                        url: secret_url.clone(),
                        source,
                    })?;
            if read_bytes == 0 {
                break;
            }
            if bytes.len().saturating_add(read_bytes) > maximum_body_bytes {
                return Err(SourceError::HttpBodyTooLarge {
                    operation: request.kind.operation(),
                    url: secret_url,
                    maximum_bytes: maximum_body_bytes,
                });
            }
            bytes.extend_from_slice(&chunk[..read_bytes]);
        }

        finish_bounded_response(&request, bytes, range_metadata)
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
        let mut response = async_client
            .get(request.target.expose_secret_for_request())
            .headers(request_headers)
            .send()
            .await
            .map_err(|source| map_reqwest_error(request.kind.operation(), &secret_url, source))?;
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let range_metadata = match validate_bounded_response(
            &request,
            &secret_url,
            response.status(),
            response.headers(),
        )? {
            ValidatedBoundedResponse::Redirect(redirect) => {
                return Ok(HttpBoundedFetchHop::Redirect(redirect));
            }
            ValidatedBoundedResponse::Body(range_metadata) => range_metadata,
        };

        let maximum_body_bytes = request.maximum_body_bytes.get();
        let mut bytes = Vec::with_capacity(maximum_body_bytes.min(64 * 1024));
        loop {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let Some(chunk) =
                response
                    .chunk()
                    .await
                    .map_err(|source| SourceError::HttpBodyRead {
                        operation: request.kind.operation(),
                        url: secret_url.clone(),
                        source: io::Error::other(source.without_url()),
                    })?
            else {
                break;
            };
            if bytes.len().saturating_add(chunk.len()) > maximum_body_bytes {
                return Err(SourceError::HttpBodyTooLarge {
                    operation: request.kind.operation(),
                    url: secret_url,
                    maximum_bytes: maximum_body_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }

        finish_bounded_response(&request, bytes, range_metadata)
    }
}
