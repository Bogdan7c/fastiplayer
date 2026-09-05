use std::fmt;

use url::Url;
use web_media_transport_api::{
    SecretQueryOverride, SecretQueryOverrideError, SecretRequestContextBuilder,
};

use super::{
    SecretText, YtDlpCookieMaterialRef, YtDlpRequestFragment, YtDlpRequestMaterial,
    YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
};

/// Fixed comparison origin; эта строка никогда не используется для запроса.
const RELATIVE_FRAGMENT_SENTINEL_BASE: &str = "https://fastiplayer.invalid/dash-base/";

/// Выбранный authoritative DASH input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpDashInputKind {
    /// MPD endpoint; multi-period semantics принадлежат MPD.
    Manifest,
    /// Concrete bounded fragments одного Period.
    SerializedFragments,
}

/// DASH input без публичного доступа к secret-bearing storage.
pub struct YtDlpDashInput<'material> {
    /// Private variant сохраняет mutually-exclusive invariant.
    inner: DashInput<'material>,
}

/// Service-private storage variant.
enum DashInput<'material> {
    /// Manifest-backed путь.
    Manifest {
        /// MPD endpoint, выбранный по service-owned precedence.
        manifest_url: &'material SecretText,
    },
    /// Serialized single-period путь.
    SerializedFragments {
        /// Optional base для relative fragment paths.
        fragment_base_url: Option<&'material SecretText>,
        /// Concrete validated fragment rows.
        fragments: &'material [YtDlpRequestFragment],
    },
}

impl YtDlpDashInput<'_> {
    /// Возвращает non-secret variant.
    pub const fn kind(&self) -> YtDlpDashInputKind {
        match self.inner {
            DashInput::Manifest { .. } => YtDlpDashInputKind::Manifest,
            DashInput::SerializedFragments { .. } => YtDlpDashInputKind::SerializedFragments,
        }
    }

    /// Раскрывает MPD URL только manifest fetch owner-у.
    pub fn manifest_url_for_fetch(&self) -> Option<&str> {
        match &self.inner {
            DashInput::Manifest { manifest_url } => {
                Some(manifest_url.expose_secret_for_transport())
            }
            DashInput::SerializedFragments { .. } => None,
        }
    }

    /// Возвращает validated fragments только serialized runtime-у.
    pub fn fragments(&self) -> impl ExactSizeIterator<Item = YtDlpDashFragment<'_>> {
        let (fragment_base_url, fragments) = match &self.inner {
            DashInput::SerializedFragments {
                fragment_base_url,
                fragments,
            } => (*fragment_base_url, *fragments),
            DashInput::Manifest { .. } => (None, &[] as &[YtDlpRequestFragment]),
        };
        fragments
            .iter()
            .enumerate()
            .map(move |(index, fragment)| YtDlpDashFragment {
                fragment,
                fragment_base_url,
                role: if index == 0 {
                    YtDlpDashFragmentRole::Initialization
                } else {
                    YtDlpDashFragmentRole::Media
                },
            })
    }
}

impl fmt::Debug for YtDlpDashInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDashInput")
            .field("kind", &self.kind())
            .field("fragment_count", &self.fragments().len())
            .finish_non_exhaustive()
    }
}

/// Locator shape одного fragment-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpDashFragmentLocatorKind {
    /// Upstream absolute `url` имеет приоритет над `path`.
    AbsoluteUrl,
    /// Relative `path` должен разрешаться от fragment base.
    RelativePath,
}

/// Доказанная serialized fragment role из pinned yt-dlp shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpDashFragmentRole {
    /// Единственный первый fragment без duration.
    Initialization,
    /// Каждый последующий fragment с positive duration.
    Media,
}

/// Borrowed validated fragment без публичного Debug plaintext.
pub struct YtDlpDashFragment<'material> {
    /// Service-owned fragment row.
    fragment: &'material YtDlpRequestFragment,
    /// Required base только для relative path.
    fragment_base_url: Option<&'material SecretText>,
    /// Role уже доказана validation-ом полного ordered списка.
    role: YtDlpDashFragmentRole,
}

