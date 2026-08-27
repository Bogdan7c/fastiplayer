use std::fmt;
use std::io::Read;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap, HeaderName, HeaderValue, LAST_MODIFIED, RANGE,
};

use crate::http_client::blocking_http_client_builder;
use crate::http_session::parse_redirect_hop;
use crate::{
    ByteSource, CancellationToken, HttpRangeRedirectBodyForwarding, HttpRangeRedirectHandler,
    HttpRangeRedirectHopCount, HttpRedirectRequestBehavior, HttpRequestTarget, NotSeekableReason,
    RangeDiagnostics, SecretHttpUrl, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceRuntimeConfig, SourceValidators,
};

/// HTTP header, который внешний service layer передал как данные direct URL-а.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpHeader {
    /// Имя HTTP header-а.
    pub name: String,

    /// Значение HTTP header-а.
    pub value: String,
}

impl fmt::Debug for HttpHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl HttpHeader {
    /// Создаёт header без validation; validation выполняется при открытии source.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Конфигурация HTTP Range source.
#[derive(Debug, Clone)]
pub struct HttpRangeSourceConfig {
    /// Direct media URL.
    url: SecretHttpUrl,

    /// Headers direct URL-а, полученные от caller-а.
    headers: Vec<HttpHeader>,

    /// Runtime-настройки, полученные из пользовательского config.
    source_config: SourceRuntimeConfig,
}

impl HttpRangeSourceConfig {
    /// Создаёт конфигурацию HTTP source без hardcoded service headers.
    #[must_use]
    pub fn new(
        url: SecretHttpUrl,
        headers: Vec<HttpHeader>,
        source_config: SourceRuntimeConfig,
    ) -> Self {
        Self {
            url,
            headers,
            source_config,
        }
    }
}

/// HTTP byte source, который читает только через Range requests.
pub struct HttpRangeSource {
    /// Reqwest blocking client с timeout-ами из config.
    client: Client,

    /// Stable base URL, с которого начинается каждый логический Range read.
    url: SecretHttpUrl,

    /// Stable base target для безопасного разрешения relative redirect-а.
    target: HttpRequestTarget,

    /// Базовые headers direct URL-а.
    headers: HeaderMap,

    /// Ephemeral request body, который нужно повторять для каждого Range request-а.
    request_body: Option<Vec<u8>>,

    /// Transport-owned policy hook; legacy direct source не следует read-time redirect-ам.
    redirect_handler: Option<Box<dyn HttpRangeRedirectHandler>>,

    /// Текущий byte cursor.
    position: u64,

    /// Seekability, подтверждённая probe-запросом.
    seekability: Seekability,

    /// Validators из HTTP response.
    validators: SourceValidators,

    /// Длина source-а, если server сообщил total bytes.
    content_length: Option<u64>,

    /// Fingerprint source-а.
    fingerprint: SourceFingerprint,

