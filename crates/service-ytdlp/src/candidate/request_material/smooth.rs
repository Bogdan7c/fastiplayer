//! Service-owned projection serialized yt-dlp material-а в exact ISM manifest request.

use std::fmt;

use url::Url;

use super::{
    SecretText, YtDlpRequestMaterial, YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
};

/// Категория serialized material, которую ISM manifest projection обязана отклонить.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpSmoothUnsupportedRequestMaterial {
    /// Concrete fragments принадлежат fragment transport-у, а не manifest fetch.
    #[error("serialized fragments")]
    SerializedFragments,
    /// Relative fragment base не имеет смысла для единственного manifest request-а.
    #[error("fragment base URL")]
    FragmentBaseUrl,
    /// DASH Period marker не является ISM manifest semantics.
    #[error("DASH periods")]
    DashPeriods,
    /// Inline HLS playlist принадлежит HLS provider-у.
    #[error("inline HLS playlist")]
    InlineHls,
    /// Segment query override нельзя молча применить к ISM manifest.
    #[error("segment query override")]
    SegmentQueryOverride,
    /// Key query override принадлежит HLS key lifecycle.
    #[error("key query override")]
    KeyQueryOverride,
    /// HLS AES material не является ISM manifest authorization.
    #[error("HLS AES material")]
    HlsAes,
    /// RTMP fields принадлежат другому transport family.
    #[error("RTMP material")]
    Rtmp,
    /// Range limit не применяется к целому serialized ISM manifest fetch.
    #[error("HTTP Range request limit")]
    HttpRangeRequestLimit,
}

/// Typed отказ exact ISM manifest material projection до transport side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpSmoothManifestRequestMaterialViolation {
    /// Ни `url`, ни authoritative `manifest_url` не сериализованы.
    #[error("Smooth manifest request has no target")]
    MissingTarget,
    /// Одновременные `url` и `manifest_url` не совпадают byte-exact.
    #[error("Smooth manifest URL serializations are incompatible")]
    IncompatibleTargets,
    /// Target не является syntactically valid absolute hierarchical URL с host-ом.
    #[error("Smooth manifest target is malformed")]
    MalformedTarget,
    /// Target использует transport scheme вне exact HTTP(S) vocabulary.
    #[error("Smooth manifest target is not HTTP(S)")]
    NonHttpTarget,
    /// Serialized material принадлежит другому transport/runtime owner-у.
    #[error("Smooth manifest request contains unsupported material: {0}")]
    Unsupported(YtDlpSmoothUnsupportedRequestMaterial),
    /// Общая S26 authorization projection отклонила competing Cookie serialization.
    #[error("Smooth manifest authorization material is incompatible")]
    Authorization(#[source] YtDlpRequestMaterialViolation),
}

/// Borrowed exact ISM manifest request material без публичного secret storage surface.
pub struct YtDlpSmoothManifestRequestMaterial<'material> {
    /// Authoritative fetch target после exact URL/manifest reconciliation.
    target: &'material SecretText,
    /// Owner material нужен только для validated non-Cookie headers.
    material: &'material YtDlpRequestMaterialV1,
    /// Единственная effective Cookie serialization после S26 conflict checks.
    serialized_cookies: Option<&'material str>,
}

impl YtDlpSmoothManifestRequestMaterial<'_> {
    /// Раскрывает authoritative manifest target только concrete fetch projection-у.
    #[must_use]
    pub fn manifest_target_for_fetch(&self) -> &str {
        self.target.expose_secret_for_transport()
    }

    /// Итерирует validated headers, исключая Cookie из обычного header channel.
    pub(crate) fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.material
            .http_headers
            .iter()
            .filter(|(name, _value)| !name.eq_ignore_ascii_case("cookie"))
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    /// Возвращает единственную доказанную serialized Cookie форму.
    pub(crate) const fn serialized_cookies(&self) -> Option<&str> {
        self.serialized_cookies
    }
}