impl YtDlpDashFragment<'_> {
    /// Возвращает initialization/media role без inference в runtime-е.
    pub const fn role(&self) -> YtDlpDashFragmentRole {
        self.role
    }

    /// Возвращает форму locator-а.
    pub const fn locator_kind(&self) -> YtDlpDashFragmentLocatorKind {
        if self.fragment.url.is_some() {
            YtDlpDashFragmentLocatorKind::AbsoluteUrl
        } else {
            YtDlpDashFragmentLocatorKind::RelativePath
        }
    }

    /// Раскрывает absolute URL или relative path только transport owner-у.
    pub fn locator_for_transport(&self) -> &str {
        self.fragment
            .url
            .as_ref()
            .or(self.fragment.path.as_ref())
            .expect("DASH fragment invariant established during normalization")
            .expose_secret_for_transport()
    }

    /// Раскрывает base только для relative reference resolution.
    pub fn base_url_for_relative_resolution(&self) -> Option<&str> {
        match self.locator_kind() {
            YtDlpDashFragmentLocatorKind::AbsoluteUrl => None,
            YtDlpDashFragmentLocatorKind::RelativePath => self
                .fragment_base_url
                .map(SecretText::expose_secret_for_transport),
        }
    }

    /// Optional finite non-negative duration.
    pub const fn duration_seconds(&self) -> Option<f64> {
        self.fragment.duration_seconds
    }

    /// Optional bounded serialized byte length.
    pub const fn byte_length(&self) -> Option<u64> {
        self.fragment.byte_length
    }
}

impl fmt::Debug for YtDlpDashFragment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDashFragment")
            .field("role", &self.role())
            .field("locator_kind", &self.locator_kind())
            .field("duration_seconds", &self.duration_seconds())
            .field("byte_length", &self.byte_length())
            .finish_non_exhaustive()
    }
}

/// Scoped HTTP authorization projection для MPD/init/media requests.
pub struct YtDlpDashRequestContext<'material> {
    /// Validated request material.
    material: &'material YtDlpRequestMaterialV1,
    /// Единственная effective Cookie форма.
    cookies: Option<YtDlpCookieMaterialRef<'material>>,
}

impl<'material> YtDlpDashRequestContext<'material> {
    /// Итерирует headers кроме Cookie, которому выделена отдельная boundary.
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.material
            .http_headers
            .iter()
            .filter(|(name, _value)| !name.eq_ignore_ascii_case("cookie"))
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    /// Возвращает effective Cookie intent без flattening scope attributes.
    pub(crate) const fn cookies(&self) -> Option<YtDlpCookieMaterialRef<'material>> {
        self.cookies
    }

    /// Возвращает legacy request-header форму для focused compatibility tests.
    pub fn serialized_cookies(&self) -> Option<&str> {
        self.cookies
            .and_then(YtDlpCookieMaterialRef::request_header)
    }
}

impl fmt::Debug for YtDlpDashRequestContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDashRequestContext")
            .field("header_count", &self.headers().count())
            .field("has_cookies", &self.cookies.is_some())
            .finish_non_exhaustive()
    }
}

/// Полный validated DASH request handoff.
pub struct YtDlpDashRequestMaterial<'material> {
    /// Authoritative MPD либо serialized input.
    input: YtDlpDashInput<'material>,
    /// Shared request authorization state.
    request_context: YtDlpDashRequestContext<'material>,
    /// Query projection для init/media fragments.
    segment_query: Option<&'material SecretText>,
}

impl YtDlpDashRequestMaterial<'_> {
    /// Возвращает mutually-exclusive input.
    pub const fn input(&self) -> &YtDlpDashInput<'_> {
        &self.input
    }

    /// Возвращает request context.
    pub const fn request_context(&self) -> &YtDlpDashRequestContext<'_> {
        &self.request_context
    }

    /// Раскрывает segment query только typed query projection owner-у.
    pub fn segment_query_parameters_for_projection(&self) -> Option<&str> {
        self.segment_query
            .map(SecretText::expose_secret_for_transport)
    }

    /// Единожды переносит segment query в shared S21T secret scope.
    pub fn project_scoped_query(
        &self,
        mut builder: SecretRequestContextBuilder,
    ) -> Result<SecretRequestContextBuilder, SecretQueryOverrideError> {
        if let Some(query) = self.segment_query_parameters_for_projection() {
            builder = builder.with_segment_query_override(SecretQueryOverride::new(query)?);
        }
        Ok(builder)
    }

    /// Доказывает, что два separate component-а могут безопасно разделить один MPD context.
    #[must_use]
    pub fn shares_manifest_runtime_context(&self, other: &Self) -> bool {
        let same_manifest = matches!(
            (&self.input.inner, &other.input.inner),
            (
                DashInput::Manifest {
                    manifest_url: left
                },
                DashInput::Manifest {
                    manifest_url: right
                }
            ) if *left == *right
        );
        same_manifest
            && self.request_context.material.http_headers
                == other.request_context.material.http_headers
            && self.request_context.cookies == other.request_context.cookies
            && self.segment_query == other.segment_query
    }
}

