//! Проверенный initial cookie, который можно безопасно передать RFC-aware jar-у.
//!
//! Модуль отделяет response-style cookie с `Domain`/`Path`/`Secure`/`Expires`
//! от готового request `Cookie` header-а. Это различие важно: атрибуты области
//! действия нельзя отправлять серверу как самостоятельные cookie pairs.

use std::fmt;

use cookie::{Cookie, time::OffsetDateTime};
use reqwest::header::HeaderValue;
use url::Host;

/// Один проверенный Set-Cookie seed без публичного доступа к secret value.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpCookieSeed {
    /// RFC-compatible Set-Cookie value для единственного initial target-а.
    serialized_set_cookie: HeaderValue,
}

impl HttpCookieSeed {
    /// Начинает named builder и проверяет cookie name/value до добавления scope.
    pub fn builder(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<HttpCookieSeedBuilder, HttpCookieSeedError> {
        let name = name.into();
        let value = value.into();
        if !is_http_token(&name) {
            return Err(HttpCookieSeedError::InvalidName);
        }

        // `Cookie::parse` не должен принять скрытый `; attribute` как часть value.
        let serialized_pair = format!("{name}={value}");
        let parsed =
            Cookie::parse(serialized_pair).map_err(|_| HttpCookieSeedError::InvalidValue)?;
        if parsed.name() != name || parsed.value() != value {
            return Err(HttpCookieSeedError::InvalidValue);
        }

        Ok(HttpCookieSeedBuilder {
            name,
            value,
            domain: None,
            path: None,
            secure_only: false,
            expires_at: None,
        })
    }

    /// Раскрывает Set-Cookie только jar-у внутри `source-core`.
    pub(crate) fn expose_set_cookie_for_jar(&self) -> &str {
        self.serialized_set_cookie
            .to_str()
            .expect("HttpCookieSeed всегда создаётся из ASCII-compatible HeaderValue")
    }
}

impl fmt::Debug for HttpCookieSeed {
    /// Cookie name, value и scope могут идентифицировать сессию и не логируются.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpCookieSeed(<redacted>)")
    }
}

/// Named builder одного response-style cookie seed-а.
pub struct HttpCookieSeedBuilder {
    /// Проверенное HTTP-token имя.
    name: String,
    /// Проверенное serialized value без скрытых attributes.
    value: String,
    /// Обязательный upstream domain scope.
    domain: Option<String>,
    /// Optional RFC path scope.
    path: Option<String>,
    /// Разрешён ли cookie только HTTPS request-ам.
    secure_only: bool,
    /// Optional абсолютное время истечения.
    expires_at: Option<OffsetDateTime>,
}

impl HttpCookieSeedBuilder {
    /// Устанавливает обязательный DNS/IP domain scope.
    pub fn for_domain(mut self, domain: impl Into<String>) -> Result<Self, HttpCookieSeedError> {
        let domain = domain.into();
        let normalized_domain = domain.strip_prefix('.').unwrap_or(&domain);
        if normalized_domain.is_empty() || Host::parse(normalized_domain).is_err() {
            return Err(HttpCookieSeedError::InvalidDomain);
        }
        self.domain = Some(domain);
        Ok(self)
    }

    /// Устанавливает RFC path scope; пустой path обязан оставаться отсутствующим.
    pub fn with_path(mut self, path: impl Into<String>) -> Result<Self, HttpCookieSeedError> {
        let path = path.into();
        if !path.starts_with('/') || HeaderValue::from_str(&path).is_err() {
            return Err(HttpCookieSeedError::InvalidPath);
        }
        self.path = Some(path);
        Ok(self)
    }

    /// Помечает cookie как доступный только HTTPS request-ам.
    #[must_use]
    pub fn secure_only(mut self) -> Self {
        self.secure_only = true;
        self
    }

    /// Переводит upstream Unix timestamp в RFC cookie expiration.
    pub fn expires_at_unix_seconds(
        mut self,
        unix_seconds: i64,
    ) -> Result<Self, HttpCookieSeedError> {
        self.expires_at = Some(
            OffsetDateTime::from_unix_timestamp(unix_seconds)
                .map_err(|_| HttpCookieSeedError::InvalidExpiration)?,
        );
        Ok(self)
    }

    /// Завершает immutable seed; domain обязателен для fail-closed scoping.
    pub fn build(self) -> Result<HttpCookieSeed, HttpCookieSeedError> {
        let domain = self.domain.ok_or(HttpCookieSeedError::MissingDomain)?;
        let mut cookie = Cookie::build((self.name, self.value)).domain(domain);
        if let Some(path) = self.path {
            cookie = cookie.path(path);
        }
        if self.secure_only {
            cookie = cookie.secure(true);
        }
        if let Some(expires_at) = self.expires_at {
            cookie = cookie.expires(expires_at);
        }
        let serialized_set_cookie = HeaderValue::from_str(&cookie.build().to_string())
            .map_err(|_| HttpCookieSeedError::InvalidValue)?;
        Ok(HttpCookieSeed {
            serialized_set_cookie,
        })
    }
}

/// Secret-safe причина отказа response-style cookie seed-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpCookieSeedError {
    /// Имя не соответствует HTTP token grammar.
    #[error("cookie name имеет недопустимую форму")]
    InvalidName,
    /// Value нельзя безопасно сериализовать как единственную cookie pair.
    #[error("cookie value имеет недопустимую форму")]
    InvalidValue,
    /// Scoped cookie обязан иметь Domain в pinned yt-dlp representation.
    #[error("scoped cookie не содержит domain")]
    MissingDomain,
    /// Domain не является DNS/IP host-ом.
    #[error("cookie domain имеет недопустимую форму")]
    InvalidDomain,
    /// Path не является абсолютным HTTP path-ом.
    #[error("cookie path имеет недопустимую форму")]
    InvalidPath,
    /// Unix timestamp не представим поддерживаемым временем.
    #[error("cookie expiration находится вне поддерживаемого диапазона")]
    InvalidExpiration,
}

/// Проверяет RFC HTTP token grammar для cookie name.
fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет полную scoped serialization и обязательную redaction.
    #[test]
    fn builder_preserves_scope_without_exposing_secret_in_debug() {
        let seed = HttpCookieSeed::builder("session", "cookie-secret")
            .expect("valid cookie pair")
            .for_domain(".example.test")
            .expect("valid parent domain")
            .with_path("/media")
            .expect("valid cookie path")
            .secure_only()
            .expires_at_unix_seconds(1_900_000_000)
            .expect("valid future timestamp")
            .build()
            .expect("complete scoped seed");

        let serialized = seed.expose_set_cookie_for_jar();
        assert!(serialized.contains("Domain=example.test"));
        assert!(serialized.contains("Path=/media"));
        assert!(serialized.contains("Secure"));
        assert!(serialized.contains("Expires="));
        assert!(!format!("{seed:?}").contains("cookie-secret"));
    }

    /// Проверяет fail-closed обязательный domain и скрытые attributes в value.
    #[test]
    fn builder_rejects_unscoped_or_attribute_smuggling_values() {
        let missing_domain = HttpCookieSeed::builder("session", "cookie-secret")
            .expect("valid pair")
            .build()
            .expect_err("scoped seed without domain must fail");
        assert_eq!(missing_domain, HttpCookieSeedError::MissingDomain);

        let hidden_attribute = match HttpCookieSeed::builder("session", "value; Path=/") {
            Ok(_) => panic!("value must not smuggle Set-Cookie attributes"),
            Err(error) => error,
        };
        assert_eq!(hidden_attribute, HttpCookieSeedError::InvalidValue);
    }
}
