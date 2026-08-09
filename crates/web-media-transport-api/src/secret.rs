//! Ephemeral origin/path/secure-scoped request material.

use std::fmt;

use source_core::{
    HttpCookieSeed, HttpHeader, HttpHeaderValidationError, HttpPathScope, HttpRequestScope,
    HttpRequestTarget, HttpScopeSecurity, ValidatedHttpHeaders,
};

/// Требование к transport security при forwarding secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretForwardingRequirement {
    /// Secret разрешён только HTTPS target-у.
    SecureOnly,
    /// Secret разрешён проверенному HTTP либо HTTPS target-у.
    ValidatedHttpOrHttps,
}

/// Exact request purpose для purpose-scoped payload/overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRequestPurpose {
    /// Первичный component resource.
    PrimaryResource,
    /// Manifest resource.
    Manifest,
    /// Media segment/fragment.
    MediaSegment,
    /// Encryption key resource.
    EncryptionKey,
}

/// Origin + path + secure scope одного ephemeral context-а.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretRequestScope {
    /// Shared low-level proof для secret forwarding и ephemeral cookie jar.
    request_scope: HttpRequestScope,
}

impl SecretRequestScope {
    /// Строит safe scope от проверенного initial target-а.
    #[must_use]
    pub fn from_target(target: &HttpRequestTarget, path: HttpPathScope) -> Self {
        Self {
            request_scope: HttpRequestScope::from_target(target, path),
        }
    }

    /// Проверяет все три scope dimensions до раскрытия material.
    #[must_use]
    pub fn allows(&self, target: &HttpRequestTarget) -> bool {
        self.request_scope.allows(target)
    }

    /// Возвращает secure requirement для provider diagnostics без secrets.
    #[must_use]
    pub const fn secure_requirement(&self) -> SecretForwardingRequirement {
        match self.request_scope.security() {
            HttpScopeSecurity::SecureOnly => SecretForwardingRequirement::SecureOnly,
            HttpScopeSecurity::ValidatedHttpOrHttps => {
                SecretForwardingRequirement::ValidatedHttpOrHttps
            }
        }
    }

    /// Клонирует тот же proof для concrete per-source cookie jar.
    #[must_use]
    pub fn request_scope_proof(&self) -> HttpRequestScope {
        self.request_scope.clone()
    }
}

impl fmt::Debug for SecretRequestScope {
    /// Path скрывается своим Debug; origin не содержит userinfo/path/query.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretRequestScope")
            .field("request_scope", &self.request_scope)
            .finish()
    }
}

/// Exact serialized query addition без ведущего `?`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretQueryOverride(String);

impl SecretQueryOverride {
    /// Проверяет, что override не меняет URL structure вне query.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretQueryOverrideError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretQueryOverrideError::Empty);
        }
        if value.starts_with('?') || value.contains('#') {
            return Err(SecretQueryOverrideError::InvalidStructure);
        }
        Ok(Self(value))
    }

    /// Раскрывает exact override только concrete URL request builder-у.
    #[must_use]
    pub fn expose_secret_for_request(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretQueryOverride {
    /// Query может содержать token/signature и всегда скрывается.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretQueryOverride(<redacted>)")
    }
}

/// Secret-safe query override validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretQueryOverrideError {
    /// Пустой override не несёт intent и должен быть `None`.
    #[error("secret query override пуст")]
    Empty,
    /// Override пытается включить query marker либо fragment.
    #[error("secret query override имеет некорректную структуру")]
    InvalidStructure,
}

/// Owned secret bytes с намеренным request-only accessor-ом.
#[derive(Clone, PartialEq, Eq)]
struct SecretPayload(Vec<u8>);

impl SecretPayload {
    /// Копирует caller-owned ephemeral bytes.
    fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Раскрывает bytes только после успешной scope проверки.
    fn expose_for_request(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretPayload {
    /// Ни payload, ни его потенциально identifying length не отражаются.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretPayload(<redacted>)")
    }
}

/// Ephemeral scoped headers/cookies/body/query overrides одного source component-а.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretRequestContext {
    /// Scope proof, обязательный для каждого material access.
    scope: SecretRequestScope,
    /// Проверенные serialized headers.
    headers: ValidatedHttpHeaders,
    /// Уже готовый serialized request Cookie header value.
    cookie_header: Option<SecretPayload>,
    /// Response-style cookies с отдельными Domain/Path/Secure/expiry scopes.
    cookie_seeds: Box<[HttpCookieSeed]>,
    /// Serialized request body только initial component request-а.
    request_data: Option<SecretPayload>,
    /// Query addition только media segment URL.
    segment_query: Option<SecretQueryOverride>,
    /// Query addition только encryption key URL.
    key_query: Option<SecretQueryOverride>,
}