    /// Range diagnostics для telemetry panel.
    diagnostics: RangeDiagnostics,
}

impl fmt::Debug for HttpRangeSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRangeSource")
            .field("url", &self.url)
            .field("headers", &"<redacted>")
            .field("has_request_body", &self.request_body.is_some())
            .field("has_redirect_handler", &self.redirect_handler.is_some())
            .field("position", &self.position)
            .field("seekability", &self.seekability)
            .field("validators", &self.validators)
            .field("content_length", &self.content_length)
            .field("fingerprint", &self.fingerprint)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl HttpRangeSource {
    /// Открывает HTTP Range source и выполняет seekability probe.
    pub fn open(config: HttpRangeSourceConfig) -> SourceResult<Self> {
        let client = blocking_http_client_builder(&config.source_config)
            .build()
            .map_err(|source| SourceError::HttpClientBuild { source })?;
        let headers = build_header_map(&config.headers)?;
        let probe = probe_seekability(&client, &config.url, &headers)?;
        let fingerprint =
            build_http_fingerprint(&config.url, probe.content_length, &probe.validators);

        let target =
            HttpRequestTarget::parse_exact(config.url.expose_secret_for_open()).map_err(|_| {
                SourceError::InvalidHttpRedirect {
                    url: config.url.clone(),
                    reason: "invalid-current-target",
                }
            })?;

        Ok(Self {
            client,
            url: config.url,
            target,
            headers,
            request_body: None,
            redirect_handler: None,
            position: 0,
            seekability: probe.seekability,
            validators: probe.validators,
            content_length: probe.content_length,
            fingerprint,
            diagnostics: RangeDiagnostics::default(),
        })
    }

    /// Строит seekable source из уже проверенного `206` ответа одного HTTP session-а.
    pub(crate) fn from_partial_content_probe(
        client: Client,
        target: HttpRequestTarget,
        headers: HeaderMap,
        request_body: Option<Vec<u8>>,
        response_headers: &HeaderMap,
    ) -> SourceResult<Self> {
        let url =
            SecretHttpUrl::from_secret_for_open(target.expose_secret_for_request().to_owned());
        let parsed_range = parse_content_range_header(&url, response_headers)?;
        if parsed_range.start != 0 || parsed_range.end_inclusive != 0 {
            return Err(SourceError::InvalidContentRange {
                url,
                header: "<unexpected-probe-range>".to_string(),
            });
        }
        let validators = validators_from_headers(response_headers);
        let content_length = parsed_range.total_length;
        let fingerprint = build_http_fingerprint(&url, content_length, &validators);

        Ok(Self {
            client,
            url,
            target,
            headers,
            request_body,
            redirect_handler: None,
            position: 0,
            seekability: Seekability::Seekable,
            validators,
            content_length,
            fingerprint,
            diagnostics: RangeDiagnostics::default(),
        })
    }

    /// Подключает policy owner-а для redirect-ов последующих Range request-ов.
    #[must_use]
    pub fn with_range_redirect_handler(
        mut self,
        redirect_handler: Box<dyn HttpRangeRedirectHandler>,
    ) -> Self {
        self.redirect_handler = Some(redirect_handler);
        self
    }

    /// Возвращает текущие HTTP Range counters.
    #[must_use]
    pub const fn range_diagnostics(&self) -> RangeDiagnostics {
        self.diagnostics
    }

    /// Читает один bounded range с единственным retry при transport/body failure.
    fn read_range_with_retry(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        let mut attempts = 0_u8;
        let length = output.len();

        loop {
            let result = self.read_range_once(offset, output, cancellation);
            match result {
                Ok(bytes_read) => return Ok(bytes_read),
                Err(error) if attempts == 0 && error.is_retryable_range_failure() => {
                    self.record_range_error(&error);
                    attempts = attempts.saturating_add(1);
                    tracing::warn!(
                        source = %self.url,
                        offset,
                        length,
                        error = %error,
                        "HTTP Range read failed; retrying once"
                    );
                }
                Err(error) => {
                    self.record_range_error(&error);
                    return Err(error);
                }
            }
        }
    }

    /// Выполняет один HTTP Range request и полностью читает response body.
    fn read_range_once(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let length = output.len();
        let range = ByteRange::new(offset, length);
        let mut current_target = self.target.clone();
        let mut current_url = self.url.clone();
        let mut current_headers = self.headers.clone();
        let mut current_request_body = self.request_body.clone();
        let mut completed_redirect_hops = HttpRangeRedirectHopCount::none();
        if let Some(redirect_handler) = self.redirect_handler.as_mut() {
            redirect_handler.begin_range_request();
        }

        loop {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }

            self.diagnostics.range_requests = self.diagnostics.range_requests.saturating_add(1);
            self.diagnostics.bytes_requested = self
                .diagnostics
                .bytes_requested
                .saturating_add(length as u64);
            let response = send_range_request(
                &self.client,
                &current_url,
                &current_headers,
                current_request_body.as_deref(),
                &range,
                "range-read",
            )?;

            if response.status().is_redirection() {
                let (next_target, next_url, next_headers, next_request_body) = self
                    .follow_range_redirect(
                        &current_target,
                        &current_url,
                        current_request_body.as_deref(),
                        completed_redirect_hops,
                        &response,
                    )?;
                current_target = next_target;
                current_url = next_url;
                current_headers = next_headers;
                current_request_body = next_request_body;
                completed_redirect_hops = completed_redirect_hops.checked_next().ok_or(
                    SourceError::HttpRangeRedirectRejected {
                        url: current_url.clone(),
                        reason: crate::HttpRangeRedirectRejection::PolicyRejected,
                    },
                )?;
                continue;
            }

            if response.status() != StatusCode::PARTIAL_CONTENT {
                if response.status() == StatusCode::OK {
                    return Err(SourceError::HttpRangeUnsupported {
                        reason: NotSeekableReason::HttpRangeStatus {
                            status: response.status().as_u16(),
                        },
                    });
                }

                return Err(SourceError::HttpStatus {
                    operation: "range-read",
                    url: current_url,
                    status: response.status(),
                    retry_after: crate::HttpRetryAfter::Unavailable,
                });
            }

            validate_content_range(&current_url, response.headers(), &range)?;
            let bytes_read =
                read_response_body_into(&current_url, response, offset, output, cancellation)?;
            self.diagnostics.bytes_read = self
                .diagnostics
                .bytes_read
                .saturating_add(bytes_read as u64);
            return Ok(bytes_read);
        }
    }

