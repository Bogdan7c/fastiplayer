use std::collections::BTreeMap;

use serde_json::{Map, Value};
use web_media_transport_api::HttpRangeRequestLimit;

use super::cookies::normalize_yt_dlp_cookies;
use super::{
    MAX_HTTP_RANGE_REQUEST_BYTES, MAX_INLINE_HLS_UTF8_BYTES, MAX_REQUEST_FRAGMENT_BYTE_LENGTH,
    MAX_REQUEST_FRAGMENT_DURATION_SECONDS, MAX_REQUEST_FRAGMENTS, MAX_REQUEST_HEADERS,
    MAX_REQUEST_SECRET_UTF8_BYTES, MAX_RTMP_CONNECTION_ARGUMENTS, SecretText, YtDlpHlsAesMaterial,
    YtDlpRequestFragment, YtDlpRequestMaterial, YtDlpRequestMaterialV1,
    YtDlpRequestMaterialViolation, YtDlpRtmpRequestMaterial,
};
use crate::candidate::raw::YtDlpSerializedFormat;

/// Валидирует все S00 request-material fields одной format row.
pub(in crate::candidate) fn normalize_request_material(
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
        cookies: normalize_yt_dlp_cookies(format.cookies.as_ref())?,
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
    if format.bunnycdn_ping_data.is_some() || format.cookie_refresh_params.is_some() {
        return Err(YtDlpRequestMaterialViolation::PrivateExtractorStateRequired);
    }
    Ok(())
}

/// Выделяет только безопасный declarative `http_chunk_size` из downloader options.
///
/// Любой иной ключ остаётся fail-closed: Fastiplayer не исполняет downloader
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
pub(super) fn normalize_fragments(
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
