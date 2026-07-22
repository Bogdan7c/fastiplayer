//! Один concrete HTTP session для progressive Range/non-Range открытия.
//!
//! Модуль намеренно выполняет только один redirect hop за вызов. Решение,
//! разрешён ли следующий target и какие секреты можно переслать, остаётся у
//! transport provider-а. Автоматические redirect-ы reqwest здесь выключены.

use std::fmt;
use std::io::Read;
use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, LOCATION, RANGE};
use reqwest::redirect::Policy;

use crate::http::{build_header_map, map_reqwest_error};
use crate::{
    CancellationToken, HttpHeader, HttpRangeSource, HttpRequestTarget, ScopedHttpCookieJar,
    SecretHttpUrl, SourceError, SourceResult, SourceRuntimeConfig, StreamingByteSource,
};

/// Семантика request body после конкретного HTTP redirect status-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRedirectRequestBehavior {
    /// `307`/`308` сохраняют исходный метод и body.
    PreserveMethodAndBody,
    /// `301`/`302`/`303` продолжаются безопасным `GET` без исходного body.
    SwitchToGetWithoutBody,
}

/// Явная форма request body без неоднозначного `Option<Vec<u8>>` на boundary.
pub enum HttpRequestBody {
    /// Hop выполняется методом `GET` без body.
    Absent,
    /// Hop выполняется методом `POST` с exact ephemeral body.
    Bytes(Vec<u8>),
}

impl HttpRequestBody {
    /// Возвращает body для построения request-а, если он присутствует.
    #[must_use]
    const fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Absent => None,
            Self::Bytes(bytes) => Some(bytes.as_slice()),
        }
    }

    /// Передаёт body seekable source-у для последующих Range request-ов.
    #[must_use]
    pub(crate) fn into_bytes(self) -> Option<Vec<u8>> {
        match self {
            Self::Absent => None,
            Self::Bytes(bytes) => Some(bytes),
        }
    }

    /// Сообщает diagnostics только факт наличия body, не раскрывая payload.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Bytes(_))
    }
}

impl fmt::Debug for HttpRequestBody {
    /// Никогда не форматирует secret request payload.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("Absent"),
            Self::Bytes(_) => formatter.write_str("Bytes(<redacted>)"),
        }
    }
}

/// Проверенный следующий redirect hop без публикации `Location` в diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRedirectHop {
    /// Разрешённый absolute HTTP(S) target следующего hop-а.
    target: HttpRequestTarget,
    /// Требуемая стандартом семантика method/body.
    request_behavior: HttpRedirectRequestBehavior,
}

impl HttpRedirectHop {
    /// Возвращает следующий exact target transport provider-у.
    #[must_use]
    pub const fn target(&self) -> &HttpRequestTarget {
        &self.target
    }

    /// Возвращает method/body policy для следующего hop-а.
    #[must_use]
    pub const fn request_behavior(&self) -> HttpRedirectRequestBehavior {
        self.request_behavior
    }
}

/// Безопасная категория отказа read-time redirect policy без secret payload-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRangeRedirectRejection {
    /// Redirect нарушает bounded origin/secure/hop policy.
    PolicyRejected,
    /// Новый target не входит в разрешённую область transient secret material.
    SecretScopeRejected,
    /// Scoped headers/body не удалось безопасно выразить как HTTP request.
    RequestMaterialRejected,
}

/// Число уже завершённых redirect hop-ов внутри одного логического Range read-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HttpRangeRedirectHopCount(u8);

impl HttpRangeRedirectHopCount {
    /// Начинает новую независимую redirect chain.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Возвращает точное число завершённых hop-ов policy owner-у.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    /// Продвигает bounded counter после успешно авторизованного hop-а.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

/// Может ли следующий hop сохранить фактическое body текущего Range request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRangeRedirectBodyForwarding {
    /// Сохранить текущее body, только если HTTP redirect semantics тоже разрешает это.
    PreserveCurrent,
    /// Принудительно удалить body из следующего hop-а.
    Drop,
}

impl fmt::Display for HttpRangeRedirectRejection {
    /// Форматирует только стабильную категорию, не target и не secret material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::PolicyRejected => "redirect-policy-rejected",
            Self::SecretScopeRejected => "secret-scope-rejected",
            Self::RequestMaterialRejected => "request-material-rejected",
        };
        formatter.write_str(reason)
    }
}

/// Transport-owned policy hook для redirect-а, возникшего после seekable open.
///
/// `source-core` владеет HTTP Range механикой, но намеренно не знает service
/// secret scopes. Реализация обязана вернуть уже авторизованный и очищенный
/// material следующего hop-а либо typed safe rejection.
pub trait HttpRangeRedirectHandler: Send {
    /// Начинает независимую redirect chain одного логического Range read-а.
    ///
    /// Source вызывает boundary заново и перед retry, поэтому transport обязан
    /// сбросить sticky forwarding state к состоянию stable base request-а.
    fn begin_range_request(&mut self);