impl fmt::Debug for YtDlpSmoothManifestRequestMaterial<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpSmoothManifestRequestMaterial")
            .field("header_count", &self.headers().count())
            .field("has_serialized_cookies", &self.serialized_cookies.is_some())
            .finish_non_exhaustive()
    }
}

/// Доказывает exact serialized subset, которым владеет будущий ISM manifest provider.
pub(super) fn smooth_manifest_request_material(
    request: &YtDlpRequestMaterial,
) -> Result<YtDlpSmoothManifestRequestMaterial<'_>, YtDlpSmoothManifestRequestMaterialViolation> {
    let YtDlpRequestMaterial::V1(material) = request;
    let authorization = request
        .http_authorization_material()
        .map_err(YtDlpSmoothManifestRequestMaterialViolation::Authorization)?;
    reject_unsupported_material(material)?;
    let target = authoritative_manifest_target(material)?;
    validate_absolute_http_target(target)?;

    Ok(YtDlpSmoothManifestRequestMaterial {
        target,
        material,
        serialized_cookies: authorization
            .serialized_cookies
            .map(SecretText::expose_secret_for_transport),
    })
}

/// Возвращает authoritative manifest target без fallback между разными serializations.
fn authoritative_manifest_target(
    material: &YtDlpRequestMaterialV1,
) -> Result<&SecretText, YtDlpSmoothManifestRequestMaterialViolation> {
    match (material.url.as_ref(), material.manifest_url.as_ref()) {
        (None, None) => Err(YtDlpSmoothManifestRequestMaterialViolation::MissingTarget),
        (Some(url), None) => Ok(url),
        (None, Some(manifest_url)) => Ok(manifest_url),
        (Some(url), Some(manifest_url)) if url == manifest_url => Ok(manifest_url),
        (Some(_url), Some(_manifest_url)) => {
            Err(YtDlpSmoothManifestRequestMaterialViolation::IncompatibleTargets)
        }
    }
}

/// Отклоняет каждую material category, которой manifest-only projection не владеет.
fn reject_unsupported_material(
    material: &YtDlpRequestMaterialV1,
) -> Result<(), YtDlpSmoothManifestRequestMaterialViolation> {
    let unsupported = if !material.fragments.is_empty() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::SerializedFragments)
    } else if material.fragment_base_url.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::FragmentBaseUrl)
    } else if material.is_dash_periods {
        Some(YtDlpSmoothUnsupportedRequestMaterial::DashPeriods)
    } else if material.hls_media_playlist_data.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::InlineHls)
    } else if material.extra_param_to_segment_url.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::SegmentQueryOverride)
    } else if material.extra_param_to_key_url.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::KeyQueryOverride)
    } else if material.hls_aes.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::HlsAes)
    } else if material.rtmp.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::Rtmp)
    } else if material.http_range_request_limit.is_some() {
        Some(YtDlpSmoothUnsupportedRequestMaterial::HttpRangeRequestLimit)
    } else {
        None
    };

    match unsupported {
        Some(category) => Err(YtDlpSmoothManifestRequestMaterialViolation::Unsupported(
            category,
        )),
        None => Ok(()),
    }
}

