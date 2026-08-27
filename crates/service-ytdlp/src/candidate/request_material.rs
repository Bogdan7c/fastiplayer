use std::collections::BTreeMap;
use std::fmt;

use source_core::{FtpRequestTarget, HttpRequestTarget};
use web_media_transport_api::HttpRangeRequestLimit;
use zeroize::Zeroizing;

mod cookies;
mod dash;
mod hds;
mod hls;
mod normalization;
mod smooth;

use cookies::YtDlpCookieMaterial;
pub(super) use cookies::YtDlpCookieMaterialRef;
pub use dash::{
    YtDlpDashFragment, YtDlpDashFragmentLocatorKind, YtDlpDashFragmentRole, YtDlpDashInput,
    YtDlpDashInputKind, YtDlpDashRequestContext, YtDlpDashRequestMaterial,
    YtDlpDashRequestMaterialViolation,
};
pub use hds::{YtDlpHdsManifestRequestMaterial, YtDlpHdsManifestRequestMaterialViolation};
pub use hls::{
    YtDlpHlsAesOverride, YtDlpHlsManifestInput, YtDlpHlsManifestInputKind, YtDlpHlsRequestMaterial,
    YtDlpHlsRequestMaterialViolation,
};
#[cfg(test)]
use normalization::normalize_fragments;
pub(super) use normalization::normalize_request_material;
pub use smooth::{
    YtDlpSmoothManifestRequestMaterial, YtDlpSmoothManifestRequestMaterialViolation,
    YtDlpSmoothUnsupportedRequestMaterial,
};

/// Версия service-owned schema transient request material.
pub const YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION: u16 = 1;

/// Максимальное число concrete fragments одной format row.
const MAX_REQUEST_FRAGMENTS: usize = 10_000;
/// Максимальная serialized duration одного fragment-а (24 часа).
const MAX_REQUEST_FRAGMENT_DURATION_SECONDS: f64 = 24.0 * 60.0 * 60.0;
/// Максимальная serialized filesize одного fragment-а (64 GiB).
const MAX_REQUEST_FRAGMENT_BYTE_LENGTH: u64 = 64 * 1024 * 1024 * 1024;
/// Максимальное число transient HTTP headers.
const MAX_REQUEST_HEADERS: usize = 128;
/// Максимальная длина secret/request строки.
const MAX_REQUEST_SECRET_UTF8_BYTES: usize = 65_536;
/// Максимальный размер inline HLS manifest.
const MAX_INLINE_HLS_UTF8_BYTES: usize = 2 * 1024 * 1024;
/// Максимальное число RTMP connection arguments.
const MAX_RTMP_CONNECTION_ARGUMENTS: usize = 128;
/// Максимальный extractor-provided предел одного HTTP Range-запроса.
const MAX_HTTP_RANGE_REQUEST_BYTES: u64 = 64 * 1024 * 1024;

/// Versioned transient request material одного format component-а.
///
/// Поля намеренно закрыты: будущий transport owner получит отдельные intent
/// accessors, а UI/diagnostics уже сейчас может видеть только safe summary.
#[derive(Clone, PartialEq)]
pub enum YtDlpRequestMaterial {
    /// Schema, соответствующая S00 profile v1.
    V1(YtDlpRequestMaterialV1),
}

impl YtDlpRequestMaterial {
    /// Возвращает numeric schema version для explicit handoff-а.
    pub const fn schema_version(&self) -> u16 {
        match self {
            Self::V1(_) => YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION,
        }
    }

    /// Возвращает только non-secret shape request material.
    pub fn summary(&self) -> YtDlpRequestMaterialSummary {
        match self {
            Self::V1(material) => material.summary(),
        }
    }
}

impl fmt::Debug for YtDlpRequestMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpRequestMaterial")
            .field("schema_version", &self.schema_version())
            .field("summary", &self.summary())
            .finish_non_exhaustive()
    }
}

