//! Проверенные HTTP values для service-neutral network policy.
//!
//! Модуль не выполняет запросы и не решает redirect/auth policy. Его задача —
//! один раз разобрать untrusted locator/header values, сохранить exact secret
//! identity для transport-а и наружу отдать только безопасные структурные
//! доказательства: scheme, origin и request path.

use std::fmt;

use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

use crate::{HttpHeader, SecretHttpUrl};

/// Разрешённая схема byte HTTP transport-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpScheme {
    /// Незашифрованный HTTP.
    Http,
    /// HTTP поверх TLS.
    Https,
}

/// Требование transport security для scoped HTTP request material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpScopeSecurity {
    /// Scope, созданный от HTTPS resource-а, запрещает downgrade на HTTP.
    SecureOnly,
    /// Scope, созданный от HTTP resource-а, допускает проверенные HTTP(S) targets.
    ValidatedHttpOrHttps,
}

impl HttpScheme {
    /// Возвращает canonical scheme label без пользовательского payload.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    /// Возвращает effective default port scheme-а.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }

    /// Классифицирует уже распарсенную scheme без alias expansion.
    fn parse(value: &str) -> Result<Self, HttpRequestTargetError> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            _ => Err(HttpRequestTargetError::UnsupportedScheme),
        }
    }
}

/// Нормализованный HTTP security origin: scheme + host + effective port.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HttpOrigin {
    /// Exact admitted scheme family.
    scheme: HttpScheme,
    /// WHATWG-normalized host без userinfo/path/query/fragment.
    host: String,
    /// Explicit либо scheme-default port.
    effective_port: u16,
}

impl HttpOrigin {
    /// Возвращает admitted HTTP scheme.
    #[must_use]
    pub const fn scheme(&self) -> HttpScheme {
        self.scheme
    }

    /// Возвращает normalized host для exact origin comparison.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Возвращает explicit либо scheme-default port.
    #[must_use]
    pub const fn effective_port(&self) -> u16 {
        self.effective_port
    }
}

impl fmt::Debug for HttpOrigin {
    /// Показывает только origin; secret path/query/userinfo сюда не входят.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpOrigin")
            .field("scheme", &self.scheme)
            .field("host", &self.host)
            .field("effective_port", &self.effective_port)
            .finish()
    }
}

/// Exact secret HTTP request target плюс проверенные policy attributes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HttpRequestTarget {
    /// Exact caller serialization для единственной реальной request boundary.
    exact: SecretHttpUrl,
    /// Normalized origin для same-origin checks.
    origin: HttpOrigin,
    /// Normalized absolute path без query/fragment.
    path: String,
}

impl HttpRequestTarget {
    /// Проверяет absolute hierarchical HTTP(S) URL, сохраняя exact input отдельно.
    pub fn parse_exact(exact: impl Into<String>) -> Result<Self, HttpRequestTargetError> {
        let exact = exact.into();
        let parsed = Url::parse(&exact).map_err(|_| HttpRequestTargetError::InvalidSyntax)?;
        let scheme = HttpScheme::parse(parsed.scheme())?;
        if parsed.cannot_be_a_base() {
            return Err(HttpRequestTargetError::NonHierarchical);
        }
        let host = parsed
            .host_str()
            .ok_or(HttpRequestTargetError::MissingHost)?
            .to_owned();
        let effective_port = parsed
            .port_or_known_default()
            .unwrap_or(scheme.default_port());
        let origin = HttpOrigin {
            scheme,
            host,
            effective_port,
        };

        Ok(Self {
            exact: SecretHttpUrl::from_secret_for_open(exact),
            origin,
            path: parsed.path().to_owned(),
        })
    }

    /// Возвращает admitted scheme без раскрытия locator-а.
    #[must_use]
    pub const fn scheme(&self) -> HttpScheme {
        self.origin.scheme()
    }

    /// Возвращает normalized security origin.
    #[must_use]
    pub const fn origin(&self) -> &HttpOrigin {
        &self.origin
    }

    /// Раскрывает exact locator только concrete HTTP request owner-у.
    #[must_use]
    pub fn expose_secret_for_request(&self) -> &str {
        self.exact.expose_secret_for_open()
    }

    /// Возвращает opaque hash exact identity без раскрытия locator-а.
    #[must_use]
    pub fn stable_identity_hash(&self) -> u64 {
        self.exact.stable_identity_hash()
    }