/// Проверяет absolute hierarchical HTTP(S) target без reserialization identity.
fn validate_absolute_http_target(
    target: &SecretText,
) -> Result<(), YtDlpSmoothManifestRequestMaterialViolation> {
    let parsed = Url::parse(target.expose_secret_for_transport())
        .map_err(|_| YtDlpSmoothManifestRequestMaterialViolation::MalformedTarget)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(YtDlpSmoothManifestRequestMaterialViolation::NonHttpTarget);
    }
    if parsed.cannot_be_a_base() || parsed.host_str().is_none() {
        return Err(YtDlpSmoothManifestRequestMaterialViolation::MalformedTarget);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use web_media_transport_api::HttpRangeRequestLimit;

    use super::*;
    use crate::candidate::request_material::{
        YtDlpHlsAesMaterial, YtDlpRequestFragment, YtDlpRtmpRequestMaterial,
    };

    /// Создаёт bounded secret через тот же owner constructor, что production normalization.
    fn secret(exact: &str) -> SecretText {
        SecretText::bounded(exact.to_owned(), 65_536).expect("test secret должен быть bounded")
    }

    /// Строит минимальный manifest material с явными target serializations.
    fn request(url: Option<&str>, manifest_url: Option<&str>) -> YtDlpRequestMaterial {
        YtDlpRequestMaterial::V1(YtDlpRequestMaterialV1 {
            url: url.map(secret),
            manifest_url: manifest_url.map(secret),
            fragments: Box::new([]),
            fragment_base_url: None,
            is_dash_periods: false,
            hls_media_playlist_data: None,
            http_headers: BTreeMap::new(),
            http_range_request_limit: None,
            cookies: None,
            extra_param_to_segment_url: None,
            extra_param_to_key_url: None,
            hls_aes: None,
            rtmp: None,
        })
    }

    /// Возвращает mutable v1 owner для focused unsupported-field setup.
    fn v1_mut(request: &mut YtDlpRequestMaterial) -> &mut YtDlpRequestMaterialV1 {
        let YtDlpRequestMaterial::V1(material) = request;
        material
    }

    /// Проверяет exact typed unsupported category для одного изолированного field-а.
    fn assert_unsupported(
        request: &YtDlpRequestMaterial,
        expected: YtDlpSmoothUnsupportedRequestMaterial,
    ) {
        assert_eq!(
            request
                .smooth_manifest_request_material()
                .expect_err("foreign material должен fail closed"),
            YtDlpSmoothManifestRequestMaterialViolation::Unsupported(expected)
        );
    }

    #[test]
    fn equal_and_single_target_serializations_choose_exact_manifest_target() {
        let exact = "https://user:password@media.invalid/channel.ism/Manifest?token=secret";
        let equal = request(Some(exact), Some(exact));
        let url_only = request(Some(exact), None);
        let manifest_only = request(None, Some(exact));

        assert_eq!(
            equal
                .smooth_manifest_request_material()
                .expect("equal targets должны быть совместимы")
                .manifest_target_for_fetch(),
            exact
        );
        assert_eq!(
            url_only
                .smooth_manifest_request_material()
                .expect("url-only target должен быть допустим")
                .manifest_target_for_fetch(),
            exact
        );
        assert_eq!(
            manifest_only
                .smooth_manifest_request_material()
                .expect("manifest-only target должен быть допустим")
                .manifest_target_for_fetch(),
            exact
        );
    }

    #[test]
    fn missing_different_malformed_and_non_http_targets_are_distinct() {
        assert!(matches!(
            request(None, None).smooth_manifest_request_material(),
            Err(YtDlpSmoothManifestRequestMaterialViolation::MissingTarget)
        ));
        assert!(matches!(
            request(
                Some("https://media.invalid/one"),
                Some("https://media.invalid/two")
            )
            .smooth_manifest_request_material(),
            Err(YtDlpSmoothManifestRequestMaterialViolation::IncompatibleTargets)
        ));
        assert!(matches!(
            request(Some("../relative/Manifest"), None).smooth_manifest_request_material(),
            Err(YtDlpSmoothManifestRequestMaterialViolation::MalformedTarget)
        ));
        assert!(matches!(
            request(Some("ftp://media.invalid/channel.ism/Manifest"), None)
                .smooth_manifest_request_material(),
            Err(YtDlpSmoothManifestRequestMaterialViolation::NonHttpTarget)
        ));
    }

    #[test]
    fn every_foreign_material_category_is_rejected_separately() {
        let mut fragments = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut fragments).fragments = vec![YtDlpRequestFragment {
            url: Some(secret("https://media.invalid/fragment")),
            path: None,
            duration_seconds: Some(1.0),
            byte_length: Some(1),
        }]
        .into_boxed_slice();
        assert_unsupported(
            &fragments,
            YtDlpSmoothUnsupportedRequestMaterial::SerializedFragments,
        );

        let mut fragment_base = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut fragment_base).fragment_base_url = Some(secret("https://media.invalid/base/"));
        assert_unsupported(
            &fragment_base,
            YtDlpSmoothUnsupportedRequestMaterial::FragmentBaseUrl,
        );

        let mut dash_periods = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut dash_periods).is_dash_periods = true;
        assert_unsupported(
            &dash_periods,
            YtDlpSmoothUnsupportedRequestMaterial::DashPeriods,
        );

        let mut inline_hls = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut inline_hls).hls_media_playlist_data = Some(secret("#EXTM3U"));
        assert_unsupported(
            &inline_hls,
            YtDlpSmoothUnsupportedRequestMaterial::InlineHls,
        );

        let mut segment_query = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut segment_query).extra_param_to_segment_url = Some(secret("token=segment"));
        assert_unsupported(
            &segment_query,
            YtDlpSmoothUnsupportedRequestMaterial::SegmentQueryOverride,
        );

        let mut key_query = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut key_query).extra_param_to_key_url = Some(secret("token=key"));
        assert_unsupported(
            &key_query,
            YtDlpSmoothUnsupportedRequestMaterial::KeyQueryOverride,
        );

        let mut hls_aes = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut hls_aes).hls_aes = Some(YtDlpHlsAesMaterial {
            uri: Some(secret("https://key.invalid")),
            key: None,
            iv: None,
        });
        assert_unsupported(&hls_aes, YtDlpSmoothUnsupportedRequestMaterial::HlsAes);

        let mut rtmp = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut rtmp).rtmp = Some(YtDlpRtmpRequestMaterial {
            page_url: None,
            app: None,
            play_path: None,
            tc_url: None,
            flash_version: None,
            live: None,
            connection_arguments: Box::new([]),
            protocol: Some(secret("rtmp")),
            real_time: None,
        });
        assert_unsupported(&rtmp, YtDlpSmoothUnsupportedRequestMaterial::Rtmp);

        let mut range_limit = request(Some("https://media.invalid/Manifest"), None);
        v1_mut(&mut range_limit).http_range_request_limit =
            Some(HttpRangeRequestLimit::new(1).expect("positive test limit"));
        assert_unsupported(
            &range_limit,
            YtDlpSmoothUnsupportedRequestMaterial::HttpRangeRequestLimit,
        );
    }

    #[test]
    fn competing_cookie_serializations_keep_existing_typed_failure() {
        let mut material = request(Some("https://media.invalid/Manifest"), None);
        let material = v1_mut(&mut material);
        material
            .http_headers
            .insert("Cookie".to_owned(), secret("header=secret"));
        material.cookies = Some(secret("field=secret"));

        assert!(matches!(
            YtDlpRequestMaterial::V1(material.clone()).smooth_manifest_request_material(),
            Err(YtDlpSmoothManifestRequestMaterialViolation::Authorization(
                YtDlpRequestMaterialViolation::ConflictingCookieMaterial
            ))
        ));
    }

    #[test]
    fn debug_never_exposes_target_headers_or_cookies() {
        let target = "https://user:password@media.invalid/channel.ism/Manifest?token=query-secret";
        let mut secret_request = request(Some(target), Some(target));
        let material = v1_mut(&mut secret_request);
        material
            .http_headers
            .insert("Authorization".to_owned(), secret("Bearer header-secret"));
        material.cookies = Some(secret("session=cookie-secret"));
        let projected = secret_request
            .smooth_manifest_request_material()
            .expect("secret material должно быть допустимо");
        let incompatible_error = request(
            Some(target),
            Some("https://other:credential@media.invalid/Manifest?token=other-query-secret"),
        )
        .smooth_manifest_request_material()
        .expect_err("different target serializations должны fail closed");

        let diagnostic =
            format!("{secret_request:?} {projected:?} {incompatible_error:?} {incompatible_error}");
        for forbidden in [
            "query-secret",
            "user",
            "password",
            "header-secret",
            "cookie-secret",
            "other-query-secret",
            "credential@",
        ] {
            assert!(!diagnostic.contains(forbidden));
        }
    }
}
