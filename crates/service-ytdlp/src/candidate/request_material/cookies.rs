//! Pinned yt-dlp cookie serialization → typed request material.
//!
//! `yt-dlp 2026.07.04` записывает поле `cookies` как последовательность
//! `name=value; Domain=...; Path=...; Secure; Expires=<unix>; ...`, а не как
//! готовый request `Cookie` header. Здесь находится единственное знание этого
//! upstream contract-а; transport/source слои получают только neutral types.

use serde_json::Value;
use source_core::HttpCookieSeed;

use super::{MAX_REQUEST_SECRET_UTF8_BYTES, SecretText, YtDlpRequestMaterialViolation};

/// Нормализованная форма одного yt-dlp `cookies` field-а.
#[derive(Clone, PartialEq)]
pub(super) enum YtDlpCookieMaterial {
    /// Backward-compatible готовый request Cookie header без scope attributes.
    RequestHeader(SecretText),
    /// Pinned response-style cookie records с индивидуальными scopes.
    ScopedSeeds(Box<[HttpCookieSeed]>),
}

impl YtDlpCookieMaterial {
    /// Возвращает borrowed variant без раскрытия secret в diagnostics.
    pub(super) fn as_ref(&self) -> YtDlpCookieMaterialRef<'_> {
        match self {
            Self::RequestHeader(header) => {
                YtDlpCookieMaterialRef::RequestHeader(header.expose_secret_for_transport())
            }
            Self::ScopedSeeds(seeds) => YtDlpCookieMaterialRef::ScopedSeeds(seeds),
        }
    }
}

/// Borrowed effective cookie intent для transport projection.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum YtDlpCookieMaterialRef<'material> {
    /// Exact request Cookie header.
    RequestHeader(&'material str),
    /// Individually scoped cookie seeds.
    ScopedSeeds(&'material [HttpCookieSeed]),
}

impl<'material> YtDlpCookieMaterialRef<'material> {
    /// Раскрывает request header только named transport builder-у.
    pub(crate) fn request_header(self) -> Option<&'material str> {
        match self {
            Self::RequestHeader(header) => Some(header),
            Self::ScopedSeeds(_) => None,
        }
    }
}

/// Mutable accumulator ровно одного flattened scoped cookie record-а.
struct PendingCookie {
    /// Cookie name.
    name: String,
    /// Serialized cookie value.
    value: String,
    /// Обязательный Domain attribute.
    domain: Option<String>,
    /// Optional Path attribute.
    path: Option<String>,
    /// Secure marker.
    secure_only: bool,
    /// Optional абсолютный Unix timestamp.
    expires_at_unix_seconds: Option<i64>,
    /// Version marker уже встречался; даже повторный `Version=0` неоднозначен.
    version_seen: bool,
}

/// Атрибуты, которые pinned yt-dlp реально сериализует после cookie pair.
enum CookieAttribute<'serialized> {
    /// Domain scope.
    Domain(&'serialized str),
    /// Path scope.
    Path(&'serialized str),
    /// HTTPS-only marker.
    Secure,
    /// Unix expiration timestamp.
    Expires(&'serialized str),
    /// Устаревшая cookie version; поддерживается только нейтральный `0`.
    Version(&'serialized str),
}

/// Нормализует optional yt-dlp cookie field до I/O и provider mutation.
pub(super) fn normalize_yt_dlp_cookies(
    raw_cookies: Option<&Value>,
) -> Result<Option<YtDlpCookieMaterial>, YtDlpRequestMaterialViolation> {
    let Some(raw_cookies) = raw_cookies else {
        return Ok(None);
    };
    let Some(cookies) = raw_cookies.as_str() else {
        return Err(YtDlpRequestMaterialViolation::InvalidCookies);
    };
    let bounded = SecretText::bounded(cookies.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
        .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)?;
    if cookies.is_empty() {
        return Ok(None);
    }
    if !contains_scoped_cookie_attribute(cookies) {
        return Ok(Some(YtDlpCookieMaterial::RequestHeader(bounded)));
    }

    parse_scoped_cookie_seeds(cookies)
        .map(YtDlpCookieMaterial::ScopedSeeds)
        .map(Some)
}

/// Отличает pinned scoped representation от старого request-header subset-а.
fn contains_scoped_cookie_attribute(serialized: &str) -> bool {
    serialized
        .split(';')
        .map(str::trim)
        .any(|token| cookie_attribute(token).is_some())
}

/// Разбирает bounded flattened records и не допускает неявных attributes.
fn parse_scoped_cookie_seeds(
    serialized: &str,
) -> Result<Box<[HttpCookieSeed]>, YtDlpRequestMaterialViolation> {
    let mut seeds = Vec::new();
    let mut pending_cookie = None;
    for token in serialized.split(';').map(str::trim) {
        if token.is_empty() {
            return Err(YtDlpRequestMaterialViolation::InvalidCookies);
        }
        if let Some(attribute) = cookie_attribute(token) {
            apply_cookie_attribute(
                pending_cookie
                    .as_mut()
                    .ok_or(YtDlpRequestMaterialViolation::InvalidCookies)?,
                attribute,
            )?;
            continue;
        }

        if let Some(completed) = pending_cookie.take() {
            seeds.push(build_cookie_seed(completed)?);
        }
        let (name, value) = token
            .split_once('=')
            .ok_or(YtDlpRequestMaterialViolation::InvalidCookies)?;
        if name.is_empty() {
            return Err(YtDlpRequestMaterialViolation::InvalidCookies);
        }
        pending_cookie = Some(PendingCookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: None,
            path: None,
            secure_only: false,
            expires_at_unix_seconds: None,
            version_seen: false,
        });
    }
    let completed = pending_cookie.ok_or(YtDlpRequestMaterialViolation::InvalidCookies)?;
    seeds.push(build_cookie_seed(completed)?);
    Ok(seeds.into_boxed_slice())
}

/// Распознаёт только exact pinned attribute vocabulary.
fn cookie_attribute(token: &str) -> Option<CookieAttribute<'_>> {
    if token.eq_ignore_ascii_case("secure") {
        return Some(CookieAttribute::Secure);
    }
    let (name, value) = token.split_once('=')?;
    if name.eq_ignore_ascii_case("domain") {
        Some(CookieAttribute::Domain(value))
    } else if name.eq_ignore_ascii_case("path") {
        Some(CookieAttribute::Path(value))
    } else if name.eq_ignore_ascii_case("expires") {
        Some(CookieAttribute::Expires(value))
    } else if name.eq_ignore_ascii_case("version") {
        Some(CookieAttribute::Version(value))
    } else {
        None
    }
}

/// Применяет attribute ровно один раз к текущему cookie record-у.
fn apply_cookie_attribute(
    cookie: &mut PendingCookie,
    attribute: CookieAttribute<'_>,
) -> Result<(), YtDlpRequestMaterialViolation> {
    match attribute {
        CookieAttribute::Domain(domain) if cookie.domain.is_none() && !domain.is_empty() => {
            cookie.domain = Some(domain.to_owned());
        }
        CookieAttribute::Path(path) if cookie.path.is_none() && !path.is_empty() => {
            cookie.path = Some(path.to_owned());
        }
        CookieAttribute::Secure if !cookie.secure_only => {
            cookie.secure_only = true;
        }
        CookieAttribute::Expires(expires) if cookie.expires_at_unix_seconds.is_none() => {
            cookie.expires_at_unix_seconds = Some(
                expires
                    .parse::<i64>()
                    .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)?,
            );
        }
        CookieAttribute::Version("0") if !cookie.version_seen => {
            cookie.version_seen = true;
        }
        CookieAttribute::Domain(_)
        | CookieAttribute::Path(_)
        | CookieAttribute::Secure
        | CookieAttribute::Expires(_)
        | CookieAttribute::Version(_) => {
            return Err(YtDlpRequestMaterialViolation::InvalidCookies);
        }
    }
    Ok(())
}