    /// Разрешает URI reference относительно exact effective response target-а.
    pub fn resolve_reference(&self, reference: &str) -> Result<Self, HttpRequestTargetError> {
        let base = Url::parse(self.exact.expose_secret_for_open())
            .map_err(|_| HttpRequestTargetError::InvalidSyntax)?;
        let resolved = base
            .join(reference)
            .map_err(|_| HttpRequestTargetError::InvalidSyntax)?;
        Self::parse_exact(resolved)
    }

    /// Заменяет только query exact target-а scoped secret override-ом.
    pub fn with_query_override(&self, query: &str) -> Result<Self, HttpRequestTargetError> {
        let mut parsed = Url::parse(self.exact.expose_secret_for_open())
            .map_err(|_| HttpRequestTargetError::InvalidSyntax)?;
        parsed.set_query(Some(query));
        Self::parse_exact(parsed)
    }

    /// Сливает extractor query parameters с target query по semantics pinned yt-dlp.
    ///
    /// Existing keys сохраняют порядок, override заменяет все значения совпавшего
    /// key, новые keys добавляются в конце. Пустые значения отбрасываются так же,
    /// как `urllib.parse.parse_qs(..., keep_blank_values=False)`.
    pub fn merge_extractor_query_parameters(
        &self,
        query_parameters: &str,
    ) -> Result<Self, HttpRequestTargetError> {
        let mut parsed = Url::parse(self.exact.expose_secret_for_open())
            .map_err(|_| HttpRequestTargetError::InvalidSyntax)?;
        let mut merged = grouped_non_empty_query_pairs(parsed.query().unwrap_or_default());
        let overrides = grouped_non_empty_query_pairs(query_parameters);
        if overrides.is_empty() {
            return Ok(self.clone());
        }
        for (override_key, override_values) in overrides {
            if let Some((_, existing_values)) = merged
                .iter_mut()
                .find(|(existing_key, _)| existing_key == &override_key)
            {
                *existing_values = override_values;
            } else {
                merged.push((override_key, override_values));
            }
        }
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, values) in merged {
            for value in values {
                serializer.append_pair(&key, &value);
            }
        }
        let merged_query = serializer.finish();
        parsed.set_query((!merged_query.is_empty()).then_some(&merged_query));
        Self::parse_exact(parsed)
    }
}

/// Группирует decoded query pairs, сохраняя first-key и duplicate-value order.
fn grouped_non_empty_query_pairs(query: &str) -> Vec<(String, Vec<String>)> {
    let mut grouped = Vec::<(String, Vec<String>)>::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if value.is_empty() {
            continue;
        }
        if let Some((_, values)) = grouped
            .iter_mut()
            .find(|(existing_key, _)| existing_key == key.as_ref())
        {
            values.push(value.into_owned());
        } else {
            grouped.push((key.into_owned(), vec![value.into_owned()]));
        }
    }
    grouped
}

impl fmt::Debug for HttpRequestTarget {
    /// Не допускает утечку userinfo/path/query/fragment через diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequestTarget")
            .field("origin", &self.origin)
            .field("path", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for HttpRequestTarget {
    /// Display намеренно совпадает с безопасным origin-only представлением.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}://{}:{}<redacted>",
            self.origin.scheme().as_str(),
            self.origin.host(),
            self.origin.effective_port()
        )
    }
}

/// Низкоуровневое доказательство origin/path/secure scope для HTTP material.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HttpRequestScope {
    /// Exact normalized security origin.
    origin: HttpOrigin,
    /// Segment-boundary-aware path subtree.
    path: HttpPathScope,
    /// Запрет HTTPS downgrade либо разрешение validated HTTP(S).
    security: HttpScopeSecurity,
}

impl HttpRequestScope {
    /// Строит scope от уже проверенного initial target-а и explicit path boundary.
    #[must_use]
    pub fn from_target(target: &HttpRequestTarget, path: HttpPathScope) -> Self {
        let security = if target.scheme() == HttpScheme::Https {
            HttpScopeSecurity::SecureOnly
        } else {
            HttpScopeSecurity::ValidatedHttpOrHttps
        };
        Self {
            origin: target.origin().clone(),
            path,
            security,
        }
    }