impl fmt::Debug for YtDlpDashRequestMaterial<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDashRequestMaterial")
            .field("input", &self.input)
            .field("request_context", &self.request_context)
            .field("has_segment_query", &self.segment_query.is_some())
            .finish_non_exhaustive()
    }
}

/// Typed DASH material rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpDashRequestMaterialViolation {
    /// Общая authorization serialization конфликтует.
    #[error("invalid DASH request context")]
    InvalidRequestContext,
    /// Нет MPD и нет concrete fragments.
    #[error("DASH request has no manifest or fragments")]
    MissingInput,
    /// Multi-period serialization требует authoritative MPD.
    #[error("multi-period DASH requires an MPD")]
    MultiPeriodRequiresManifest,
    /// Relative fragment не имеет base URL.
    #[error("relative DASH fragment has no base")]
    RelativeFragmentMissingBase,
    /// MPD endpoint не является absolute HTTP(S) URL.
    #[error("invalid DASH manifest URL")]
    InvalidManifestUrl,
    /// Absolute fragment URL либо relative path имеет неверную форму.
    #[error("invalid DASH fragment locator")]
    InvalidFragmentLocator,
    /// Serialized row не начинается с единственного initialization fragment-а.
    #[error("serialized DASH fragments require one leading initialization fragment")]
    MissingLeadingInitialization,
    /// Serialized row не содержит media fragments после initialization.
    #[error("serialized DASH fragments contain no media fragments")]
    MissingMediaFragments,
    /// Media fragment без positive duration неоднозначен относительно второй init.
    #[error("serialized DASH media fragment has no positive duration")]
    AmbiguousFragmentRole,
    /// Fragment base не является absolute HTTP(S) URL.
    #[error("invalid DASH fragment base URL")]
    InvalidFragmentBaseUrl,
}

/// Строит DASH projection с explicit precedence и без runtime fallback.
pub(super) fn dash_request_material(
    request: &YtDlpRequestMaterial,
) -> Result<YtDlpDashRequestMaterial<'_>, YtDlpDashRequestMaterialViolation> {
    let YtDlpRequestMaterial::V1(material) = request;
    let authorization = request
        .http_authorization_material()
        .map_err(map_request_context_violation)?;
    let input = if material.is_dash_periods {
        let manifest_url = material
            .manifest_url
            .as_ref()
            .ok_or(YtDlpDashRequestMaterialViolation::MultiPeriodRequiresManifest)?;
        validate_absolute_http_url(manifest_url)
            .map_err(|_| YtDlpDashRequestMaterialViolation::InvalidManifestUrl)?;
        YtDlpDashInput {
            inner: DashInput::Manifest { manifest_url },
        }
    } else if !material.fragments.is_empty() {
        if material
            .fragments
            .iter()
            .any(|fragment| fragment.url.is_none() && material.fragment_base_url.is_none())
        {
            return Err(YtDlpDashRequestMaterialViolation::RelativeFragmentMissingBase);
        }
        validate_serialized_fragments(material)?;
        YtDlpDashInput {
            inner: DashInput::SerializedFragments {
                fragment_base_url: material.fragment_base_url.as_ref(),
                fragments: &material.fragments,
            },
        }
    } else {
        let manifest_url = material
            .manifest_url
            .as_ref()
            .or(material.url.as_ref())
            .ok_or(YtDlpDashRequestMaterialViolation::MissingInput)?;
        validate_absolute_http_url(manifest_url)
            .map_err(|_| YtDlpDashRequestMaterialViolation::InvalidManifestUrl)?;
        YtDlpDashInput {
            inner: DashInput::Manifest { manifest_url },
        }
    };
    Ok(YtDlpDashRequestMaterial {
        input,
        request_context: YtDlpDashRequestContext {
            material,
            cookies: authorization.cookies(),
        },
        segment_query: material.extra_param_to_segment_url.as_ref(),
    })
}

/// Проверяет absolute HTTP(S) endpoint без reserialization identity.
fn validate_absolute_http_url(secret_url: &SecretText) -> Result<(), ()> {
    let parsed = Url::parse(secret_url.expose_secret_for_transport()).map_err(|_| ())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(());
    }
    Ok(())
}

