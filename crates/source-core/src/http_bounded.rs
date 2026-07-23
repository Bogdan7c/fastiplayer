//! Bounded full-resource и exact Range GET поверх единственной HTTP session.

use std::fmt;
use std::io::Read;
use std::num::NonZeroUsize;

use reqwest::StatusCode;
use reqwest::header::{HeaderValue, RANGE};

use crate::http::{ByteRange, build_header_map, map_reqwest_error, validate_content_range};
use crate::http_session::parse_redirect_hop;
use crate::{
    CancellationToken, HttpHeader, HttpRedirectHop, HttpRequestTarget, HttpSourceSession,
    SecretHttpUrl, SourceError, SourceResult,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpBoundedResponse {
    /// Exact body без container/metadata parsing.
    bytes: Vec<u8>,
}

impl HttpBoundedResponse {
    /// Передаёт exact body transport owner-у.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
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

        let secret_url = SecretHttpUrl::from_secret_for_open(
            request.target.expose_secret_for_request().to_owned(),
        );
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

        let mut response = self
            .client
            .get(request.target.expose_secret_for_request())
            .headers(request_headers)
            .send()
            .map_err(|source| map_reqwest_error(request.kind.operation(), &secret_url, source))?;
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        if response.status().is_redirection() {
            return parse_redirect_hop(&request.target, &secret_url, response.status(), &response)
                .map(HttpBoundedFetchHop::Redirect);
        }

        match request.byte_range {
            None if response.status() != StatusCode::OK => {
                return Err(SourceError::HttpStatus {
                    operation: request.kind.operation(),
                    url: secret_url,
                    status: response.status(),
                });
            }
            Some(_) if response.status() == StatusCode::OK => {
                return Err(SourceError::HttpRangeUnsupported {
                    reason: crate::NotSeekableReason::HttpRangeStatus {
                        status: response.status().as_u16(),
                    },
                });
            }
            Some(_) if response.status() != StatusCode::PARTIAL_CONTENT => {
                return Err(SourceError::HttpStatus {
                    operation: request.kind.operation(),
                    url: secret_url,
                    status: response.status(),
                });
            }
            Some(byte_range) => {
                let range = ByteRange::new(byte_range.start, byte_range.length.get());
                validate_content_range(&secret_url, response.headers(), &range)?;
            }
            None => {}
        }

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

        if let Some(byte_range) = request.byte_range
            && bytes.len() != byte_range.length.get()
        {
            return Err(SourceError::UnexpectedEof {
                offset: byte_range.start,
                expected_bytes: byte_range.length.get(),
                actual_bytes: bytes.len(),
            });
        }
        Ok(HttpBoundedFetchHop::Complete(HttpBoundedResponse { bytes }))
    }
}
