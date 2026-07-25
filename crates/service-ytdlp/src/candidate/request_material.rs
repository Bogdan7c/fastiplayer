use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};
use source_core::{FtpRequestTarget, HttpRequestTarget};
use web_media_transport_api::HttpRangeRequestLimit;
use zeroize::Zeroizing;

use super::raw::YtDlpSerializedFormat;

mod dash;
mod hds;
mod hls;
mod smooth;

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
    /// Scoped cookies.
    cookies: Option<SecretText>,
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
    /// Progressive FTP не принимает HTTP authorization/range material.
    #[error("request material contains HTTP-only fields incompatible with progressive FTP")]
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
    /// Единственная effective Cookie serialization после conflict checks.
    serialized_cookies: Option<&'a SecretText>,
}

/// Общая HTTP authorization projection без progressive/HLS profile guessing.
pub(super) struct YtDlpHttpAuthorizationMaterial<'a> {
    material: &'a YtDlpRequestMaterialV1,
    serialized_cookies: Option<&'a SecretText>,
}

impl YtDlpHttpAuthorizationMaterial<'_> {
    pub(super) fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.material
            .http_headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    pub(super) fn serialized_cookies(&self) -> Option<&str> {
        self.serialized_cookies
            .map(SecretText::expose_secret_for_transport)
    }
}

impl YtDlpProgressiveHttpRequestMaterial<'_> {
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

    /// Возвращает единственную доказанную serialized Cookie форму.
    pub(super) fn serialized_cookies(&self) -> Option<&str> {
        self.serialized_cookies
            .map(SecretText::expose_secret_for_transport)
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
        if cookie_headers.next().is_some()
            || matches!(
                (cookie_header, material.cookies.as_ref()),
                (Some(header), Some(field)) if header != field
            )
        {
            return Err(YtDlpRequestMaterialViolation::ConflictingCookieMaterial);
        }
        Ok(YtDlpHttpAuthorizationMaterial {
            material,
            serialized_cookies: material.cookies.as_ref().or(cookie_header),
        })
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
            serialized_cookies: authorization.serialized_cookies,
        })
    }

    /// Доказывает progressive FTP subset без HTTP authorization/range material.
    pub(super) fn progressive_ftp_request_material(
        &self,
    ) -> Result<YtDlpProgressiveFtpRequestMaterial<'_>, YtDlpRequestMaterialViolation> {
        let Self::V1(material) = self;
        ensure_progressive_single_url_subset(material)?;
        if !material.http_headers.is_empty()
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