/// Проверяет URL/path/base shape всех authoritative fragments.
fn validate_serialized_fragments(
    material: &YtDlpRequestMaterialV1,
) -> Result<(), YtDlpDashRequestMaterialViolation> {
    if let Some(fragment_base_url) = material.fragment_base_url.as_ref() {
        validate_absolute_http_url(fragment_base_url)
            .map_err(|_| YtDlpDashRequestMaterialViolation::InvalidFragmentBaseUrl)?;
    }
    let sentinel_base = Url::parse(RELATIVE_FRAGMENT_SENTINEL_BASE)
        .expect("fixed DASH fragment sentinel URL is valid");
    for fragment in &material.fragments {
        if let Some(fragment_url) = fragment.url.as_ref() {
            validate_absolute_http_url(fragment_url)
                .map_err(|_| YtDlpDashRequestMaterialViolation::InvalidFragmentLocator)?;
            continue;
        }
        let relative_path = fragment
            .path
            .as_ref()
            .ok_or(YtDlpDashRequestMaterialViolation::InvalidFragmentLocator)?
            .expose_secret_for_transport();
        if relative_path.is_empty()
            || Url::parse(relative_path).is_ok()
            || starts_with_network_path_prefix(relative_path)
        {
            return Err(YtDlpDashRequestMaterialViolation::InvalidFragmentLocator);
        }
        let resolved_reference = sentinel_base
            .join(relative_path)
            .map_err(|_| YtDlpDashRequestMaterialViolation::InvalidFragmentLocator)?;
        if resolved_reference.origin() != sentinel_base.origin() {
            return Err(YtDlpDashRequestMaterialViolation::InvalidFragmentLocator);
        }
    }
    let Some((initialization, media_fragments)) = material.fragments.split_first() else {
        return Err(YtDlpDashRequestMaterialViolation::MissingInput);
    };
    if initialization.duration_seconds.is_some() {
        return Err(YtDlpDashRequestMaterialViolation::MissingLeadingInitialization);
    }
    if media_fragments.is_empty() {
        return Err(YtDlpDashRequestMaterialViolation::MissingMediaFragments);
    }
    if media_fragments.iter().any(|fragment| {
        fragment
            .duration_seconds
            .is_none_or(|duration| !duration.is_finite() || duration <= 0.0)
    }) {
        return Err(YtDlpDashRequestMaterialViolation::AmbiguousFragmentRole);
    }
    Ok(())
}

/// Отделяет root-relative `/path` от network-path `//host` и slash/backslash aliases.
fn starts_with_network_path_prefix(reference: &str) -> bool {
    let mut characters = reference.chars();
    let first_is_separator = characters
        .next()
        .is_some_and(|character| matches!(character, '/' | '\\'));
    let second_is_separator = characters
        .next()
        .is_some_and(|character| matches!(character, '/' | '\\'));
    first_is_separator && second_is_separator
}