    /// Передаёт read-time redirect policy owner-у, не меняя stable base source-а.
    fn follow_range_redirect(
        &mut self,
        current_target: &HttpRequestTarget,
        current_url: &SecretHttpUrl,
        current_request_body: Option<&[u8]>,
        completed_hops: HttpRangeRedirectHopCount,
        response: &Response,
    ) -> SourceResult<(HttpRequestTarget, SecretHttpUrl, HeaderMap, Option<Vec<u8>>)> {
        let redirect =
            parse_redirect_hop(current_target, current_url, response.status(), response)?;
        let Some(redirect_handler) = self.redirect_handler.as_mut() else {
            return Err(SourceError::HttpStatus {
                operation: "range-read",
                url: current_url.clone(),
                status: response.status(),
                retry_after: crate::HttpRetryAfter::Unavailable,
            });
        };
        let next_material = redirect_handler
            .material_for_redirect(current_target, &redirect, completed_hops)
            .map_err(|reason| SourceError::HttpRangeRedirectRejected {
                url: current_url.clone(),
                reason,
            })?;
        let next_target = redirect.target().clone();
        let request_behavior = redirect.request_behavior();
        let (next_headers, body_forwarding) = next_material.into_parts();
        let next_url =
            SecretHttpUrl::from_secret_for_open(next_target.expose_secret_for_request().to_owned());
        let next_headers = build_header_map(&next_headers)?;
        let next_request_body = match (request_behavior, body_forwarding) {
            (
                HttpRedirectRequestBehavior::PreserveMethodAndBody,
                HttpRangeRedirectBodyForwarding::PreserveCurrent,
            ) => current_request_body.map(<[u8]>::to_vec),
            (
                HttpRedirectRequestBehavior::PreserveMethodAndBody
                | HttpRedirectRequestBehavior::SwitchToGetWithoutBody,
                HttpRangeRedirectBodyForwarding::Drop,
            )
            | (
                HttpRedirectRequestBehavior::SwitchToGetWithoutBody,
                HttpRangeRedirectBodyForwarding::PreserveCurrent,
            ) => None,
        };

        Ok((next_target, next_url, next_headers, next_request_body))
    }

    /// Учитывает ошибку range path в diagnostics.
    fn record_range_error(&mut self, error: &SourceError) {
        if matches!(error, SourceError::HttpTimeout { .. }) {
            self.diagnostics.timeouts = self.diagnostics.timeouts.saturating_add(1);
        }
    }
}

