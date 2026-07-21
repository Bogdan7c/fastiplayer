use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

use super::raw::YtDlpSerializedFormat;

/// Версия service-owned schema transient request material.
pub const YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION: u16 = 1;

/// Максимальное число concrete fragments одной format row.
const MAX_REQUEST_FRAGMENTS: usize = 10_000;
/// Максимальное число transient HTTP headers.
const MAX_REQUEST_HEADERS: usize = 128;
/// Максимальная длина secret/request строки.
const MAX_REQUEST_SECRET_UTF8_BYTES: usize = 65_536;
/// Максимальный размер inline HLS manifest.
const MAX_INLINE_HLS_UTF8_BYTES: usize = 2 * 1024 * 1024;
/// Максимальное число RTMP connection arguments.
const MAX_RTMP_CONNECTION_ARGUMENTS: usize = 128;

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
    /// Inline HLS media playlist.
    hls_media_playlist_data: Option<SecretText>,
    /// Transient headers.
    http_headers: BTreeMap<String, SecretText>,
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
            has_inline_hls: self.hls_media_playlist_data.is_some(),
            header_count: self.http_headers.len(),
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
    /// Inline HLS manifest присутствует.
    pub has_inline_hls: bool,
    /// Число transient headers.
    pub header_count: usize,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpRequestMaterialViolation {
    /// Fragments не являются bounded concrete array.
    InvalidFragments,
    /// Fragment count превысил internal safety cap.
    TooManyFragments,
    /// HTTP headers имеют неподдерживаемую форму или размер.
    InvalidHttpHeaders,
    /// Cookies имеют неподдерживаемую форму или размер.
    InvalidCookies,
    /// `request_data` требуется строке, но исключён S00 profile.
    RequestDataRequired,
    /// Candidate требует browser impersonation provider-а.
    ImpersonationRequired,
    /// Candidate зависит от internal downloader state.
    DownloaderStateRequired,
    /// Candidate зависит от pinned private extractor state.
    PrivateExtractorStateRequired,
    /// HLS AES shape нельзя доказанно интерпретировать как v1 material.
    InvalidHlsAes,
    /// RTMP connection arguments имеют неподдерживаемую форму.
    InvalidRtmpConnectionArguments,
    /// Secret/request строка превысила named bound.
    RequestFieldTooLong,
}

/// Secret string без plaintext `Debug`/`Display`.
#[derive(Clone, PartialEq)]
struct SecretText(String);

impl SecretText {
    /// Проверяет field-specific byte cap и сохраняет exact string.
    fn bounded(exact: String, maximum_bytes: usize) -> Result<Self, YtDlpRequestMaterialViolation> {
        if exact.len() > maximum_bytes {
            return Err(YtDlpRequestMaterialViolation::RequestFieldTooLong);
        }
        Ok(Self(exact))
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
        hls_media_playlist_data: optional_secret(
            format.hls_media_playlist_data.clone(),
            MAX_INLINE_HLS_UTF8_BYTES,
        )?,
        http_headers: normalize_headers(format.http_headers.as_ref())?,
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
    if format.downloader_options.is_some() {
        return Err(YtDlpRequestMaterialViolation::DownloaderStateRequired);
    }
    if format.bunnycdn_ping_data.is_some() || format.cookie_refresh_params.is_some() {
        return Err(YtDlpRequestMaterialViolation::PrivateExtractorStateRequired);
    }
    Ok(())
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