    /// Проверяет origin, path boundary и HTTPS downgrade до раскрытия material.
    #[must_use]
    pub fn allows(&self, target: &HttpRequestTarget) -> bool {
        let security_allowed = match self.security {
            HttpScopeSecurity::SecureOnly => target.scheme() == HttpScheme::Https,
            HttpScopeSecurity::ValidatedHttpOrHttps => true,
        };
        security_allowed && self.origin == *target.origin() && self.path.allows_target(target)
    }

    /// Возвращает только typed security requirement без locator payload.
    #[must_use]
    pub const fn security(&self) -> HttpScopeSecurity {
        self.security
    }
}

impl fmt::Debug for HttpRequestScope {
    /// Path остаётся redacted через `HttpPathScope::Debug`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequestScope")
            .field("origin", &self.origin)
            .field("path", &self.path)
            .field("security", &self.security)
            .finish()
    }
}

/// Secret-safe ошибка проверки request target-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpRequestTargetError {
    /// Locator не является syntactically valid absolute URL.
    #[error("некорректный absolute HTTP request target")]
    InvalidSyntax,
    /// Scheme не входит в exact HTTP(S) vocabulary.
    #[error("HTTP request target использует неподдерживаемую схему")]
    UnsupportedScheme,
    /// Opaque URL нельзя использовать как hierarchical network target.
    #[error("HTTP request target не является hierarchical URL")]
    NonHierarchical,
    /// Network target обязан иметь host.
    #[error("HTTP request target не содержит host")]
    MissingHost,
}

/// Нормализованный path prefix для secret forwarding.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HttpPathScope(String);

impl HttpPathScope {
    /// Проверяет absolute path без query/fragment.
    pub fn new(path_prefix: impl Into<String>) -> Result<Self, HttpPathScopeError> {
        let path_prefix = path_prefix.into();
        if !path_prefix.starts_with('/') {
            return Err(HttpPathScopeError::NotAbsolute);
        }
        if path_prefix.contains(['?', '#']) {
            return Err(HttpPathScopeError::ContainsQueryOrFragment);
        }
        Ok(Self(path_prefix))
    }

    /// Создаёт scope из уже проверенного request path.
    #[must_use]
    pub fn from_target_path(target: &HttpRequestTarget) -> Self {
        Self(target.path.clone())
    }

    /// Проверяет target path внутри `source-core`, не раскрывая path caller-у.
    #[must_use]
    pub fn allows_target(&self, target: &HttpRequestTarget) -> bool {
        self.allows(&target.path)
    }

    /// Проверяет path-match с boundary между path segments.
    #[must_use]
    fn allows(&self, request_path: &str) -> bool {
        if self.0 == "/" || request_path == self.0 {
            return true;
        }
        if self.0.ends_with('/') {
            return request_path.starts_with(&self.0);
        }
        request_path
            .strip_prefix(&self.0)
            .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

impl fmt::Debug for HttpPathScope {
    /// Path scope может содержать media identity, поэтому скрывается целиком.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpPathScope(<redacted>)")
    }
}

/// Ошибка построения HTTP path scope без отражения исходного path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpPathScopeError {
    /// Scope обязан начинаться с `/`.
    #[error("HTTP path scope должен быть absolute path")]
    NotAbsolute,
    /// Query и fragment не являются частью path scope.
    #[error("HTTP path scope не может содержать query или fragment")]
    ContainsQueryOrFragment,
}

/// Проверенный owned набор headers с secret-safe formatting.
#[derive(Clone, PartialEq, Eq)]
pub struct ValidatedHttpHeaders(Box<[HttpHeader]>);

impl ValidatedHttpHeaders {
    /// Проверяет имена и значения через тот же HTTP stack, который выполнит запрос.
    pub fn new(headers: Vec<HttpHeader>) -> Result<Self, HttpHeaderValidationError> {
        for header in &headers {
            HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|_| HttpHeaderValidationError::InvalidName)?;
            HeaderValue::from_str(&header.value)
                .map_err(|_| HttpHeaderValidationError::InvalidValue)?;
        }
        Ok(Self(headers.into_boxed_slice()))
    }

    /// Возвращает число headers без раскрытия значений.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Проверяет отсутствие headers.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Раскрывает validated headers только concrete request owner-у.
    #[must_use]
    pub fn expose_for_request(&self) -> &[HttpHeader] {
        &self.0
    }
}

impl fmt::Debug for ValidatedHttpHeaders {
    /// Diagnostics содержит только количество, но не names/values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedHttpHeaders")
            .field("count", &self.len())
            .finish()
    }
}

