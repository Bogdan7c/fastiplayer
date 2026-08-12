//! Единственная проекция HTTP authorization material в neutral transport context.
//!
//! Модуль владеет различием между готовым request `Cookie` header-ом и
//! response-style cookie seeds. Остальной candidate transport передаёт только
//! intent и не знает, как он хранится внутри `SecretRequestContext`.

use source_core::{HttpHeader, HttpPathScope, HttpRequestTarget, ValidatedHttpHeaders};
use url::Url;
use web_media_transport_api::{
    SecretRequestContext, SecretRequestContextBuilder, SecretRequestScope,
};

use super::super::request_material::{
    YtDlpCookieMaterialRef, YtDlpDashFragmentLocatorKind, YtDlpDashInputKind,
    YtDlpDashRequestMaterial, YtDlpSmoothManifestRequestMaterial,
};
use super::YtDlpTransportRequestError;

/// Создаёт builder с проверенными headers и одним typed cookie intent-ом.
pub(super) fn http_secret_context_builder<'header>(
    secret_scope: SecretRequestScope,
    headers: impl Iterator<Item = (&'header str, &'header str)>,
    cookies: Option<YtDlpCookieMaterialRef<'_>>,
) -> Result<SecretRequestContextBuilder, YtDlpTransportRequestError> {
    let serialized_headers = headers
        .map(|(name, value)| HttpHeader::new(name, value))
        .collect::<Vec<_>>();
    let validated_headers = ValidatedHttpHeaders::new(serialized_headers)
        .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
    let secret_builder =
        SecretRequestContext::builder(secret_scope).with_headers(validated_headers);

    match cookies {
        None => Ok(secret_builder),
        Some(YtDlpCookieMaterialRef::RequestHeader(request_header)) => secret_builder
            .with_serialized_cookies(request_header)
            .map_err(YtDlpTransportRequestError::AuthorizationSerialization),
        Some(YtDlpCookieMaterialRef::ScopedSeeds(scoped_seeds)) => {
            Ok(secret_builder.with_scoped_cookie_seeds(scoped_seeds.iter().cloned()))
        }
    }
}

/// Собирает final Smooth presentation context для Manifest и sibling fragments.
pub(super) fn smooth_manifest_secret_context(
    material: &YtDlpSmoothManifestRequestMaterial<'_>,
    target: &HttpRequestTarget,
) -> Result<SecretRequestContext, YtDlpTransportRequestError> {
    let path_scope = resource_directory_path_scope(target)
        .ok_or(YtDlpTransportRequestError::SmoothTargetResolution)?;
    let secret_scope = SecretRequestScope::from_target(target, path_scope);
    http_secret_context_builder(secret_scope, material.headers(), material.cookies())
        .map(SecretRequestContextBuilder::build)
}

/// Ограничивает HDS credentials каталогом authoritative manifest-а.
pub(super) fn hds_resource_path_scope(
    target: &HttpRequestTarget,
) -> Result<HttpPathScope, YtDlpTransportRequestError> {
    resource_directory_path_scope(target).ok_or(YtDlpTransportRequestError::HdsTargetResolution)
}

/// Возвращает каталог playlist/manifest URL для sibling media resources.
pub(super) fn resource_directory_path_scope(target: &HttpRequestTarget) -> Option<HttpPathScope> {
    let parsed = Url::parse(target.expose_secret_for_request()).ok()?;
    let path = parsed.path();
    let directory = if path.ends_with('/') {
        path.to_owned()
    } else {
        path.rsplit_once('/')
            .map_or_else(|| "/".to_owned(), |(parent, _)| format!("{parent}/"))
    };
    HttpPathScope::new(directory).ok()
}

/// Выбирает request-scope anchor без fallback между authoritative DASH inputs.
pub(super) fn dash_transport_anchor(
    material: &YtDlpDashRequestMaterial<'_>,
) -> Result<String, YtDlpTransportRequestError> {
    match material.input().kind() {
        YtDlpDashInputKind::Manifest => material
            .input()
            .manifest_url_for_fetch()
            .map(ToOwned::to_owned)
            .ok_or(YtDlpTransportRequestError::DashTargetResolution),
        YtDlpDashInputKind::SerializedFragments => {
            let fragment = material
                .input()
                .fragments()
                .next()
                .ok_or(YtDlpTransportRequestError::DashTargetResolution)?;
            match fragment.locator_kind() {
                YtDlpDashFragmentLocatorKind::AbsoluteUrl => {
                    Ok(fragment.locator_for_transport().to_owned())
                }
                YtDlpDashFragmentLocatorKind::RelativePath => {
                    let base = fragment
                        .base_url_for_relative_resolution()
                        .ok_or(YtDlpTransportRequestError::DashTargetResolution)?;
                    let parsed_base = Url::parse(base)
                        .map_err(|_| YtDlpTransportRequestError::DashTargetResolution)?;
                    parsed_base
                        .join(fragment.locator_for_transport())
                        .map(Into::into)
                        .map_err(|_| YtDlpTransportRequestError::DashTargetResolution)
                }
            }
        }
    }
}

/// Ограничивает DASH credentials директорией authoritative MPD/fragment base-а.
///
/// Exact-file scope progressive source-а недостаточен segmented transport-у:
/// sibling init/media resources должны получить тот же fresh request context.
/// Origin и HTTPS downgrade по-прежнему проверяет shared S21T boundary.
pub(super) fn dash_resource_path_scope(
    material: &YtDlpDashRequestMaterial<'_>,
    anchor: &HttpRequestTarget,
) -> Result<HttpPathScope, YtDlpTransportRequestError> {
    let scope_locator = if material.input().kind() == YtDlpDashInputKind::SerializedFragments {
        material
            .input()
            .fragments()
            .next()
            .and_then(|fragment| {
                fragment
                    .base_url_for_relative_resolution()
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| anchor.expose_secret_for_request().to_owned())
    } else {
        anchor.expose_secret_for_request().to_owned()
    };
    let parsed_scope =
        Url::parse(&scope_locator).map_err(|_| YtDlpTransportRequestError::DashTargetResolution)?;
    let scope_path = parsed_scope.path();
    let directory_path = if scope_path.ends_with('/') {
        scope_path.to_owned()
    } else {
        let parent = scope_path
            .rsplit_once('/')
            .map_or("/", |(parent, _)| parent);
        format!("{parent}/")
    };
    HttpPathScope::new(directory_path).map_err(|_| YtDlpTransportRequestError::DashTargetResolution)
}