impl ByteSource for HttpRangeSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        if let Seekability::NotSeekable { reason } = &self.seekability {
            return Err(SourceError::NotSeekable {
                reason: reason.clone(),
            });
        }

        let requested_length =
            bounded_read_length(self.position, output.len(), self.content_length);
        if requested_length == 0 {
            return Ok(0);
        }

        let bytes_read = self.read_range_with_retry(
            self.position,
            &mut output[..requested_length],
            cancellation,
        )?;
        self.position = self.position.saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        self.seekability.clone()
    }

    fn validators(&self) -> SourceValidators {
        self.validators.clone()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ByteRange {
    /// Начальный byte offset.
    start: u64,

    /// Количество bytes в range.
    length: usize,
}

impl ByteRange {
    /// Создаёт bounded byte range.
    pub(crate) const fn new(start: u64, length: usize) -> Self {
        Self { start, length }
    }

    /// Возвращает последний byte offset включительно.
    fn end_inclusive(self) -> u64 {
        self.start
            .saturating_add(self.length as u64)
            .saturating_sub(1)
    }

    /// Форматирует header `Range`.
    pub(crate) fn header_value(self) -> String {
        format!("bytes={}-{}", self.start, self.end_inclusive())
    }
}

#[derive(Debug)]
struct ProbeResult {
    /// Seekability после Range probe.
    seekability: Seekability,

    /// Validators из probe response.
    validators: SourceValidators,

    /// Длина source-а, если известна.
    content_length: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ParsedContentRange {
    /// Начальный offset response range.
    pub(crate) start: u64,

    /// Конечный offset response range включительно.
    pub(crate) end_inclusive: u64,

    /// Общая длина source-а, если server её сообщил.
    pub(crate) total_length: Option<u64>,
}

/// Выполняет `Range: bytes=0-0` и подтверждает seekability только через 206.
fn probe_seekability(
    client: &Client,
    url: &SecretHttpUrl,
    headers: &HeaderMap,
) -> SourceResult<ProbeResult> {
    let probe_range = ByteRange::new(0, 1);
    let response = send_range_request(client, url, headers, None, &probe_range, "range-probe")?;
    let validators = validators_from_headers(response.headers());

    if response.status() == StatusCode::PARTIAL_CONTENT {
        let parsed_range = parse_content_range_header(url, response.headers())?;
        return Ok(ProbeResult {
            seekability: Seekability::Seekable,
            validators,
            content_length: parsed_range.total_length,
        });
    }

    if response.status() == StatusCode::OK {
        return Ok(ProbeResult {
            seekability: Seekability::NotSeekable {
                reason: NotSeekableReason::HttpRangeStatus {
                    status: response.status().as_u16(),
                },
            },
            validators,
            content_length: response.content_length().or_else(|| {
                response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
            }),
        });
    }

    Err(SourceError::HttpStatus {
        operation: "range-probe",
        url: url.clone(),
        status: response.status(),
        retry_after: crate::HttpRetryAfter::Unavailable,
    })
}

/// Отправляет GET с Range header, сохраняя service-provided headers как данные.
fn send_range_request(
    client: &Client,
    url: &SecretHttpUrl,
    headers: &HeaderMap,
    request_body: Option<&[u8]>,
    range: &ByteRange,
    operation: &'static str,
) -> SourceResult<reqwest::blocking::Response> {
    let mut request_headers = headers.clone();
    request_headers.remove(RANGE);
    request_headers.insert(
        RANGE,
        HeaderValue::from_str(&range.header_value()).map_err(|_| {
            SourceError::InvalidHttpHeaderValue {
                name: RANGE.as_str().to_string(),
            }
        })?,
    );

    let request = match request_body {
        Some(body) => client
            .post(url.expose_secret_for_open())
            .headers(request_headers)
            .body(body.to_vec()),
        None => client
            .get(url.expose_secret_for_open())
            .headers(request_headers),
    };

    request
        .send()
        .map_err(|source| map_reqwest_error(operation, url, source))
}

/// Валидирует, что `Content-Range` соответствует запрошенному range.
pub(crate) fn validate_content_range(
    url: &SecretHttpUrl,
    headers: &HeaderMap,
    range: &ByteRange,
) -> SourceResult<ParsedContentRange> {
    let parsed_range = parse_content_range_header(url, headers)?;
    if parsed_range.start != range.start || parsed_range.end_inclusive != range.end_inclusive() {
        return Err(SourceError::InvalidContentRange {
            url: url.clone(),
            header: "<unexpected range>".to_string(),
        });
    }
    if parsed_range
        .total_length
        .is_some_and(|total_length| total_length <= parsed_range.end_inclusive)
    {
        return Err(SourceError::InvalidContentRange {
            url: url.clone(),
            header: "<inconsistent total length>".to_string(),
        });
    }

    Ok(parsed_range)
}

/// Читает ровно ожидаемое количество bytes из response body в caller buffer.
fn read_response_body_into(
    url: &SecretHttpUrl,
    mut response: reqwest::blocking::Response,
    offset: u64,
    output: &mut [u8],
    cancellation: &CancellationToken,
) -> SourceResult<usize> {
    let expected_length = output.len();
    let mut total_read = 0_usize;

    while total_read < expected_length {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        match response.read(&mut output[total_read..]) {
            Ok(0) => break,
            Ok(bytes_read) => {
                total_read = total_read.saturating_add(bytes_read);
            }
            Err(source) if source.kind() == std::io::ErrorKind::TimedOut => {
                return Err(SourceError::HttpTimeout {
                    operation: "range-read",
                    url: url.clone(),
                });
            }
            Err(source) => {
                return Err(SourceError::HttpBodyRead {
                    operation: "range-read",
                    url: url.clone(),
                    source,
                });
            }
        }
    }

    if total_read != expected_length {
        return Err(SourceError::UnexpectedEof {
            offset,
            expected_bytes: expected_length,
            actual_bytes: total_read,
        });
    }

    Ok(total_read)
}

/// Строит reqwest HeaderMap из внешних headers.
pub(crate) fn build_header_map(headers: &[HttpHeader]) -> SourceResult<HeaderMap> {
    let mut header_map = HeaderMap::new();

    for header in headers {
        let header_name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| {
            SourceError::InvalidHttpHeaderName {
                name: header.name.clone(),
            }
        })?;
        let header_value = HeaderValue::from_str(&header.value).map_err(|_| {
            SourceError::InvalidHttpHeaderValue {
                name: header.name.clone(),
            }
        })?;
        header_map.insert(header_name, header_value);
    }

    Ok(header_map)
}

/// Достаёт validators из HTTP response headers.
pub(crate) fn validators_from_headers(headers: &HeaderMap) -> SourceValidators {
    SourceValidators {
        etag: headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
        last_modified: headers
            .get(LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
    }
}

/// Парсит обязательный для 206 response header `Content-Range`.
fn parse_content_range_header(
    url: &SecretHttpUrl,
    headers: &HeaderMap,
) -> SourceResult<ParsedContentRange> {
    let header = headers
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| SourceError::InvalidContentRange {
            url: url.clone(),
            header: "<missing>".to_string(),
        })?;

    parse_content_range_value(header).ok_or_else(|| SourceError::InvalidContentRange {
        url: url.clone(),
        header: "<invalid>".to_string(),
    })
}

/// Парсит значение вида `bytes 0-99/1000`.
fn parse_content_range_value(header: &str) -> Option<ParsedContentRange> {
    let range_and_total = header.strip_prefix("bytes ")?;
    let (range_part, total_part) = range_and_total.split_once('/')?;
    let (start, end_inclusive) = range_part.split_once('-')?;
    let total_length = if total_part == "*" {
        None
    } else {
        Some(total_part.parse::<u64>().ok()?)
    };

    Some(ParsedContentRange {
        start: start.parse::<u64>().ok()?,
        end_inclusive: end_inclusive.parse::<u64>().ok()?,
        total_length,
    })
}

/// Ограничивает read известной длиной source-а.
fn bounded_read_length(offset: u64, requested_length: usize, content_length: Option<u64>) -> usize {
    let Some(content_length) = content_length else {
        return requested_length;
    };

    if offset >= content_length {
        return 0;
    }

    let remaining = content_length - offset;
    requested_length.min(usize::try_from(remaining).unwrap_or(usize::MAX))
}

/// Нормализует reqwest errors в player/UI-friendly source errors.
pub(crate) fn map_reqwest_error(
    operation: &'static str,
    url: &SecretHttpUrl,
    source: reqwest::Error,
) -> SourceError {
    if source.is_timeout() {
        SourceError::HttpTimeout {
            operation,
            url: url.clone(),
        }
    } else {
        SourceError::HttpRequest {
            operation,
            url: url.clone(),
            source: source.without_url(),
        }
    }
}

/// Строит fingerprint из URL, validators и длины, не добавляя service-specific logic.
fn build_http_fingerprint(
    url: &SecretHttpUrl,
    content_length: Option<u64>,
    validators: &SourceValidators,
) -> SourceFingerprint {
    let identity_hash = url.stable_identity_hash();

    SourceFingerprint::new(format!(
        "http:{identity_hash:016x}:{}:{}:{}",
        content_length
            .map(|length| length.to_string())
            .unwrap_or_else(|| "unknown-length".to_string()),
        validators.etag.as_deref().unwrap_or("no-etag"),
        validators
            .last_modified
            .as_deref()
            .unwrap_or("no-last-modified")
    ))
}

#[cfg(test)]
mod range_redirect_tests;

#[cfg(test)]
mod error_mapping_tests;

#[cfg(test)]
mod tests;
