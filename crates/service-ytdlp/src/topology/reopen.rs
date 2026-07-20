//! Service-owned classification стабильной topology identity для durable reopen.

use std::fmt;

use thiserror::Error;

use crate::YtDlpMediaLocator;

/// Stable owner discriminator для neutral durable payload registry.
pub const YT_DLP_DURABLE_REOPEN_SERVICE_OWNER: &str = "service-ytdlp";
/// Версия exact payload grammar, которой владеет только `service-ytdlp`.
pub const YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION: u16 = 1;
/// Hard bound service payload до передачи neutral playlist boundary.
pub const YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES: usize = 8 * 1024;

/// Service-owned stable identity category без playlist/domain dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpDurableReopenMaterialKind {
    /// Exact extracted webpage identity.
    StableWebpageIdentity,
    /// Exact extracted original/delegation identity.
    StableOriginalIdentity,
    /// Extractor namespace + local stable ID.
    StableExtractorIdentity,
}

/// Named borrowed input для service-owned stable identity classification.
pub struct YtDlpDurableReopenIdentityInput<'identity> {
    /// Extractor-local stable ID.
    pub extractor_id: Option<&'identity str>,
    /// Extractor namespace/key.
    pub extractor_key: Option<&'identity str>,
    /// Exact stable webpage locator.
    pub webpage_locator: Option<&'identity YtDlpMediaLocator>,
    /// Exact stable original locator.
    pub original_locator: Option<&'identity YtDlpMediaLocator>,
}

impl fmt::Debug for YtDlpDurableReopenIdentityInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDurableReopenIdentityInput")
            .field("has_extractor_id", &self.extractor_id.is_some())
            .field("has_extractor_key", &self.extractor_key.is_some())
            .field("has_webpage_locator", &self.webpage_locator.is_some())
            .field("has_original_locator", &self.original_locator.is_some())
            .finish()
    }
}

/// Versioned opaque payload, который app может перенести в neutral durable locator.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpDurableReopenPayload {
    /// Stable semantic category.
    material_kind: YtDlpDurableReopenMaterialKind,
    /// Exact service-owned v1 bytes.
    payload: Box<[u8]>,
}

impl YtDlpDurableReopenPayload {
    /// Возвращает owner-controlled material category.
    #[must_use]
    pub const fn material_kind(&self) -> YtDlpDurableReopenMaterialKind {
        self.material_kind
    }

    /// Раскрывает exact bytes только persistence/reopen adapter-у.
    #[must_use]
    pub fn expose_payload_for_persistence(&self) -> &[u8] {
        &self.payload
    }

    /// Передаёт owned exact bytes neutral persistence adapter-у без второго копирования.
    #[must_use]
    pub fn into_payload_for_persistence(self) -> Vec<u8> {
        self.payload.into_vec()
    }
}

impl fmt::Debug for YtDlpDurableReopenPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpDurableReopenPayload")
            .field("material_kind", &self.material_kind)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Safe classification failure без extractor ID либо raw locator bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum YtDlpDurableReopenClassificationError {
    /// Extractor не дал ни stable locator, ни local ID.
    #[error("topology identity не содержит durable reopen material")]
    MissingStableIdentity,
    /// Stable identity не помещается в approved durable payload envelope.
    #[error(
        "topology identity превышает durable payload bound: {provided_bytes} > {maximum_bytes}"
    )]
    PayloadLimitExceeded {
        /// Фактический размер без содержимого.
        provided_bytes: usize,
        /// Approved hard maximum.
        maximum_bytes: usize,
    },
    /// Versioned extractor grammar не может представить длину компонента.
    #[error("extractor identity не помещается в versioned length field")]
    ExtractorIdentityLengthExceeded,
}

/// Классифицирует только stable webpage/original/extractor identity в owner-defined порядке.
pub fn classify_yt_dlp_durable_reopen_identity(
    input: YtDlpDurableReopenIdentityInput<'_>,
) -> Result<YtDlpDurableReopenPayload, YtDlpDurableReopenClassificationError> {
    if let Some(locator) = input.webpage_locator {
        return payload_from_exact_locator(
            YtDlpDurableReopenMaterialKind::StableWebpageIdentity,
            locator,
        );
    }
    if let Some(locator) = input.original_locator {
        return payload_from_exact_locator(
            YtDlpDurableReopenMaterialKind::StableOriginalIdentity,
            locator,
        );
    }

    let extractor_id = input
        .extractor_id
        .filter(|identity| !identity.is_empty())
        .ok_or(YtDlpDurableReopenClassificationError::MissingStableIdentity)?;
    let payload = encode_extractor_identity(input.extractor_key, extractor_id)?;
    build_payload(
        YtDlpDurableReopenMaterialKind::StableExtractorIdentity,
        payload,
    )
}

/// Классифицирует delegation target как original stable service identity.
pub fn classify_yt_dlp_delegation_reopen_target(
    target: &YtDlpMediaLocator,
) -> Result<YtDlpDurableReopenPayload, YtDlpDurableReopenClassificationError> {
    payload_from_exact_locator(
        YtDlpDurableReopenMaterialKind::StableOriginalIdentity,
        target,
    )
}

/// Переносит exact typed locator bytes без URL normalization/reparse.
fn payload_from_exact_locator(
    material_kind: YtDlpDurableReopenMaterialKind,
    locator: &YtDlpMediaLocator,
) -> Result<YtDlpDurableReopenPayload, YtDlpDurableReopenClassificationError> {
    build_payload(
        material_kind,
        locator.expose_secret_for_persistence().as_bytes().to_vec(),
    )
}