/// Валидирует все S00 request-material fields одной format row.
pub(super) fn normalize_request_material(
    format: &YtDlpSerializedFormat,
) -> Result<YtDlpRequestMaterial, YtDlpRequestMaterialViolation> {
    reject_excluded_material(format)?;

    let material = YtDlpRequestMaterialV1 {
        url: optional_secret(format.url.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        manifest_url: optional_secret(format.manifest_url.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        fragments: normalize_fragments(format.fragments.as_ref())?,
        fragment_base_url: optional_secret(
            format.fragment_base_url.clone(),
            MAX_REQUEST_SECRET_UTF8_BYTES,
        )?,
        is_dash_periods: format.is_dash_periods.unwrap_or(false),
        hls_media_playlist_data: optional_secret(
            format.hls_media_playlist_data.clone(),
            MAX_INLINE_HLS_UTF8_BYTES,
        )?,
        http_headers: normalize_headers(format.http_headers.as_ref())?,
        http_range_request_limit: normalize_http_range_request_limit(
            format.downloader_options.as_ref(),
        )?,
        cookies: normalize_cookies(format.cookies.as_ref())?,
        extra_param_to_segment_url: optional_secret(
            format.extra_param_to_segment_url.clone(),
            MAX_REQUEST_SECRET_UTF8_BYTES,
        )?,
        extra_param_to_key_url: optional_secret(
            format.extra_param_to_key_url.clone(),
            MAX_REQUEST_SECRET_UTF8_BYTES,
        )?,
        hls_aes: normalize_hls_aes(format.hls_aes.as_ref())?,
        rtmp: normalize_rtmp_material(format)?,
    };

    Ok(YtDlpRequestMaterial::V1(material))
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

/// Fail-closed классифицирует поля, которые S00 не умеет воспроизвести.
fn reject_excluded_material(
    format: &YtDlpSerializedFormat,
) -> Result<(), YtDlpRequestMaterialViolation> {
    if format.request_data.is_some() {
        return Err(YtDlpRequestMaterialViolation::RequestDataRequired);
    }
    if format.impersonate.is_some() {
        return Err(YtDlpRequestMaterialViolation::ImpersonationRequired);
    }
    if format.bunnycdn_ping_data.is_some() || format.cookie_refresh_params.is_some() {
        return Err(YtDlpRequestMaterialViolation::PrivateExtractorStateRequired);
    }
    Ok(())
}

/// Выделяет только безопасный declarative `http_chunk_size` из downloader options.
///
/// Любой иной ключ остаётся fail-closed: Rustiplayer не исполняет downloader
/// state и не пытается угадывать семантику будущих yt-dlp options.
fn normalize_http_range_request_limit(
    raw_downloader_options: Option<&Value>,
) -> Result<Option<HttpRangeRequestLimit>, YtDlpRequestMaterialViolation> {
    let Some(raw_downloader_options) = raw_downloader_options else {
        return Ok(None);
    };
    let Some(downloader_options) = raw_downloader_options.as_object() else {
        return Err(YtDlpRequestMaterialViolation::DownloaderStateRequired);
    };
    if downloader_options.is_empty() {
        return Ok(None);
    }
    if downloader_options.len() != 1 || !downloader_options.contains_key("http_chunk_size") {
        return Err(YtDlpRequestMaterialViolation::DownloaderStateRequired);
    }
    let maximum_bytes = downloader_options["http_chunk_size"]
        .as_u64()
        .filter(|maximum_bytes| *maximum_bytes <= MAX_HTTP_RANGE_REQUEST_BYTES)
        .ok_or(YtDlpRequestMaterialViolation::InvalidHttpChunkSize)?;
    HttpRangeRequestLimit::new(maximum_bytes)
        .map(Some)
        .map_err(|_| YtDlpRequestMaterialViolation::InvalidHttpChunkSize)
}

/// Нормализует optional secret string с named cap.
fn optional_secret(
    exact: Option<String>,
    maximum_bytes: usize,
) -> Result<Option<SecretText>, YtDlpRequestMaterialViolation> {
    exact
        .map(|value| SecretText::bounded(value, maximum_bytes))
        .transpose()
}

/// Материализует только concrete fragment array.
fn normalize_fragments(
    raw_fragments: Option<&Value>,
) -> Result<Box<[YtDlpRequestFragment]>, YtDlpRequestMaterialViolation> {
    let Some(raw_fragments) = raw_fragments else {
        return Ok(Box::new([]));
    };
    let Some(fragments) = raw_fragments.as_array() else {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    };
    if fragments.len() > MAX_REQUEST_FRAGMENTS {
        return Err(YtDlpRequestMaterialViolation::TooManyFragments);
    }

    fragments
        .iter()
        .map(normalize_fragment)
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

/// Проверяет один fragment без раскрытия locator-а в ошибке.
fn normalize_fragment(
    raw_fragment: &Value,
) -> Result<YtDlpRequestFragment, YtDlpRequestMaterialViolation> {
    let Some(fragment) = raw_fragment.as_object() else {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    };
    let url = optional_object_secret(
        fragment,
        "url",
        YtDlpRequestMaterialViolation::InvalidFragments,
    )?;
    let path = optional_object_secret(
        fragment,
        "path",
        YtDlpRequestMaterialViolation::InvalidFragments,
    )?;
    if url.is_none() && path.is_none() {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    }
    let duration_seconds = optional_non_negative_f64(fragment, "duration")?;
    let byte_length = optional_u64(fragment, "filesize")?;
    if duration_seconds.is_some_and(|duration| duration > MAX_REQUEST_FRAGMENT_DURATION_SECONDS)
        || byte_length.is_some_and(|length| length > MAX_REQUEST_FRAGMENT_BYTE_LENGTH)
    {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    }

    Ok(YtDlpRequestFragment {
        url,
        path,
        duration_seconds,
        byte_length,
    })
}

/// Нормализует bounded string-to-string header map.
fn normalize_headers(
    raw_headers: Option<&Value>,
) -> Result<BTreeMap<String, SecretText>, YtDlpRequestMaterialViolation> {
    let Some(raw_headers) = raw_headers else {
        return Ok(BTreeMap::new());
    };
    let Some(headers) = raw_headers.as_object() else {
        return Err(YtDlpRequestMaterialViolation::InvalidHttpHeaders);
    };
    if headers.len() > MAX_REQUEST_HEADERS {
        return Err(YtDlpRequestMaterialViolation::InvalidHttpHeaders);
    }

    headers
        .iter()
        .map(|(name, raw_value)| {
            let Some(value) = raw_value.as_str() else {
                return Err(YtDlpRequestMaterialViolation::InvalidHttpHeaders);
            };
            if name.is_empty() || name.len() > MAX_REQUEST_SECRET_UTF8_BYTES {
                return Err(YtDlpRequestMaterialViolation::InvalidHttpHeaders);
            }
            Ok((
                name.clone(),
                SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
            ))
        })
        .collect()
}

/// Нормализует serialized scoped cookie string.
fn normalize_cookies(
    raw_cookies: Option<&Value>,
) -> Result<Option<SecretText>, YtDlpRequestMaterialViolation> {
    let Some(raw_cookies) = raw_cookies else {
        return Ok(None);
    };
    let Some(cookies) = raw_cookies.as_str() else {
        return Err(YtDlpRequestMaterialViolation::InvalidCookies);
    };
    SecretText::bounded(cookies.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
        .map(Some)
        .map_err(|_| YtDlpRequestMaterialViolation::InvalidCookies)
}

/// Проверяет ограниченный serialized HLS AES object.
fn normalize_hls_aes(
    raw_hls_aes: Option<&Value>,
) -> Result<Option<YtDlpHlsAesMaterial>, YtDlpRequestMaterialViolation> {
    let Some(raw_hls_aes) = raw_hls_aes else {
        return Ok(None);
    };
    let Some(hls_aes) = raw_hls_aes.as_object() else {
        return Err(YtDlpRequestMaterialViolation::InvalidHlsAes);
    };
    if hls_aes
        .keys()
        .any(|key| !matches!(key.as_str(), "uri" | "key" | "iv"))
    {
        return Err(YtDlpRequestMaterialViolation::InvalidHlsAes);
    }

    let material = YtDlpHlsAesMaterial {
        uri: optional_object_secret(hls_aes, "uri", YtDlpRequestMaterialViolation::InvalidHlsAes)?,
        key: optional_object_secret(hls_aes, "key", YtDlpRequestMaterialViolation::InvalidHlsAes)?,
        iv: optional_object_secret(hls_aes, "iv", YtDlpRequestMaterialViolation::InvalidHlsAes)?,
    };
    if material.uri.is_none() && material.key.is_none() && material.iv.is_none() {
        return Err(YtDlpRequestMaterialViolation::InvalidHlsAes);
    }
    Ok(Some(material))
}

/// Нормализует declarative RTMP fields без transport execution.
fn normalize_rtmp_material(
    format: &YtDlpSerializedFormat,
) -> Result<Option<YtDlpRtmpRequestMaterial>, YtDlpRequestMaterialViolation> {
    let has_rtmp_material = format.page_url.is_some()
        || format.app.is_some()
        || format.play_path.is_some()
        || format.tc_url.is_some()
        || format.flash_version.is_some()
        || format.rtmp_live.is_some()
        || format.rtmp_conn.is_some()
        || format.rtmp_protocol.is_some()
        || format.rtmp_real_time.is_some();
    if !has_rtmp_material {
        return Ok(None);
    }

    Ok(Some(YtDlpRtmpRequestMaterial {
        page_url: optional_secret(format.page_url.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        app: optional_secret(format.app.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        play_path: optional_secret(format.play_path.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        tc_url: optional_secret(format.tc_url.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        flash_version: optional_secret(
            format.flash_version.clone(),
            MAX_REQUEST_SECRET_UTF8_BYTES,
        )?,
        live: format.rtmp_live,
        connection_arguments: normalize_rtmp_connection_arguments(format.rtmp_conn.as_ref())?,
        protocol: optional_secret(format.rtmp_protocol.clone(), MAX_REQUEST_SECRET_UTF8_BYTES)?,
        real_time: format.rtmp_real_time,
    }))
}

/// Проверяет RTMP connection arguments как bounded string array.
fn normalize_rtmp_connection_arguments(
    raw_arguments: Option<&Value>,
) -> Result<Box<[SecretText]>, YtDlpRequestMaterialViolation> {
    let Some(raw_arguments) = raw_arguments else {
        return Ok(Box::new([]));
    };
    let Some(arguments) = raw_arguments.as_array() else {
        return Err(YtDlpRequestMaterialViolation::InvalidRtmpConnectionArguments);
    };
    if arguments.len() > MAX_RTMP_CONNECTION_ARGUMENTS {
        return Err(YtDlpRequestMaterialViolation::InvalidRtmpConnectionArguments);
    }

    arguments
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .ok_or(YtDlpRequestMaterialViolation::InvalidRtmpConnectionArguments)
                .and_then(|value| {
                    SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
                })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

/// Читает optional secret string из JSON object.
fn optional_object_secret(
    object: &Map<String, Value>,
    field: &str,
    invalid_shape: YtDlpRequestMaterialViolation,
) -> Result<Option<SecretText>, YtDlpRequestMaterialViolation> {
    let Some(raw_value) = object.get(field) else {
        return Ok(None);
    };
    let Some(value) = raw_value.as_str() else {
        return Err(invalid_shape);
    };
    SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
        .map(Some)
        .map_err(|_| invalid_shape)
}

/// Читает optional finite non-negative number.
fn optional_non_negative_f64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<f64>, YtDlpRequestMaterialViolation> {
    let Some(raw_value) = object.get(field) else {
        return Ok(None);
    };
    let Some(value) = raw_value.as_f64() else {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    };
    if !value.is_finite() || value < 0.0 {
        return Err(YtDlpRequestMaterialViolation::InvalidFragments);
    }
    Ok(Some(value))
}

/// Читает optional unsigned byte length.
fn optional_u64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u64>, YtDlpRequestMaterialViolation> {
    let Some(raw_value) = object.get(field) else {
        return Ok(None);
    };
    raw_value
        .as_u64()
        .map(Some)
        .ok_or(YtDlpRequestMaterialViolation::InvalidFragments)
}
