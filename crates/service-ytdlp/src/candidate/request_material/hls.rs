//! Intent API pinned yt-dlp HLS material без HTTP/AES/demux ownership.

use std::fmt;

use web_media_transport_api::{
    SecretQueryOverride, SecretQueryOverrideError, SecretRequestContextBuilder,
};

use super::{SecretText, YtDlpHlsAesMaterial, YtDlpRequestMaterial, YtDlpRequestMaterialV1};

/// Происхождение manifest определяет, разрешён ли network fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YtDlpHlsManifestInputKind {
    /// Inline bytes authoritative, повторный manifest fetch запрещён.
    Inline,
    /// Selected format URL должен быть загружен будущим runtime ровно один раз.
    FetchSelectedUrl,
}

/// Validated manifest input, где selected format URL — единственная resolution base.
pub struct YtDlpHlsManifestInput<'material> {
    kind: YtDlpHlsManifestInputKind,
    selected_url: &'material SecretText,
    inline_playlist: Option<&'material SecretText>,
}

impl YtDlpHlsManifestInput<'_> {
    /// Инвариант network/inline-состояния.
    pub const fn kind(&self) -> YtDlpHlsManifestInputKind {
        self.kind
    }

    /// Exact selected `url`, но не `manifest_url`, для разрешения дочерних references.
    pub fn selected_url_for_resolution(&self) -> &str {
        self.selected_url.expose_secret_for_transport()
    }

    /// Возвращает authoritative inline data только в zero-fetch состоянии.
    pub fn inline_playlist_for_parse(&self) -> Option<&str> {
        self.inline_playlist
            .map(SecretText::expose_secret_for_transport)
    }

    /// Возвращает fetch endpoint только при отсутствии inline data.
    pub fn selected_url_for_manifest_fetch(&self) -> Option<&str> {
        (self.kind == YtDlpHlsManifestInputKind::FetchSelectedUrl)
            .then(|| self.selected_url.expose_secret_for_transport())
    }
}

impl fmt::Debug for YtDlpHlsManifestInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpHlsManifestInput")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Validated внешний AES override; значения opaque до HLS crypto owner.
pub struct YtDlpHlsAesOverride<'material> {
    material: &'material YtDlpHlsAesMaterial,
}

impl YtDlpHlsAesOverride<'_> {
    /// External URI заменяет manifest key URI и обходит key-query merge.
    pub fn replacement_key_uri_for_fetch(&self) -> Option<&str> {
        self.material
            .uri
            .as_ref()
            .map(SecretText::expose_secret_for_transport)
    }

    /// Exact validated 16-byte key hex; наличие запрещает любой key fetch.
    pub fn key_hex_for_crypto(&self) -> Option<&str> {
        self.material
            .key
            .as_ref()
            .map(SecretText::expose_secret_for_transport)
    }

    /// Exact validated IV hex; crypto owner дополняет его нулями слева до 16 bytes.
    pub fn iv_hex_for_crypto(&self) -> Option<&str> {
        self.material
            .iv
            .as_ref()
            .map(SecretText::expose_secret_for_transport)
    }
}

impl fmt::Debug for YtDlpHlsAesOverride<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpHlsAesOverride")
            .field("has_uri", &self.material.uri.is_some())
            .field("has_key", &self.material.key.is_some())
            .field("has_iv", &self.material.iv.is_some())
            .finish()
    }
}

/// Полный provider-neutral handoff material для будущего HLS composition adapter.
pub struct YtDlpHlsRequestMaterial<'material> {
    manifest: YtDlpHlsManifestInput<'material>,
    segment_query: Option<&'material SecretText>,
    key_query: Option<&'material SecretText>,
    aes_override: Option<YtDlpHlsAesOverride<'material>>,
}

