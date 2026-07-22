//! Ephemeral cookie jar с обязательным origin/path/secure scope.
//!
//! Модуль намеренно не знает о media service, extractor-е или credential UI.
//! Он хранит cookies только в памяти одной HTTP source session и дополнительно
//! ограничивает RFC cookie matching внешним `HttpRequestScope` proof-ом.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Mutex;

use reqwest::{
    cookie::{CookieStore, Jar},
    header::HeaderValue,
};
use url::Url;

use crate::{HttpRequestScope, HttpRequestTarget};

/// Per-source in-memory cookie jar, который не может отправить cookie вне scope.
pub struct ScopedHttpCookieJar {
    /// Низкоуровневое origin/path/secure доказательство из S21T boundary.
    scope: HttpRequestScope,
    /// Exact effective Cookie header extractor-а до server-side updates.
    initial_cookies: Option<HeaderValue>,
    /// Имена, которые любой accepted Set-Cookie уже заменил либо удалил.
    overridden_cookie_names: Mutex<BTreeSet<String>>,
    /// RFC-aware reqwest jar для Domain/Path/Secure/expiry semantics.
    jar: Jar,
}

impl ScopedHttpCookieJar {
    /// Создаёт пустой jar и импортирует optional serialized `Cookie` header.
    pub fn new(
        scope: HttpRequestScope,
        initial_target: &HttpRequestTarget,
        serialized_cookies: Option<&[u8]>,
    ) -> Result<Self, ScopedHttpCookieJarError> {
        if !scope.allows(initial_target) {
            return Err(ScopedHttpCookieJarError::InitialTargetOutsideScope);
        }

        let initial_cookies = serialized_cookies
            .map(validate_serialized_cookie_header)
            .transpose()?
            .flatten();
        Ok(Self {
            scope,
            initial_cookies,
            overridden_cookie_names: Mutex::new(BTreeSet::new()),
            jar: Jar::default(),
        })
    }

    /// Проверяет URL reqwest-а через тот же secret-safe HTTP target parser и scope.
    fn allows_url(&self, request_url: &Url) -> bool {
        HttpRequestTarget::parse_exact(request_url.as_str())
            .is_ok_and(|target| self.scope.allows(&target))
    }
}

impl CookieStore for ScopedHttpCookieJar {
    /// Принимает `Set-Cookie` только от response target-а внутри source scope.
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &HeaderValue>,
        response_url: &Url,
    ) {
        if self.allows_url(response_url) {
            let cookie_headers = cookie_headers.cloned().collect::<Vec<_>>();
            let mut overridden_cookie_names = self
                .overridden_cookie_names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for header in &cookie_headers {
                if let Some(cookie_name) = set_cookie_name(header) {
                    overridden_cookie_names.insert(cookie_name.to_owned());
                }
            }
            drop(overridden_cookie_names);
            self.jar
                .set_cookies(&mut cookie_headers.iter(), response_url);
        }
    }

    /// Возвращает RFC-matched cookies только request target-у внутри source scope.
    fn cookies(&self, request_url: &Url) -> Option<HeaderValue> {
        if !self.allows_url(request_url) {
            return None;
        }
        let overridden_cookie_names = self
            .overridden_cookie_names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        merge_cookie_headers(
            self.initial_cookies.as_ref(),
            self.jar.cookies(request_url).as_ref(),
            &overridden_cookie_names,
        )
    }
}

/// Проверяет exact serialized Cookie header без потери duplicate names/order.
fn validate_serialized_cookie_header(
    serialized_cookies: &[u8],
) -> Result<Option<HeaderValue>, ScopedHttpCookieJarError> {
    if serialized_cookies.is_empty() {
        return Ok(None);
    }
    let header = HeaderValue::from_bytes(serialized_cookies)
        .map_err(|_| ScopedHttpCookieJarError::InvalidSerializedCookies)?;
    let serialized = header
        .to_str()
        .map_err(|_| ScopedHttpCookieJarError::InvalidSerializedCookies)?;
    for cookie_pair in serialized.split(';').map(str::trim) {
        let cookie_name = request_cookie_name(cookie_pair)
            .ok_or(ScopedHttpCookieJarError::InvalidSerializedCookies)?;
        if !is_http_token(cookie_name) {
            return Err(ScopedHttpCookieJarError::InvalidSerializedCookies);
        }
    }
    Ok(Some(header))
}

/// Извлекает case-sensitive cookie name из request Cookie pair-а.
fn request_cookie_name(cookie_pair: &str) -> Option<&str> {
    let (name, _value) = cookie_pair.split_once('=')?;
    (!name.is_empty()).then_some(name)
}