/// Secret-safe ошибка header validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpHeaderValidationError {
    /// Header name нарушает HTTP grammar.
    #[error("некорректное имя HTTP header")]
    InvalidName,
    /// Header value содержит недопустимые bytes.
    #[error("некорректное значение HTTP header")]
    InvalidValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact locator сохраняется, а policy attributes нормализуются отдельно.
    #[test]
    fn request_target_preserves_exact_secret_and_normalizes_origin() {
        let exact = "HTTPS://user:secret@Example.COM:443/private/video?q=token#fragment";
        let target = HttpRequestTarget::parse_exact(exact).expect("valid HTTPS target");

        assert_eq!(target.scheme(), HttpScheme::Https);
        assert_eq!(target.origin().host(), "example.com");
        assert_eq!(target.origin().effective_port(), 443);
        assert_eq!(target.path, "/private/video");
        assert_eq!(target.expose_secret_for_request(), exact);

        let formatted = format!("{target:?} {target}");
        assert!(!formatted.contains("user"));
        assert!(!formatted.contains("secret"));
        assert!(!formatted.contains("private"));
        assert!(!formatted.contains("token"));
    }

    #[test]
    fn extractor_query_merge_replaces_keys_and_preserves_duplicates_and_encoding() {
        let target = HttpRequestTarget::parse_exact(
            "https://media.example.invalid/seg.ts?a=old&a=older&keep=one+two&blank=#frag",
        )
        .expect("valid target");

        let merged = target
            .merge_extractor_query_parameters("a=new%20value&a=second&added=%2Fpath&empty=")
            .expect("valid merged target");

        assert_eq!(
            merged.expose_secret_for_request(),
            "https://media.example.invalid/seg.ts?a=new+value&a=second&keep=one+two&added=%2Fpath#frag"
        );
    }

    #[test]
    fn extractor_query_merge_does_not_change_existing_replacement_api() {
        let target =
            HttpRequestTarget::parse_exact("https://example.invalid/s?a=1").expect("valid target");

        assert_eq!(
            target
                .with_query_override("b=2")
                .expect("replacement")
                .expose_secret_for_request(),
            "https://example.invalid/s?b=2"
        );
        assert_eq!(
            target
                .merge_extractor_query_parameters("b=2")
                .expect("merge")
                .expose_secret_for_request(),
            "https://example.invalid/s?a=1&b=2"
        );
        assert_eq!(
            target
                .merge_extractor_query_parameters("ignored=&flag")
                .expect("empty parsed override is exact no-op")
                .expose_secret_for_request(),
            "https://example.invalid/s?a=1"
        );
    }

    /// Scheme/host failures не отражают untrusted locator в error text.
    #[test]
    fn target_errors_are_typed_and_secret_safe() {
        let secret = "ftp://user:password@example.test/private?token=secret";
        let error = HttpRequestTarget::parse_exact(secret).expect_err("FTP is unsupported");

        assert_eq!(error, HttpRequestTargetError::UnsupportedScheme);
        let formatted = format!("{error:?} {error}");
        assert!(!formatted.contains("password"));
        assert!(!formatted.contains("private"));
        assert!(!formatted.contains("token"));
    }

    /// Prefix совпадает только с тем же path segment subtree.
    #[test]
    fn path_scope_requires_segment_boundary() {
        let scope = HttpPathScope::new("/media/private").expect("valid path scope");

        assert!(scope.allows("/media/private"));
        assert!(scope.allows("/media/private/segment.ts"));
        assert!(!scope.allows("/media/private-copy/segment.ts"));
        assert!(!scope.allows("/other/segment.ts"));
    }

    /// Header validator не печатает secret values даже при ошибке.
    #[test]
    fn validated_headers_redact_values_and_validation_errors() {
        let headers =
            ValidatedHttpHeaders::new(vec![HttpHeader::new("Authorization", "Bearer top-secret")])
                .expect("valid authorization header");
        assert_eq!(headers.len(), 1);
        assert!(!format!("{headers:?}").contains("top-secret"));

        let error = ValidatedHttpHeaders::new(vec![HttpHeader::new(
            "Authorization",
            "Bearer top-secret\ninvalid",
        )])
        .expect_err("newline is not a valid header value");
        assert_eq!(error, HttpHeaderValidationError::InvalidValue);
        assert!(!format!("{error:?} {error}").contains("top-secret"));
    }
}