/// Кодирует extractor identity как `[key_len:u16][key][id_len:u16][id]`.
fn encode_extractor_identity(
    extractor_key: Option<&str>,
    extractor_id: &str,
) -> Result<Vec<u8>, YtDlpDurableReopenClassificationError> {
    let extractor_key = extractor_key.unwrap_or_default().as_bytes();
    let extractor_id = extractor_id.as_bytes();
    let extractor_key_len = u16::try_from(extractor_key.len())
        .map_err(|_| YtDlpDurableReopenClassificationError::ExtractorIdentityLengthExceeded)?;
    let extractor_id_len = u16::try_from(extractor_id.len())
        .map_err(|_| YtDlpDurableReopenClassificationError::ExtractorIdentityLengthExceeded)?;
    let payload_capacity = extractor_key
        .len()
        .checked_add(extractor_id.len())
        .and_then(|combined| combined.checked_add(4))
        .ok_or(
            YtDlpDurableReopenClassificationError::PayloadLimitExceeded {
                provided_bytes: usize::MAX,
                maximum_bytes: YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES,
            },
        )?;
    let mut payload = Vec::with_capacity(payload_capacity);

    payload.extend_from_slice(&extractor_key_len.to_be_bytes());
    payload.extend_from_slice(extractor_key);
    payload.extend_from_slice(&extractor_id_len.to_be_bytes());
    payload.extend_from_slice(extractor_id);
    Ok(payload)
}

/// Применяет единый service payload bound до передачи app/domain adapter-у.
fn build_payload(
    material_kind: YtDlpDurableReopenMaterialKind,
    payload: Vec<u8>,
) -> Result<YtDlpDurableReopenPayload, YtDlpDurableReopenClassificationError> {
    if payload.len() > YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES {
        return Err(
            YtDlpDurableReopenClassificationError::PayloadLimitExceeded {
                provided_bytes: payload.len(),
                maximum_bytes: YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES,
            },
        );
    }

    Ok(YtDlpDurableReopenPayload {
        material_kind,
        payload: payload.into_boxed_slice(),
    })
}

#[cfg(test)]
mod tests {
    use crate::parse_yt_dlp_media_locator;

    use super::*;

    #[test]
    fn webpage_then_original_then_extractor_priority_is_exact() {
        let webpage = parse_yt_dlp_media_locator("https://web.invalid/watch?v=1&secret=exact")
            .expect("webpage fixture");
        let original = parse_yt_dlp_media_locator("https://original.invalid/item?id=2")
            .expect("original fixture");
        let input = YtDlpDurableReopenIdentityInput {
            extractor_id: Some("extractor-id"),
            extractor_key: Some("Extractor"),
            webpage_locator: Some(&webpage),
            original_locator: Some(&original),
        };
        let input_debug = format!("{input:?}");
        let payload = classify_yt_dlp_durable_reopen_identity(input).expect("stable identity");

        assert!(!input_debug.contains("extractor-id"));
        assert!(!input_debug.contains("secret"));
        assert_eq!(
            payload.material_kind(),
            YtDlpDurableReopenMaterialKind::StableWebpageIdentity
        );
        assert_eq!(
            payload.expose_payload_for_persistence(),
            webpage.expose_secret_for_persistence().as_bytes()
        );
    }

    #[test]
    fn extractor_payload_has_versioned_unambiguous_lengths() {
        let payload = classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
            extractor_id: Some("video-42"),
            extractor_key: Some("Example"),
            webpage_locator: None,
            original_locator: None,
        })
        .expect("extractor identity");

        assert_eq!(
            payload.material_kind(),
            YtDlpDurableReopenMaterialKind::StableExtractorIdentity
        );
        assert_eq!(
            payload.expose_payload_for_persistence(),
            b"\0\x07Example\0\x08video-42"
        );
    }

    #[test]
    fn missing_and_oversized_identity_are_typed_without_raw_debug() {
        assert_eq!(
            classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
                extractor_id: None,
                extractor_key: Some("orphan"),
                webpage_locator: None,
                original_locator: None,
            }),
            Err(YtDlpDurableReopenClassificationError::MissingStableIdentity)
        );
        assert_eq!(
            classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
                extractor_id: Some(""),
                extractor_key: Some("orphan"),
                webpage_locator: None,
                original_locator: None,
            }),
            Err(YtDlpDurableReopenClassificationError::MissingStableIdentity)
        );

        let oversized_exact = format!(
            "https://oversized.invalid/{}",
            "s".repeat(YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES)
        );
        let oversized_locator =
            parse_yt_dlp_media_locator(&oversized_exact).expect("syntactically valid fixture");
        let error = classify_yt_dlp_delegation_reopen_target(&oversized_locator)
            .expect_err("oversized durable identity");
        let debug = format!("{error:?}");

        assert!(matches!(
            error,
            YtDlpDurableReopenClassificationError::PayloadLimitExceeded { .. }
        ));
        assert!(!debug.contains("oversized.invalid"));
        assert!(!debug.contains(&"s".repeat(32)));
    }

    #[test]
    fn delegation_target_preserves_exact_original_identity_and_redacts_debug() {
        let target = parse_yt_dlp_media_locator(
            "https://user:password@delegate.invalid/path?token=secret#fragment",
        )
        .expect("delegation fixture");
        let payload =
            classify_yt_dlp_delegation_reopen_target(&target).expect("delegation identity");
        let debug = format!("{payload:?}");

        assert_eq!(
            payload.material_kind(),
            YtDlpDurableReopenMaterialKind::StableOriginalIdentity
        );
        assert_eq!(
            payload.expose_payload_for_persistence(),
            target.expose_secret_for_persistence().as_bytes()
        );
        assert!(!debug.contains("password"));
        assert!(!debug.contains("token"));
        assert!(!debug.contains("/path"));
    }
}
