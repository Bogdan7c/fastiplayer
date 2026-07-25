//! Service-owned HDS manifest request material.

use source_core::HttpRequestTarget;

use super::{
    SecretText, YtDlpRequestMaterial, YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
};

/// Borrowed validated HDS root-manifest material.
pub struct YtDlpHdsManifestRequestMaterial<'candidate> {
    /// Authoritative manifest target, preferring manifest_url over url.
    pub(super) target: &'candidate SecretText,
    /// Parent material remains the single owner of headers/cookies.
    pub(super) material: &'candidate YtDlpRequestMaterialV1,
    /// Effective cookie serialization after conflict validation.
    pub(super) serialized_cookies: Option<&'candidate SecretText>,
}

impl<'candidate> YtDlpHdsManifestRequestMaterial<'candidate> {
    /// Returns the validated absolute manifest target.
    pub fn manifest_target_for_fetch(&self) -> &str {
        self.target.expose_secret_for_transport()
    }

    /// Returns transient HTTP headers without changing their owner.
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.material
            .http_headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.expose_secret_for_transport()))
    }

    /// Returns effective scoped cookie serialization.
    pub fn serialized_cookies(&self) -> Option<&str> {
        self.serialized_cookies
            .map(SecretText::expose_secret_for_transport)
    }
}

impl std::fmt::Debug for YtDlpHdsManifestRequestMaterial<'_> {
    /// Debug is intentionally secret-safe.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("YtDlpHdsManifestRequestMaterial")
            .field("has_manifest_target", &true)
            .field("header_count", &self.material.http_headers.len())
            .field("has_cookies", &self.serialized_cookies.is_some())
            .finish_non_exhaustive()
    }
}

/// Typed HDS material rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpHdsManifestRequestMaterialViolation {
    /// Общая auth serialization policy already rejected the row.
    #[error("HDS authorization material is invalid")]
    Authorization(#[source] YtDlpRequestMaterialViolation),
    /// Material contains a category outside F4M manifest ownership.
    #[error("HDS request material contains unsupported fields")]
    Unsupported(#[source] YtDlpRequestMaterialViolation),
    /// No manifest_url/url was available.
    #[error("HDS request material has no manifest target")]
    MissingManifestTarget,
    /// Target was not absolute HTTP(S).
    #[error("HDS manifest target is not absolute HTTP(S)")]
    NonHttpTarget,
}

/// Proves the narrow S38 manifest-only request subset.
pub(super) fn hds_manifest_request_material(
    request: &YtDlpRequestMaterial,
) -> Result<YtDlpHdsManifestRequestMaterial<'_>, YtDlpHdsManifestRequestMaterialViolation> {
    let YtDlpRequestMaterial::V1(material) = request;
    let authorization = request
        .http_authorization_material()
        .map_err(YtDlpHdsManifestRequestMaterialViolation::Authorization)?;
    if !material.fragments.is_empty()
        || material.fragment_base_url.is_some()
        || material.is_dash_periods
        || material.hls_media_playlist_data.is_some()
        || material.extra_param_to_segment_url.is_some()
        || material.extra_param_to_key_url.is_some()
        || material.hls_aes.is_some()
        || material.rtmp.is_some()
    {
        return Err(YtDlpHdsManifestRequestMaterialViolation::Unsupported(
            YtDlpRequestMaterialViolation::NonProgressiveMaterial,
        ));
    }
    let target = material
        .manifest_url
        .as_ref()
        .or(material.url.as_ref())
        .ok_or(YtDlpHdsManifestRequestMaterialViolation::MissingManifestTarget)?;
    if HttpRequestTarget::parse_exact(target.expose_secret_for_transport()).is_err() {
        return Err(YtDlpHdsManifestRequestMaterialViolation::NonHttpTarget);
    }
    Ok(YtDlpHdsManifestRequestMaterial {
        target,
        material,
        serialized_cookies: authorization.serialized_cookies,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::YtDlpRequestFragment;
    use super::*;

    /// Создаёт bounded secret тем же owner-типом, что и production normalization.
    fn secret(value: &str) -> SecretText {
        SecretText::bounded(value.to_owned(), 65_536).expect("test secret fits bound")
    }

    /// Строит минимальный manifest-only HDS material.
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

    /// Проверяет manifest_url precedence и secret-safe public accessors.
    #[test]
    fn manifest_target_precedes_url_and_preserves_authorization_material() {
        let mut request = request(
            Some("https://media.example/fallback.f4m"),
            Some("https://media.example/root.f4m"),
        );
        let YtDlpRequestMaterial::V1(material) = &mut request;
        material
            .http_headers
            .insert("User-Agent".to_owned(), secret("fixture-agent"));
        material.cookies = Some(secret("session=fixture"));

        let hds = request
            .hds_manifest_request_material()
            .expect("valid HDS material");

        assert_eq!(
            hds.manifest_target_for_fetch(),
            "https://media.example/root.f4m"
        );
        assert_eq!(
            hds.headers().collect::<Vec<_>>(),
            [("User-Agent", "fixture-agent")]
        );
        assert_eq!(hds.serialized_cookies(), Some("session=fixture"));
        assert!(!format!("{hds:?}").contains("fixture-agent"));
    }

    /// Проверяет, что HDS не принимает already-expanded fragment material.
    #[test]
    fn rejects_non_manifest_material() {
        let mut request = request(Some("https://media.example/root.f4m"), None);
        let YtDlpRequestMaterial::V1(material) = &mut request;
        material.fragments = vec![YtDlpRequestFragment {
            url: Some(secret("https://media.example/Seg1-Frag1")),
            path: None,
            duration_seconds: Some(1.0),
            byte_length: None,
        }]
        .into_boxed_slice();

        let error = request
            .hds_manifest_request_material()
            .expect_err("HDS S38 owns bootstrap expansion, not extractor fragments");

        assert!(matches!(
            error,
            YtDlpHdsManifestRequestMaterialViolation::Unsupported(_)
        ));
    }

    /// Проверяет typed rejection non-HTTP root target.
    #[test]
    fn rejects_non_http_manifest_target() {
        let request = request(Some("ftp://media.example/root.f4m"), None);

        assert_eq!(
            request
                .hds_manifest_request_material()
                .expect_err("HDS manifest must be HTTP(S)"),
            YtDlpHdsManifestRequestMaterialViolation::NonHttpTarget
        );
    }
}