/// Opaque S00 request-material schema v1.
#[derive(Clone, PartialEq)]
pub struct YtDlpRequestMaterialV1 {
    /// Effective endpoint.
    url: Option<SecretText>,
    /// Parent manifest endpoint.
    manifest_url: Option<SecretText>,
    /// Concrete serialized fragments.
    fragments: Box<[YtDlpRequestFragment]>,
    /// Base endpoint для relative fragments.
    fragment_base_url: Option<SecretText>,
    /// Upstream marker: fragments пересекают DASH Period boundaries.
    is_dash_periods: bool,
    /// Inline HLS media playlist.
    hls_media_playlist_data: Option<SecretText>,
    /// Transient headers.
    http_headers: BTreeMap<String, SecretText>,
    /// Нейтральный source-specific предел одного HTTP Range-запроса.
    http_range_request_limit: Option<HttpRangeRequestLimit>,
    /// Request-header либо scoped response-style cookies без смешения semantics.
    cookies: Option<YtDlpCookieMaterial>,
    /// Segment query material.
    extra_param_to_segment_url: Option<SecretText>,
    /// Key query material.
    extra_param_to_key_url: Option<SecretText>,
    /// Extractor-provided AES material.
    hls_aes: Option<YtDlpHlsAesMaterial>,
    /// Declarative RTMP request material.
    rtmp: Option<YtDlpRtmpRequestMaterial>,
}

impl YtDlpRequestMaterialV1 {
    /// Строит safe summary без secret values.
    fn summary(&self) -> YtDlpRequestMaterialSummary {
        YtDlpRequestMaterialSummary {
            has_url: self.url.is_some(),
            has_manifest_url: self.manifest_url.is_some(),
            fragment_count: self.fragments.len(),
            has_fragment_base_url: self.fragment_base_url.is_some(),
            is_dash_periods: self.is_dash_periods,
            has_inline_hls: self.hls_media_playlist_data.is_some(),
            header_count: self.http_headers.len(),
            http_range_request_limit_bytes: self
                .http_range_request_limit
                .map(HttpRangeRequestLimit::maximum_bytes),
            has_cookies: self.cookies.is_some(),
            has_segment_query: self.extra_param_to_segment_url.is_some(),
            has_key_query: self.extra_param_to_key_url.is_some(),
            has_hls_aes: self.hls_aes.is_some(),
            has_rtmp_material: self.rtmp.is_some(),
        }
    }
}

impl fmt::Debug for YtDlpRequestMaterialV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpRequestMaterialV1")
            .field("summary", &self.summary())
            .finish_non_exhaustive()
    }
}

/// Non-secret inventory request material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YtDlpRequestMaterialSummary {
    /// Effective endpoint присутствует.
    pub has_url: bool,
    /// Manifest endpoint присутствует.
    pub has_manifest_url: bool,
    /// Число concrete fragments.
    pub fragment_count: usize,
    /// Fragment base присутствует.
    pub has_fragment_base_url: bool,
    /// Upstream сообщил multi-period DASH serialization.
    pub is_dash_periods: bool,
    /// Inline HLS manifest присутствует.
    pub has_inline_hls: bool,
    /// Число transient headers.
    pub header_count: usize,
    /// Optional верхняя граница одного HTTP Range-запроса в bytes.
    pub http_range_request_limit_bytes: Option<u64>,
    /// Scoped cookies присутствуют.
    pub has_cookies: bool,
    /// Segment query material присутствует.
    pub has_segment_query: bool,
    /// Key query material присутствует.
    pub has_key_query: bool,
    /// HLS AES material присутствует.
    pub has_hls_aes: bool,
    /// RTMP declarative fields присутствуют.
    pub has_rtmp_material: bool,
}