/// Извлекает имя из Set-Cookie, не читая value в diagnostics.
fn set_cookie_name(header: &HeaderValue) -> Option<&str> {
    let serialized = header.to_str().ok()?;
    let cookie_pair = serialized.split(';').next()?.trim();
    let name = request_cookie_name(cookie_pair)?;
    is_http_token(name).then_some(name)
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

/// Объединяет exact initial header с RFC-managed updates по cookie name.
fn merge_cookie_headers(
    initial: Option<&HeaderValue>,
    updated: Option<&HeaderValue>,
    overridden_cookie_names: &BTreeSet<String>,
) -> Option<HeaderValue> {
    if overridden_cookie_names.is_empty() && updated.is_none() {
        return initial.cloned();
    }

    let mut merged_pairs = Vec::new();
    if let Some(initial) = initial.and_then(|header| header.to_str().ok()) {
        merged_pairs.extend(initial.split(';').map(str::trim).filter(|cookie_pair| {
            request_cookie_name(cookie_pair)
                .is_some_and(|name| !overridden_cookie_names.contains(name))
        }));
    }
    if let Some(updated) = updated.and_then(|header| header.to_str().ok()) {
        merged_pairs.extend(updated.split(';').map(str::trim));
    }
    if merged_pairs.is_empty() {
        return None;
    }
    Some(
        HeaderValue::from_str(&merged_pairs.join("; "))
            .expect("validated Cookie header pairs must remain valid after joining"),
    )
}

impl fmt::Debug for ScopedHttpCookieJar {
    /// Diagnostics показывают только redacted scope и не перечисляют cookie state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedHttpCookieJar")
            .field("scope", &self.scope)
            .field("cookies", &"<redacted>")
            .finish()
    }
}

/// Secret-safe ошибка initial cookie import-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedHttpCookieJarError {
    /// Caller передал initial target, не доказанный scope-ом.
    #[error("initial cookie target находится вне HTTP request scope")]
    InitialTargetOutsideScope,
    /// Extractor cookie state нельзя представить как serialized Cookie header.
    #[error("serialized cookies имеют недопустимую форму")]
    InvalidSerializedCookies,
}

#[cfg(test)]
mod tests {
    use reqwest::{cookie::CookieStore, header::HeaderValue};
    use url::Url;

    use super::ScopedHttpCookieJar;
    use crate::{HttpPathScope, HttpRequestScope, HttpRequestTarget};

    /// Строит jar с exact `/media` path subtree для focused policy tests.
    fn scoped_jar() -> ScopedHttpCookieJar {
        let target = HttpRequestTarget::parse_exact("https://media.example.test/media/video.mp4")
            .expect("test target must remain valid");
        let path = HttpPathScope::new("/media").expect("test path must remain valid");
        ScopedHttpCookieJar::new(
            HttpRequestScope::from_target(&target, path),
            &target,
            Some(b"session=initial-secret"),
        )
        .expect("test cookie jar must be valid")
    }

    #[test]
    fn cookie_scope_blocks_cross_origin_path_and_downgrade() {
        let jar = scoped_jar();

        let allowed = Url::parse("https://media.example.test/media/chunk").unwrap();
        let sibling_path = Url::parse("https://media.example.test/private/chunk").unwrap();
        let sibling_host = Url::parse("https://cdn.example.test/media/chunk").unwrap();
        let downgrade = Url::parse("http://media.example.test/media/chunk").unwrap();

        assert!(jar.cookies(&allowed).is_some());
        assert!(jar.cookies(&sibling_path).is_none());
        assert!(jar.cookies(&sibling_host).is_none());
        assert!(jar.cookies(&downgrade).is_none());
    }

    #[test]
    fn set_cookie_updates_only_inside_scope() {
        let jar = scoped_jar();
        let allowed = Url::parse("https://media.example.test/media/video.mp4").unwrap();
        let cross_origin = Url::parse("https://cdn.example.test/media/video.mp4").unwrap();
        let in_scope_cookie = HeaderValue::from_static("session=refreshed-secret; Path=/");
        let rejected_cookie = HeaderValue::from_static("leak=cross-origin-secret; Path=/");

        jar.set_cookies(&mut std::iter::once(&in_scope_cookie), &allowed);
        jar.set_cookies(&mut std::iter::once(&rejected_cookie), &cross_origin);

        let serialized = jar
            .cookies(&allowed)
            .expect("allowed target receives cookies");
        let serialized = serialized.to_str().expect("cookie header must be ASCII");
        assert!(serialized.contains("session=refreshed-secret"));
        assert!(!serialized.contains("cross-origin-secret"));
        assert!(jar.cookies(&cross_origin).is_none());
    }

    #[test]
    fn exact_initial_cookie_header_preserves_duplicates_until_server_update() {
        let target = HttpRequestTarget::parse_exact("https://media.example.test/media/video.mp4")
            .expect("test target must remain valid");
        let path = HttpPathScope::new("/media").expect("test path must remain valid");
        let jar = ScopedHttpCookieJar::new(
            HttpRequestScope::from_target(&target, path),
            &target,
            Some(b"session=first; session=second; stable=third"),
        )
        .expect("duplicate effective cookies remain serializable");
        let target_url = Url::parse("https://media.example.test/media/video.mp4")
            .expect("test URL must remain valid");

        assert_eq!(
            jar.cookies(&target_url)
                .expect("initial Cookie header exists")
                .to_str()
                .expect("Cookie header remains ASCII"),
            "session=first; session=second; stable=third"
        );

        let update = HeaderValue::from_static("session=refreshed; Path=/");
        jar.set_cookies(&mut std::iter::once(&update), &target_url);
        let refreshed = jar
            .cookies(&target_url)
            .expect("refreshed Cookie header exists");
        let refreshed = refreshed.to_str().expect("Cookie header remains ASCII");
        assert!(!refreshed.contains("session=first"));
        assert!(!refreshed.contains("session=second"));
        assert!(refreshed.contains("session=refreshed"));
        assert!(refreshed.contains("stable=third"));
    }

    #[test]
    fn debug_never_exposes_cookie_values() {
        let debug = format!("{:?}", scoped_jar());

        assert!(!debug.contains("initial-secret"));
        assert!(debug.contains("<redacted>"));
    }
}