/// Переводит service-owned record в neutral low-level seed.
fn build_cookie_seed(
    cookie: PendingCookie,
) -> Result<HttpCookieSeed, YtDlpRequestMaterialViolation> {
    let domain = cookie
        .domain
        .ok_or(YtDlpRequestMaterialViolation::InvalidCookies)?;
    let mut builder = HttpCookieSeed::builder(cookie.name, cookie.value)
        .and_then(|builder| builder.for_domain(domain))
        .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)?;
    if let Some(path) = cookie.path {
        builder = builder
            .with_path(path)
            .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)?;
    }
    if cookie.secure_only {
        builder = builder.secure_only();
    }
    if let Some(expires_at) = cookie.expires_at_unix_seconds {
        builder = builder
            .expires_at_unix_seconds(expires_at)
            .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)?;
    }
    builder
        .build()
        .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Реальная shape pinned yt-dlp становится scoped seeds, а не request pairs.
    #[test]
    fn scoped_cookie_records_preserve_individual_attributes() {
        let raw = Value::String(
            "session=secret-a; Domain=.example.test; Path=/media; Secure; Expires=1900000000; second=secret-b; Domain=cdn.example.test; Path=/"
                .to_owned(),
        );
        let material = normalize_yt_dlp_cookies(Some(&raw))
            .expect("pinned scoped representation")
            .expect("cookie material exists");
        let YtDlpCookieMaterial::ScopedSeeds(seeds) = material else {
            panic!("scope attributes must never become a request Cookie header");
        };

        assert_eq!(seeds.len(), 2);
        assert!(!format!("{seeds:?}").contains("secret-a"));
        assert!(!format!("{seeds:?}").contains("secret-b"));
    }

    /// Старый exact request header остаётся совместимым, но mixed shape fail-closed.
    #[test]
    fn request_header_subset_is_distinct_from_malformed_scoped_records() {
        let request_header = Value::String("first=one; second=two".to_owned());
        let material = normalize_yt_dlp_cookies(Some(&request_header))
            .expect("legacy request header remains valid")
            .expect("cookie material exists");
        assert!(matches!(material, YtDlpCookieMaterial::RequestHeader(_)));

        let missing_domain = Value::String("session=secret; Path=/media".to_owned());
        assert!(matches!(
            normalize_yt_dlp_cookies(Some(&missing_domain)),
            Err(YtDlpRequestMaterialViolation::InvalidCookies)
        ));
        let unsupported_version =
            Value::String("session=secret; Domain=example.test; Version=1".to_owned());
        assert!(matches!(
            normalize_yt_dlp_cookies(Some(&unsupported_version)),
            Err(YtDlpRequestMaterialViolation::InvalidCookies)
        ));
        let duplicate_version =
            Value::String("session=secret; Domain=example.test; Version=0; Version=0".to_owned());
        assert!(matches!(
            normalize_yt_dlp_cookies(Some(&duplicate_version)),
            Err(YtDlpRequestMaterialViolation::InvalidCookies)
        ));
    }
}