/// Typed причина отказа request-material normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpRequestMaterialViolation {
    /// Fragments не являются bounded concrete array.
    #[error("invalid fragment list")]
    InvalidFragments,
    /// Fragment count превысил internal safety cap.
    #[error("fragment count exceeds the safety limit")]
    TooManyFragments,
    /// HTTP headers имеют неподдерживаемую форму или размер.
    #[error("invalid HTTP headers")]
    InvalidHttpHeaders,
    /// Cookies имеют неподдерживаемую форму или размер.
    #[error("invalid cookies")]
    InvalidCookies,
    /// `request_data` требуется строке, но исключён S00 profile.
    #[error("request_data is required")]
    RequestDataRequired,
    /// Candidate требует browser impersonation provider-а.
    #[error("browser impersonation is required")]
    ImpersonationRequired,
    /// Candidate зависит от internal downloader state.
    #[error("private downloader state is required")]
    DownloaderStateRequired,
    /// `downloader_options.http_chunk_size` не является bounded positive integer.
    #[error("invalid HTTP chunk size")]
    InvalidHttpChunkSize,
    /// Candidate зависит от pinned private extractor state.
    #[error("private extractor state is required")]
    PrivateExtractorStateRequired,
    /// HLS AES shape нельзя доказанно интерпретировать как v1 material.
    #[error("invalid HLS AES material")]
    InvalidHlsAes,
    /// RTMP connection arguments имеют неподдерживаемую форму.
    #[error("invalid RTMP connection arguments")]
    InvalidRtmpConnectionArguments,
    /// Secret/request строка превысила named bound.
    #[error("request field exceeds the safety limit")]
    RequestFieldTooLong,
    /// Cookie одновременно пришёл отдельным field и обычным HTTP header-ом.
    #[error("cookie authorization material has conflicting serializations")]
    ConflictingCookieMaterial,
    /// Progressive S22 provider не владеет segment/manifest/RTMP material.
    #[error("request material does not belong to progressive HTTP")]
    NonProgressiveMaterial,
    /// Progressive HTTP subset не содержит HTTP(S) primary target.
    #[error("progressive request target is not HTTP(S)")]
    NonHttpProgressiveMaterial,
    /// Progressive FTP subset не содержит FTP(S) primary target.
    #[error("progressive request target is not FTP(S)")]
    NonFtpProgressiveMaterial,
    /// Progressive FTP не принимает stateful или нестандартный HTTP material.
    #[error("request material contains non-ambient HTTP fields incompatible with progressive FTP")]
    HttpOnlyMaterialForFtp,
    /// Progressive resource не содержит primary URL.
    #[error("progressive request has no primary URL")]
    MissingPrimaryUrl,
}

/// Secret string без plaintext `Debug`/`Display`.
#[derive(Clone, PartialEq)]
struct SecretText(Zeroizing<String>);

impl SecretText {
    /// Проверяет field-specific byte cap и сохраняет exact string.
    fn bounded(exact: String, maximum_bytes: usize) -> Result<Self, YtDlpRequestMaterialViolation> {
        if exact.len() > maximum_bytes {
            return Err(YtDlpRequestMaterialViolation::RequestFieldTooLong);
        }
        Ok(Self(Zeroizing::new(exact)))
    }

    /// Передаёт exact secret только transport adapter-у после owner-side checks.
    pub(super) fn expose_secret_for_transport(&self) -> &str {
        &self.0
    }
}

/// Заимствованный progressive FTP subset с primary target без HTTP auth surface.
pub(super) struct YtDlpProgressiveFtpRequestMaterial<'a> {
    /// Проверенный S19 material owner, который не раскрывается за пределы adapter-а.
    #[allow(dead_code)]
    material: &'a YtDlpRequestMaterialV1,
    /// Primary FTP target, наличие и scheme которого доказал constructor.
    target: &'a SecretText,
}

impl YtDlpProgressiveFtpRequestMaterial<'_> {
    /// Раскрывает primary target только transport request constructor-у.
    pub(super) fn target(&self) -> &str {
        self.target.expose_secret_for_transport()
    }
}

/// Заимствованный progressive subset с effective serialized auth state.
pub(super) struct YtDlpProgressiveHttpRequestMaterial<'a> {
    /// Проверенный S19 material owner, который не раскрывается за пределы adapter-а.
    material: &'a YtDlpRequestMaterialV1,
    /// Primary target, наличие которого доказал constructor.
    target: &'a SecretText,
    /// Единственная effective Cookie форма после conflict checks.
    cookies: Option<YtDlpCookieMaterialRef<'a>>,
}

/// Общая HTTP authorization projection без progressive/HLS profile guessing.
pub(super) struct YtDlpHttpAuthorizationMaterial<'a> {
    material: &'a YtDlpRequestMaterialV1,
    cookies: Option<YtDlpCookieMaterialRef<'a>>,
}