    /// Авторизует один redirect и строит material следующего exact hop-а.
    fn material_for_redirect(
        &mut self,
        current_target: &HttpRequestTarget,
        redirect: &HttpRedirectHop,
        completed_hops: HttpRangeRedirectHopCount,
    ) -> Result<HttpRangeRedirectRequestMaterial, HttpRangeRedirectRejection>;
}

/// Scope-filtered material следующего read-time redirect hop-а.
///
/// Target намеренно отсутствует: `source-core` уже разобрал `Location` и сам
/// использует `HttpRedirectHop::target()`, поэтому policy owner не может
/// случайно вернуть material для другого адреса.
pub struct HttpRangeRedirectRequestMaterial {
    /// Уже scope-filtered transport headers.
    headers: Vec<HttpHeader>,
    /// Least-authority разрешение сохранить уже существующее request body.
    body_forwarding: HttpRangeRedirectBodyForwarding,
}

impl HttpRangeRedirectRequestMaterial {
    /// Создаёт material только после transport-owned policy проверки.
    #[must_use]
    pub fn new(headers: Vec<HttpHeader>, body_forwarding: HttpRangeRedirectBodyForwarding) -> Self {
        Self {
            headers,
            body_forwarding,
        }
    }

    /// Передаёт redacted material Range source-у.
    #[must_use]
    pub(crate) fn into_parts(self) -> (Vec<HttpHeader>, HttpRangeRedirectBodyForwarding) {
        (self.headers, self.body_forwarding)
    }
}

impl fmt::Debug for HttpRangeRedirectRequestMaterial {
    /// Показывает только форму material-а без header/body values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRangeRedirectRequestMaterial")
            .field("header_count", &self.headers.len())
            .field("body_forwarding", &self.body_forwarding)
            .finish()
    }
}

impl fmt::Debug for HttpRedirectHop {
    /// Форматирует только redacted target и typed behavior.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRedirectHop")
            .field("target", &self.target)
            .field("request_behavior", &self.request_behavior)
            .finish()
    }
}

/// Результат одного HTTP hop-а до transport-level redirect решения.
pub enum HttpSourceHop {
    /// Сервер потребовал ещё один policy-controlled redirect hop.
    Redirect(HttpRedirectHop),
    /// Сервер доказал byte seek через корректный `206 Content-Range`.
    Seekable(HttpRangeSource),
    /// Сервер вернул полный `200` response body для forward-only чтения.
    Streaming(HttpStreamingSource),
}

impl fmt::Debug for HttpSourceHop {
    /// Не раскрывает response headers, body либо operational target.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect(hop) => formatter.debug_tuple("Redirect").field(hop).finish(),
            Self::Seekable(_) => formatter.write_str("Seekable(<redacted>)"),
            Self::Streaming(_) => formatter.write_str("Streaming(<redacted>)"),
        }
    }
}

/// Owned single-hop request с redacted diagnostics.
pub struct HttpSingleHopRequest {
    /// Exact target текущего hop-а.
    target: HttpRequestTarget,
    /// Уже scope-filtered transport headers.
    headers: Vec<HttpHeader>,
    /// Typed ephemeral request body.
    request_body: HttpRequestBody,
}

impl HttpSingleHopRequest {
    /// Создаёт request только из material, уже разрешённого transport policy.
    #[must_use]
    pub fn new(
        target: HttpRequestTarget,
        headers: Vec<HttpHeader>,
        request_body: HttpRequestBody,
    ) -> Self {
        Self {
            target,
            headers,
            request_body,
        }
    }
}

impl fmt::Debug for HttpSingleHopRequest {
    /// Показывает форму request-а без header/body values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpSingleHopRequest")
            .field("target", &self.target)
            .field("header_count", &self.headers.len())
            .field("has_request_body", &self.request_body.is_present())
            .finish()
    }
}

/// Единственный reqwest client, переиспользуемый всеми hop/range request-ами source-а.
#[derive(Clone)]
pub struct HttpSourceSession {
    /// Blocking client с caller-owned timeout policy и выключенными redirect-ами.
    client: Client,
}

impl HttpSourceSession {
    /// Создаёт session без cookie state до первого network side effect-а.
    pub fn new(source_config: &SourceRuntimeConfig) -> SourceResult<Self> {
        Self::build(source_config, None)
    }

    /// Создаёт session с caller-scoped ephemeral cookie jar.
    pub fn new_with_cookie_jar(
        source_config: &SourceRuntimeConfig,
        cookie_jar: Arc<ScopedHttpCookieJar>,
    ) -> SourceResult<Self> {
        Self::build(source_config, Some(cookie_jar))
    }