impl SecretRequestContext {
    /// Пустой context для non-HTTP transport-ов (FTP).
    ///
    /// `material_for` всегда вернёт `None`, потому что placeholder scope не
    /// совпадает ни с одним real HTTP target; `is_empty()` == true.
    #[must_use]
    pub fn empty() -> Self {
        let placeholder = HttpRequestTarget::parse_exact("https://invalid.invalid/")
            .expect("static placeholder HTTP target");
        let path = HttpPathScope::from_target_path(&placeholder);
        Self::builder(SecretRequestScope::from_target(&placeholder, path)).build()
    }

    /// Начинает named builder, чтобы секретные поля не передавались позиционно.
    #[must_use]
    pub fn builder(scope: SecretRequestScope) -> SecretRequestContextBuilder {
        SecretRequestContextBuilder {
            scope,
            headers: ValidatedHttpHeaders::new(Vec::new()).expect("empty HTTP header set is valid"),
            cookie_header: None,
            cookie_seeds: Box::new([]),
            request_data: None,
            segment_query: None,
            key_query: None,
        }
    }

    /// Проверяет, есть ли material, требующий initial target scope proof.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
            && self.cookie_header.is_none()
            && self.cookie_seeds.is_empty()
            && self.request_data.is_none()
            && self.segment_query.is_none()
            && self.key_query.is_none()
    }

    /// Возвращает scoped view либо `None`, не раскрывая причину несовпадения.
    #[must_use]
    pub fn material_for(
        &self,
        target: &HttpRequestTarget,
        purpose: SecretRequestPurpose,
    ) -> Option<ScopedSecretRequestMaterial<'_>> {
        if !self.scope.allows(target) {
            return None;
        }
        let request_data = (purpose == SecretRequestPurpose::PrimaryResource)
            .then_some(self.request_data.as_ref())
            .flatten();
        let query_override = match purpose {
            SecretRequestPurpose::MediaSegment => self.segment_query.as_ref(),
            SecretRequestPurpose::EncryptionKey => self.key_query.as_ref(),
            SecretRequestPurpose::PrimaryResource | SecretRequestPurpose::Manifest => None,
        };
        Some(ScopedSecretRequestMaterial {
            headers: &self.headers,
            cookie_header: self.cookie_header.as_ref(),
            cookie_seeds: &self.cookie_seeds,
            request_data,
            query_override,
        })
    }

    /// Возвращает scope только для policy composition.
    #[must_use]
    pub const fn scope(&self) -> &SecretRequestScope {
        &self.scope
    }
}

impl fmt::Debug for SecretRequestContext {
    /// Показывает лишь структурное наличие material, но никогда payload.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretRequestContext")
            .field("scope", &self.scope)
            .field("header_count", &self.headers.len())
            .field("has_cookie_header", &self.cookie_header.is_some())
            .field("cookie_seed_count", &self.cookie_seeds.len())
            .field("has_request_data", &self.request_data.is_some())
            .field("has_segment_query", &self.segment_query.is_some())
            .field("has_key_query", &self.key_query.is_some())
            .finish()
    }
}

/// Named builder ephemeral secret context-а.
pub struct SecretRequestContextBuilder {
    /// Scope proof.
    scope: SecretRequestScope,
    /// Validated headers.
    headers: ValidatedHttpHeaders,
    /// Уже готовый serialized request Cookie header.
    cookie_header: Option<SecretPayload>,
    /// Typed response-style cookie seeds.
    cookie_seeds: Box<[HttpCookieSeed]>,
    /// Serialized request body.
    request_data: Option<SecretPayload>,
    /// Segment query addition.
    segment_query: Option<SecretQueryOverride>,
    /// Key query addition.
    key_query: Option<SecretQueryOverride>,
}