impl<'material> YtDlpHttpAuthorizationMaterial<'material> {
    pub(super) fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.material
            .http_headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    /// Возвращает request-header либо scoped-seed форму без flattening.
    pub(super) const fn cookies(&self) -> Option<YtDlpCookieMaterialRef<'material>> {
        self.cookies
    }
}

impl<'material> YtDlpProgressiveHttpRequestMaterial<'material> {
    /// Раскрывает primary target только transport request constructor-у.
    pub(super) fn target(&self) -> &str {
        self.target.expose_secret_for_transport()
    }

    /// Итерирует effective headers, исключая Cookie с отдельной typed boundary.
    pub(super) fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.material
            .http_headers
            .iter()
            .filter(|(name, _value)| !name.eq_ignore_ascii_case("cookie"))
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    /// Возвращает единственную доказанную Cookie форму.
    pub(super) const fn cookies(&self) -> Option<YtDlpCookieMaterialRef<'material>> {
        self.cookies
    }

    /// Возвращает проверенный source-specific предел одного HTTP Range-запроса.
    pub(super) const fn http_range_request_limit(&self) -> Option<HttpRangeRequestLimit> {
        self.material.http_range_request_limit
    }
}

impl YtDlpRequestMaterial {
    /// Проверяет только общие headers/cookies, не смешивая transport profile semantics.
    pub(super) fn http_authorization_material(
        &self,
    ) -> Result<YtDlpHttpAuthorizationMaterial<'_>, YtDlpRequestMaterialViolation> {
        let Self::V1(material) = self;
        let mut cookie_headers = material
            .http_headers
            .iter()
            .filter(|(name, _value)| name.eq_ignore_ascii_case("cookie"))
            .map(|(_name, value)| value);
        let cookie_header = cookie_headers.next();
        if cookie_headers.next().is_some() {
            return Err(YtDlpRequestMaterialViolation::ConflictingCookieMaterial);
        }
        let cookies = match (cookie_header, material.cookies.as_ref()) {
            (None, None) => None,
            (Some(header), None) => Some(YtDlpCookieMaterialRef::RequestHeader(
                header.expose_secret_for_transport(),
            )),
            (None, Some(field)) => Some(field.as_ref()),
            (Some(header), Some(YtDlpCookieMaterial::RequestHeader(field))) if header == field => {
                Some(YtDlpCookieMaterialRef::RequestHeader(
                    header.expose_secret_for_transport(),
                ))
            }
            (Some(_), Some(_)) => {
                return Err(YtDlpRequestMaterialViolation::ConflictingCookieMaterial);
            }
        };
        Ok(YtDlpHttpAuthorizationMaterial { material, cookies })
    }

    /// Доказывает progressive HTTP subset и возвращает effective serialized auth.
    pub(super) fn progressive_http_request_material(
        &self,
    ) -> Result<YtDlpProgressiveHttpRequestMaterial<'_>, YtDlpRequestMaterialViolation> {
        let Self::V1(material) = self;
        let authorization = self.http_authorization_material()?;
        ensure_progressive_single_url_subset(material)?;
        let target = material
            .url
            .as_ref()
            .ok_or(YtDlpRequestMaterialViolation::MissingPrimaryUrl)?;
        if HttpRequestTarget::parse_exact(target.expose_secret_for_transport()).is_err() {
            return Err(YtDlpRequestMaterialViolation::NonHttpProgressiveMaterial);
        }
        Ok(YtDlpProgressiveHttpRequestMaterial {
            material,
            target,
            cookies: authorization.cookies,
        })
    }

    /// Доказывает progressive FTP subset без значимого HTTP authorization/range material.
    pub(super) fn progressive_ftp_request_material(
        &self,
    ) -> Result<YtDlpProgressiveFtpRequestMaterial<'_>, YtDlpRequestMaterialViolation> {
        let Self::V1(material) = self;
        ensure_progressive_single_url_subset(material)?;
        if material
            .http_headers
            .keys()
            .any(|header_name| !is_ignorable_ftp_http_header(header_name))
            || material.cookies.is_some()
            || material.http_range_request_limit.is_some()
        {
            return Err(YtDlpRequestMaterialViolation::HttpOnlyMaterialForFtp);
        }
        let target = material
            .url
            .as_ref()
            .ok_or(YtDlpRequestMaterialViolation::MissingPrimaryUrl)?;
        if FtpRequestTarget::parse_exact(target.expose_secret_for_transport()).is_err() {
            return Err(YtDlpRequestMaterialViolation::NonFtpProgressiveMaterial);
        }
        Ok(YtDlpProgressiveFtpRequestMaterial { material, target })
    }

    /// Доказывает exact pinned native-HLS request-material subset.
    pub fn hls_request_material(
        &self,
    ) -> Result<YtDlpHlsRequestMaterial<'_>, YtDlpHlsRequestMaterialViolation> {
        hls::hls_request_material(self)
    }

    /// Проецирует DASH-only manifest/fragments/request context.
    pub fn dash_request_material(
        &self,
    ) -> Result<YtDlpDashRequestMaterial<'_>, YtDlpDashRequestMaterialViolation> {
        dash::dash_request_material(self)
    }

    /// Проецирует manifest-only material для approved HDS F4M/F4F VOD profile.
    pub fn hds_manifest_request_material(
        &self,
    ) -> Result<YtDlpHdsManifestRequestMaterial<'_>, YtDlpHdsManifestRequestMaterialViolation> {
        hds::hds_manifest_request_material(self)
    }

    /// Проецирует exact manifest-only material для approved ISM VOD profile.
    pub fn smooth_manifest_request_material(
        &self,
    ) -> Result<YtDlpSmoothManifestRequestMaterial<'_>, YtDlpSmoothManifestRequestMaterialViolation>
    {
        smooth::smooth_manifest_request_material(self)
    }
}