impl YtDlpHlsRequestMaterial<'_> {
    /// Authoritative inline/fetch-состояние.
    pub const fn manifest(&self) -> &YtDlpHlsManifestInput<'_> {
        &self.manifest
    }

    /// Query parameters, сливаемые в MAP и media segment references.
    fn segment_query_parameters_for_merge(&self) -> Option<&str> {
        self.segment_query
            .map(SecretText::expose_secret_for_transport)
    }

    /// Explicit key query с fallback на segment query ради pinned compatibility.
    fn manifest_key_query_parameters_for_merge(&self) -> Option<&str> {
        if self
            .aes_override
            .as_ref()
            .is_some_and(|aes| aes.material.key.is_some() || aes.material.uri.is_some())
        {
            return None;
        }
        self.key_query
            .or(self.segment_query)
            .map(SecretText::expose_secret_for_transport)
    }

    /// Единожды проецирует yt-dlp query semantics в authoritative scoped transport context.
    ///
    /// После этого HLS runtime читает только `AdaptiveHttpContext`, поэтому segment/key fallback
    /// и `hls_aes.uri` bypass не могут разойтись во втором наборе строк.
    pub fn project_scoped_queries(
        &self,
        mut builder: SecretRequestContextBuilder,
    ) -> Result<SecretRequestContextBuilder, SecretQueryOverrideError> {
        if let Some(query) = self.segment_query_parameters_for_merge() {
            builder = builder.with_segment_query_override(SecretQueryOverride::new(query)?);
        }
        if let Some(query) = self.manifest_key_query_parameters_for_merge() {
            builder = builder.with_key_query_override(SecretQueryOverride::new(query)?);
        }
        Ok(builder)
    }

    /// Опциональный AES override, применяемый только к активному `METHOD=AES-128`.
    pub const fn aes_override(&self) -> Option<&YtDlpHlsAesOverride<'_>> {
        self.aes_override.as_ref()
    }
}

impl fmt::Debug for YtDlpHlsRequestMaterial<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpHlsRequestMaterial")
            .field("manifest", &self.manifest)
            .field("has_segment_query", &self.segment_query.is_some())
            .field("has_key_query", &self.key_query.is_some())
            .field("has_aes_override", &self.aes_override.is_some())
            .finish()
    }
}

/// Недопустимый HLS-specific material отклоняется до любой runtime/player mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpHlsRequestMaterialViolation {
    #[error("selected HLS format не содержит url")]
    MissingSelectedUrl,
    #[error("hls_aes.key не является exact 16-byte hexadecimal key")]
    InvalidAes128Key,
    #[error("hls_aes.iv не является hexadecimal IV длиной не более 16 bytes")]
    InvalidAes128Iv,
}

pub(super) fn hls_request_material(
    request: &YtDlpRequestMaterial,
) -> Result<YtDlpHlsRequestMaterial<'_>, YtDlpHlsRequestMaterialViolation> {
    let YtDlpRequestMaterial::V1(material) = request;
    build_v1(material)
}

fn build_v1(
    material: &YtDlpRequestMaterialV1,
) -> Result<YtDlpHlsRequestMaterial<'_>, YtDlpHlsRequestMaterialViolation> {
    let selected_url = material
        .url
        .as_ref()
        .ok_or(YtDlpHlsRequestMaterialViolation::MissingSelectedUrl)?;
    let kind = if material.hls_media_playlist_data.is_some() {
        YtDlpHlsManifestInputKind::Inline
    } else {
        YtDlpHlsManifestInputKind::FetchSelectedUrl
    };
    let aes_override = material
        .hls_aes
        .as_ref()
        .map(validate_aes_override)
        .transpose()?;
    Ok(YtDlpHlsRequestMaterial {
        manifest: YtDlpHlsManifestInput {
            kind,
            selected_url,
            inline_playlist: material.hls_media_playlist_data.as_ref(),
        },
        segment_query: material.extra_param_to_segment_url.as_ref(),
        key_query: material.extra_param_to_key_url.as_ref(),
        aes_override,
    })
}

fn validate_aes_override(
    material: &YtDlpHlsAesMaterial,
) -> Result<YtDlpHlsAesOverride<'_>, YtDlpHlsRequestMaterialViolation> {
    if let Some(key) = material.key.as_ref() {
        let hex = without_hex_prefix(key.expose_secret_for_transport());
        if hex.len() != 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(YtDlpHlsRequestMaterialViolation::InvalidAes128Key);
        }
    }
    if let Some(iv) = material.iv.as_ref() {
        let hex = without_hex_prefix(iv.expose_secret_for_transport());
        if hex.is_empty() || hex.len() > 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(YtDlpHlsRequestMaterialViolation::InvalidAes128Iv);
        }
    }
    Ok(YtDlpHlsAesOverride { material })
}