    /// Собирает один blocking client; jar никогда не разделяется между sources неявно.
    fn build(
        source_config: &SourceRuntimeConfig,
        cookie_jar: Option<Arc<ScopedHttpCookieJar>>,
    ) -> SourceResult<Self> {
        let mut client_builder = Client::builder()
            .connect_timeout(source_config.connect_timeout())
            .timeout(source_config.read_timeout())
            .redirect(Policy::none());
        if let Some(cookie_jar) = cookie_jar {
            client_builder = client_builder.cookie_provider(cookie_jar);
        }
        let client = client_builder
            .build()
            .map_err(|source| SourceError::HttpClientBuild { source })?;
        Ok(Self { client })
    }

    /// Выполняет ровно один `Range: bytes=0-0` hop и сохраняет его response.
    pub fn open_single_hop(
        &self,
        request: HttpSingleHopRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<HttpSourceHop> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let secret_url = SecretHttpUrl::from_secret_for_open(
            request.target.expose_secret_for_request().to_owned(),
        );
        let mut request_headers = build_header_map(&request.headers)?;
        request_headers.remove(RANGE);
        request_headers.insert(RANGE, HeaderValue::from_static("bytes=0-0"));

        let request_builder = match request.request_body.as_bytes() {
            Some(request_body) => self
                .client
                .post(request.target.expose_secret_for_request())
                .headers(request_headers)
                .body(request_body.to_vec()),
            None => self
                .client
                .get(request.target.expose_secret_for_request())
                .headers(request_headers),
        };
        let response = request_builder
            .send()
            .map_err(|source| map_reqwest_error("progressive-open", &secret_url, source))?;

        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        match response.status() {
            StatusCode::OK => Ok(HttpSourceHop::Streaming(HttpStreamingSource::new(
                response, secret_url,
            ))),
            StatusCode::PARTIAL_CONTENT => {
                let source = HttpRangeSource::from_partial_content_probe(
                    self.client.clone(),
                    request.target,
                    build_header_map(&request.headers)?,
                    request.request_body.into_bytes(),
                    response.headers(),
                )?;
                Ok(HttpSourceHop::Seekable(source))
            }
            status if status.is_redirection() => {
                parse_redirect_hop(&request.target, &secret_url, status, &response)
                    .map(HttpSourceHop::Redirect)
            }
            status => Err(SourceError::HttpStatus {
                operation: "progressive-open",
                url: secret_url,
                status,
            }),
        }
    }
}

impl fmt::Debug for HttpSourceSession {
    /// Session diagnostics не раскрывает client internals либо request material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpSourceSession(<redacted>)")
    }
}

/// Forward-only body исходного `200` response-а без второго HTTP request-а.
pub struct HttpStreamingSource {
    /// Оригинальный response body, полученный Range probe request-ом.
    response: Response,
    /// Redacted locator для typed source errors.
    url: SecretHttpUrl,
}

impl HttpStreamingSource {
    /// Сохраняет уже открытый response без повторного GET.
    fn new(response: Response, url: SecretHttpUrl) -> Self {
        Self { response, url }
    }
}

impl fmt::Debug for HttpStreamingSource {
    /// Не форматирует response headers/body/final raw URL.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpStreamingSource")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl StreamingByteSource for HttpStreamingSource {
    /// Читает body на demux worker-е и проверяет cancellation на каждой границе.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let bytes_read = self.response.read(output).map_err(|source| {
            if source.kind() == std::io::ErrorKind::TimedOut {
                SourceError::HttpTimeout {
                    operation: "progressive-read",
                    url: self.url.clone(),
                }
            } else {
                SourceError::HttpBodyRead {
                    operation: "progressive-read",
                    url: self.url.clone(),
                    source,
                }
            }
        })?;

        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        Ok(bytes_read)
    }
}

/// Разрешает `Location` относительно текущего target-а без отражения payload в error-е.
pub(crate) fn parse_redirect_hop(
    current_target: &HttpRequestTarget,
    current_url: &SecretHttpUrl,
    status: StatusCode,
    response: &Response,
) -> SourceResult<HttpRedirectHop> {
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| SourceError::InvalidHttpRedirect {
            url: current_url.clone(),
            reason: "missing-or-invalid-location",
        })?;
    let base_url = url::Url::parse(current_target.expose_secret_for_request()).map_err(|_| {
        SourceError::InvalidHttpRedirect {
            url: current_url.clone(),
            reason: "invalid-current-target",
        }
    })?;
    let resolved_target =
        base_url
            .join(location)
            .map_err(|_| SourceError::InvalidHttpRedirect {
                url: current_url.clone(),
                reason: "invalid-location",
            })?;
    let target = HttpRequestTarget::parse_exact(resolved_target.as_str()).map_err(|_| {
        SourceError::InvalidHttpRedirect {
            url: current_url.clone(),
            reason: "unsupported-location-target",
        }
    })?;
    let request_behavior = match status {
        StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
            HttpRedirectRequestBehavior::PreserveMethodAndBody
        }
        _ => HttpRedirectRequestBehavior::SwitchToGetWithoutBody,
    };

    Ok(HttpRedirectHop {
        target,
        request_behavior,
    })
}