/// Отличает безусловные browser-navigation hints yt-dlp от значимого HTTP state.
fn is_ignorable_ftp_http_header(header_name: &str) -> bool {
    [
        "accept",
        "accept-encoding",
        "accept-language",
        "sec-fetch-mode",
        "user-agent",
    ]
    .iter()
    .any(|ambient_name| header_name.eq_ignore_ascii_case(ambient_name))
}

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretText")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Concrete fragment без публичного secret surface.
#[derive(Clone, PartialEq)]
struct YtDlpRequestFragment {
    /// Absolute fragment endpoint, если он сериализован.
    url: Option<SecretText>,
    /// Relative fragment path, если он сериализован.
    path: Option<SecretText>,
    /// Optional finite duration.
    duration_seconds: Option<f64>,
    /// Optional byte length.
    byte_length: Option<u64>,
}

/// AES values остаются opaque до S32 crypto validation.
#[derive(Clone, PartialEq)]
struct YtDlpHlsAesMaterial {
    /// Optional key endpoint.
    uri: Option<SecretText>,
    /// Optional serialized key material.
    key: Option<SecretText>,
    /// Optional serialized IV material.
    iv: Option<SecretText>,
}

/// Declarative RTMP request fields из S00.
#[derive(Clone, PartialEq)]
struct YtDlpRtmpRequestMaterial {
    /// Page endpoint.
    page_url: Option<SecretText>,
    /// Application identity.
    app: Option<SecretText>,
    /// Play path.
    play_path: Option<SecretText>,
    /// tcUrl.
    tc_url: Option<SecretText>,
    /// Flash version identity.
    flash_version: Option<SecretText>,
    /// Live flag.
    live: Option<bool>,
    /// Connection arguments.
    connection_arguments: Box<[SecretText]>,
    /// Exact protocol identity.
    protocol: Option<SecretText>,
    /// Real-time flag.
    real_time: Option<bool>,
}

/// Доказывает single-URL progressive subset без adaptive/RTMP extras.
fn ensure_progressive_single_url_subset(
    material: &YtDlpRequestMaterialV1,
) -> Result<(), YtDlpRequestMaterialViolation> {
    if !material.fragments.is_empty()
        || material.fragment_base_url.is_some()
        || material.hls_media_playlist_data.is_some()
        || material.extra_param_to_segment_url.is_some()
        || material.extra_param_to_key_url.is_some()
        || material.hls_aes.is_some()
        || material.rtmp.is_some()
    {
        return Err(YtDlpRequestMaterialViolation::NonProgressiveMaterial);
    }
    Ok(())
}