/// Не выпускает generic violation наружу из DASH-specific API.
fn map_request_context_violation(
    _violation: YtDlpRequestMaterialViolation,
) -> YtDlpDashRequestMaterialViolation {
    YtDlpDashRequestMaterialViolation::InvalidRequestContext
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::candidate::raw::YtDlpSerializedFormat;
    use crate::candidate::request_material::{
        MAX_REQUEST_SECRET_UTF8_BYTES, YtDlpRequestMaterialSummary, normalize_fragments,
        normalize_request_material,
    };

    /// Test helper создаёт bounded secret без публичного API.
    fn secret(value: &str) -> SecretText {
        SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
            .expect("test secret bounded")
    }

    /// Собирает service-private material с explicit DASH fields.
    fn request(
        url: Option<&str>,
        manifest_url: Option<&str>,
        fragments: Vec<YtDlpRequestFragment>,
        fragment_base_url: Option<&str>,
        is_dash_periods: bool,
    ) -> YtDlpRequestMaterial {
        let mut http_headers = BTreeMap::new();
        http_headers.insert("Authorization".to_owned(), secret("Bearer top-secret"));
        YtDlpRequestMaterial::V1(YtDlpRequestMaterialV1 {
            url: url.map(secret),
            manifest_url: manifest_url.map(secret),
            fragments: fragments.into_boxed_slice(),
            fragment_base_url: fragment_base_url.map(secret),
            is_dash_periods,
            hls_media_playlist_data: None,
            http_headers,
            http_range_request_limit: None,
            cookies: Some(super::super::YtDlpCookieMaterial::RequestHeader(secret(
                "session=top-secret",
            ))),
            extra_param_to_segment_url: Some(secret("token=top-secret")),
            extra_param_to_key_url: None,
            hls_aes: None,
            rtmp: None,
        })
    }

    /// Создаёт fragment с обеими upstream locator формами для precedence proof.
    fn fragment(
        url: Option<&str>,
        path: Option<&str>,
        duration_seconds: Option<f64>,
        byte_length: Option<u64>,
    ) -> YtDlpRequestFragment {
        YtDlpRequestFragment {
            url: url.map(secret),
            path: path.map(secret),
            duration_seconds,
            byte_length,
        }
    }

    #[test]
    fn non_empty_fragments_are_authoritative_and_absolute_url_wins_over_path() {
        let request = request(
            Some("https://cdn.invalid/selected.mpd"),
            Some("https://cdn.invalid/manifest.mpd"),
            vec![
                fragment(
                    Some("https://cdn.invalid/absolute.m4s?secret=1"),
                    Some("ignored.m4s"),
                    None,
                    Some(123),
                ),
                fragment(None, Some("relative.m4s"), Some(2.5), None),
            ],
            Some("https://cdn.invalid/base/"),
            false,
        );
        let dash = request.dash_request_material().expect("valid fragments");
        assert_eq!(dash.input().kind(), YtDlpDashInputKind::SerializedFragments);
        assert_eq!(dash.input().manifest_url_for_fetch(), None);
        let fragments = dash.input().fragments().collect::<Vec<_>>();
        assert_eq!(
            fragments[0].locator_kind(),
            YtDlpDashFragmentLocatorKind::AbsoluteUrl
        );
        assert_eq!(
            fragments[0].locator_for_transport(),
            "https://cdn.invalid/absolute.m4s?secret=1"
        );
        assert_eq!(fragments[0].base_url_for_relative_resolution(), None);
        assert_eq!(fragments[0].role(), YtDlpDashFragmentRole::Initialization);
        assert_eq!(fragments[0].duration_seconds(), None);
        assert_eq!(fragments[0].byte_length(), Some(123));
        assert_eq!(fragments[1].role(), YtDlpDashFragmentRole::Media);
        assert_eq!(
            fragments[1].base_url_for_relative_resolution(),
            Some("https://cdn.invalid/base/")
        );
        assert_eq!(
            dash.segment_query_parameters_for_projection(),
            Some("token=top-secret")
        );
        assert_eq!(dash.request_context().headers().count(), 1);
        assert_eq!(
            dash.request_context().serialized_cookies(),
            Some("session=top-secret")
        );
    }

    #[test]
    fn multi_period_marker_forces_manifest_and_missing_manifest_is_typed_reject() {
        let fragments = vec![fragment(None, Some("period-unknown.m4s"), None, None)];
        let with_manifest = request(
            Some("https://cdn.invalid/selected.mpd"),
            Some("https://cdn.invalid/authoritative.mpd"),
            fragments.clone(),
            Some("https://cdn.invalid/base/"),
            true,
        );
        let dash = with_manifest
            .dash_request_material()
            .expect("MPD owns multi-period");
        assert_eq!(dash.input().kind(), YtDlpDashInputKind::Manifest);
        assert_eq!(
            dash.input().manifest_url_for_fetch(),
            Some("https://cdn.invalid/authoritative.mpd")
        );
        assert_eq!(dash.input().fragments().len(), 0);

        let without_manifest = request(
            Some("https://cdn.invalid/selected.mpd"),
            None,
            fragments,
            Some("https://cdn.invalid/base/"),
            true,
        );
        assert_eq!(
            without_manifest.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::MultiPeriodRequiresManifest
        );
    }

    #[test]
    fn relative_fragment_requires_base_and_empty_input_uses_manifest_precedence() {
        let missing_base = request(
            None,
            None,
            vec![fragment(None, Some("relative.m4s"), None, None)],
            None,
            false,
        );
        assert_eq!(
            missing_base.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::RelativeFragmentMissingBase
        );

        let manifest = request(
            Some("https://cdn.invalid/selected.mpd"),
            Some("https://cdn.invalid/upstream.mpd"),
            Vec::new(),
            None,
            false,
        );
        assert_eq!(
            manifest
                .dash_request_material()
                .expect("manifest")
                .input()
                .manifest_url_for_fetch(),
            Some("https://cdn.invalid/upstream.mpd")
        );

        let relative_url_field = request(
            None,
            None,
            vec![fragment(
                Some("relative-in-url-field.m4s"),
                None,
                None,
                None,
            )],
            None,
            false,
        );
        assert_eq!(
            relative_url_field.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::InvalidFragmentLocator
        );

        let invalid_base = request(
            None,
            None,
            vec![fragment(None, Some("relative.m4s"), None, None)],
            Some("relative-base/"),
            false,
        );
        assert_eq!(
            invalid_base.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::InvalidFragmentBaseUrl
        );

        let invalid_manifest = request(Some("relative.mpd"), None, Vec::new(), None, false);
        assert_eq!(
            invalid_manifest.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::InvalidManifestUrl
        );

        let root_relative = request(
            None,
            None,
            vec![
                fragment(None, Some("/init.webm"), None, None),
                fragment(None, Some("/root-relative.webm"), Some(1.0), None),
            ],
            Some("https://cdn.invalid/base/"),
            false,
        );
        root_relative
            .dash_request_material()
            .expect("root-relative reference сохраняет origin base URL");

        for unsafe_path in [
            "//evil.invalid/network-path.m4s",
            "//fastiplayer.invalid/same-origin-network-path.m4s",
            r"\\evil.invalid\backslash-network-path.m4s",
            "https://evil.invalid/cross-origin.m4s",
            "http://[invalid-ipv6",
        ] {
            let unsafe_request = request(
                None,
                None,
                vec![fragment(None, Some(unsafe_path), None, None)],
                Some("https://cdn.invalid/base/"),
                false,
            );
            assert_eq!(
                unsafe_request.dash_request_material().unwrap_err(),
                YtDlpDashRequestMaterialViolation::InvalidFragmentLocator
            );
        }
    }

    #[test]
    fn serialized_fragment_roles_require_one_init_then_positive_media_durations() {
        let leading_media = request(
            None,
            None,
            vec![
                fragment(None, Some("first.m4s"), Some(1.0), None),
                fragment(None, Some("second.m4s"), Some(1.0), None),
            ],
            Some("https://cdn.invalid/base/"),
            false,
        );
        assert_eq!(
            leading_media.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::MissingLeadingInitialization
        );

        let init_only = request(
            None,
            None,
            vec![fragment(None, Some("init.mp4"), None, None)],
            Some("https://cdn.invalid/base/"),
            false,
        );
        assert_eq!(
            init_only.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::MissingMediaFragments
        );

        let second_missing_duration = request(
            None,
            None,
            vec![
                fragment(None, Some("init.webm"), None, None),
                fragment(None, Some("ambiguous.webm"), None, None),
            ],
            Some("https://cdn.invalid/base/"),
            false,
        );
        assert_eq!(
            second_missing_duration.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::AmbiguousFragmentRole
        );
    }

    #[test]
    fn generator_shape_and_invalid_duration_or_filesize_are_rejected_during_normalization() {
        assert_eq!(
            normalize_fragments(Some(&json!("generator repr"))).err(),
            Some(YtDlpRequestMaterialViolation::InvalidFragments)
        );
        assert_eq!(
            normalize_fragments(Some(&json!([{"path":"a.m4s","duration":-1.0}]))).err(),
            Some(YtDlpRequestMaterialViolation::InvalidFragments)
        );
        assert_eq!(
            normalize_fragments(Some(&json!([{"path":"a.m4s","filesize":"large"}]))).err(),
            Some(YtDlpRequestMaterialViolation::InvalidFragments)
        );
        assert_eq!(
            normalize_fragments(Some(&json!([{"path":"a.m4s","duration":86401.0}]))).err(),
            Some(YtDlpRequestMaterialViolation::InvalidFragments)
        );
        assert_eq!(
            normalize_fragments(Some(&json!([{"path":"a.m4s","filesize":68719476737_u64}]))).err(),
            Some(YtDlpRequestMaterialViolation::InvalidFragments)
        );
    }

    #[test]
    fn raw_is_dash_periods_mapping_and_debug_are_secret_safe() {
        let raw = YtDlpSerializedFormat {
            url: Some("https://cdn.invalid/selected.mpd?token=raw".to_owned()),
            manifest_url: Some("https://cdn.invalid/manifest.mpd?token=raw".to_owned()),
            is_dash_periods: Some(true),
            ..YtDlpSerializedFormat::default()
        };
        let normalized = normalize_request_material(&raw).expect("valid raw request");
        let YtDlpRequestMaterialSummary {
            is_dash_periods, ..
        } = normalized.summary();
        assert!(is_dash_periods);
        let debug = format!("{normalized:?}");
        assert!(!debug.contains("cdn.invalid"));
        assert!(!debug.contains("token=raw"));
        assert!(debug.contains("is_dash_periods: true"));
    }
}