fn without_hex_prefix(value: &str) -> &str {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::request_material::{
        MAX_INLINE_HLS_UTF8_BYTES, MAX_REQUEST_SECRET_UTF8_BYTES,
    };
    use source_core::{HttpPathScope, HttpRequestTarget};
    use web_media_transport_api::{SecretRequestContext, SecretRequestPurpose, SecretRequestScope};

    fn secret(value: &str) -> SecretText {
        SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
            .expect("bounded secret")
    }

    fn request(
        inline: Option<&str>,
        segment_query: Option<&str>,
        key_query: Option<&str>,
        aes: Option<YtDlpHlsAesMaterial>,
    ) -> YtDlpRequestMaterial {
        YtDlpRequestMaterial::V1(YtDlpRequestMaterialV1 {
            url: Some(secret("https://cdn.invalid/media.m3u8?token=base")),
            manifest_url: Some(secret("https://wrong.invalid/master.m3u8")),
            fragments: Box::new([]),
            fragment_base_url: None,
            hls_media_playlist_data: inline.map(|value| {
                SecretText::bounded(value.to_owned(), MAX_INLINE_HLS_UTF8_BYTES)
                    .expect("bounded inline")
            }),
            http_headers: Default::default(),
            http_range_request_limit: None,
            cookies: None,
            extra_param_to_segment_url: segment_query.map(secret),
            extra_param_to_key_url: key_query.map(secret),
            hls_aes: aes,
            rtmp: None,
        })
    }

    #[test]
    fn inline_manifest_is_a_zero_fetch_type_state_and_uses_selected_url_as_base() {
        let request = request(Some("#EXTM3U\n"), None, None, None);
        let hls = request.hls_request_material().expect("valid HLS");
        assert_eq!(hls.manifest().kind(), YtDlpHlsManifestInputKind::Inline);
        assert_eq!(
            hls.manifest().inline_playlist_for_parse(),
            Some("#EXTM3U\n")
        );
        assert_eq!(hls.manifest().selected_url_for_manifest_fetch(), None);
        assert_eq!(
            hls.manifest().selected_url_for_resolution(),
            "https://cdn.invalid/media.m3u8?token=base"
        );
        assert!(!format!("{hls:?}").contains("token=base"));
    }

    #[test]
    fn key_query_falls_back_to_segment_and_external_uri_bypasses_it() {
        let aes = YtDlpHlsAesMaterial {
            uri: Some(secret("https://keys.invalid/key?secret=yes")),
            key: None,
            iv: None,
        };
        let request = request(None, Some("segment=1"), None, Some(aes));
        let hls = request.hls_request_material().expect("valid HLS");
        assert_eq!(hls.manifest_key_query_parameters_for_merge(), None);
        let target =
            HttpRequestTarget::parse_exact("https://cdn.invalid/media.m3u8").expect("target");
        let scope =
            SecretRequestScope::from_target(&target, HttpPathScope::new("/").expect("root scope"));
        let secrets = hls
            .project_scoped_queries(SecretRequestContext::builder(scope))
            .expect("query projection")
            .build();
        let segment_material = secrets
            .material_for(&target, SecretRequestPurpose::MediaSegment)
            .expect("segment scope");
        assert!(segment_material.query_override_for_request().is_some());
        let key_material = secrets
            .material_for(&target, SecretRequestPurpose::EncryptionKey)
            .expect("key scope");
        assert!(key_material.query_override_for_request().is_none());
        assert_eq!(
            hls.aes_override()
                .and_then(YtDlpHlsAesOverride::replacement_key_uri_for_fetch),
            Some("https://keys.invalid/key?secret=yes")
        );
        assert!(!format!("{hls:?}").contains("secret=yes"));
    }

    #[test]
    fn aes_hex_validation_is_strict_for_s32_key_and_left_padded_iv() {
        let invalid_key = YtDlpHlsAesMaterial {
            uri: None,
            key: Some(secret("0011")),
            iv: None,
        };
        assert_eq!(
            request(None, None, None, Some(invalid_key))
                .hls_request_material()
                .unwrap_err(),
            YtDlpHlsRequestMaterialViolation::InvalidAes128Key
        );

        let valid = YtDlpHlsAesMaterial {
            uri: None,
            key: Some(secret("0x00112233445566778899aabbccddeeff")),
            iv: Some(secret("f")),
        };
        request(None, None, None, Some(valid))
            .hls_request_material()
            .expect("short IV is left-padded by crypto owner");
    }
}