impl SecretRequestContextBuilder {
    /// Устанавливает уже проверенный набор headers.
    #[must_use]
    pub fn with_headers(mut self, headers: ValidatedHttpHeaders) -> Self {
        self.headers = headers;
        self
    }

    /// Проверяет serialized cookies как HTTP Cookie header value.
    pub fn with_serialized_cookies(
        mut self,
        cookies: impl Into<String>,
    ) -> Result<Self, HttpHeaderValidationError> {
        let cookies = cookies.into();
        ValidatedHttpHeaders::new(vec![HttpHeader::new("Cookie", cookies.clone())])?;
        self.cookie_header = Some(SecretPayload::new(cookies.into_bytes()));
        Ok(self)
    }

    /// Устанавливает уже проверенные scoped cookie seeds без flattening scope attributes.
    #[must_use]
    pub fn with_scoped_cookie_seeds(
        mut self,
        cookie_seeds: impl IntoIterator<Item = HttpCookieSeed>,
    ) -> Self {
        self.cookie_seeds = cookie_seeds
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self
    }

    /// Устанавливает opaque serialized request body.
    #[must_use]
    pub fn with_request_data(mut self, request_data: impl Into<Vec<u8>>) -> Self {
        self.request_data = Some(SecretPayload::new(request_data));
        self
    }

    /// Устанавливает media-segment-only query override.
    #[must_use]
    pub fn with_segment_query_override(mut self, override_value: SecretQueryOverride) -> Self {
        self.segment_query = Some(override_value);
        self
    }

    /// Устанавливает encryption-key-only query override.
    #[must_use]
    pub fn with_key_query_override(mut self, override_value: SecretQueryOverride) -> Self {
        self.key_query = Some(override_value);
        self
    }

    /// Завершает immutable ephemeral context.
    #[must_use]
    pub fn build(self) -> SecretRequestContext {
        SecretRequestContext {
            scope: self.scope,
            headers: self.headers,
            cookie_header: self.cookie_header,
            cookie_seeds: self.cookie_seeds,
            request_data: self.request_data,
            segment_query: self.segment_query,
            key_query: self.key_query,
        }
    }
}

/// Borrowed material, полученный только после scope validation.
pub struct ScopedSecretRequestMaterial<'a> {
    /// Validated headers.
    headers: &'a ValidatedHttpHeaders,
    /// Optional serialized request Cookie header.
    cookie_header: Option<&'a SecretPayload>,
    /// Scoped response-style cookie seeds.
    cookie_seeds: &'a [HttpCookieSeed],
    /// Initial-resource-only request body.
    request_data: Option<&'a SecretPayload>,
    /// Purpose-selected segment/key query addition.
    query_override: Option<&'a SecretQueryOverride>,
}

impl ScopedSecretRequestMaterial<'_> {
    /// Раскрывает headers concrete HTTP request builder-у.
    #[must_use]
    pub fn headers_for_request(&self) -> &[HttpHeader] {
        self.headers.expose_for_request()
    }

    /// Раскрывает serialized Cookie value concrete request builder-у.
    #[must_use]
    pub fn cookies_for_request(&self) -> Option<&[u8]> {
        self.cookie_header.map(SecretPayload::expose_for_request)
    }

    /// Раскрывает scoped cookie seeds только concrete per-source jar-у.
    #[must_use]
    pub const fn cookie_seeds_for_request(&self) -> &[HttpCookieSeed] {
        self.cookie_seeds
    }

    /// Раскрывает request body только primary-resource request-у.
    #[must_use]
    pub fn request_data_for_request(&self) -> Option<&[u8]> {
        self.request_data.map(SecretPayload::expose_for_request)
    }

    /// Раскрывает purpose-selected query override.
    #[must_use]
    pub const fn query_override_for_request(&self) -> Option<&SecretQueryOverride> {
        self.query_override
    }
}

impl fmt::Debug for ScopedSecretRequestMaterial<'_> {
    /// Scoped view также не отражает payload после успешной проверки.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedSecretRequestMaterial")
            .field("header_count", &self.headers.len())
            .field("has_cookie_header", &self.cookie_header.is_some())
            .field("cookie_seed_count", &self.cookie_seeds.len())
            .field("has_request_data", &self.request_data.is_some())
            .field("has_query_override", &self.query_override.is_some())
            .finish()
    }
}
