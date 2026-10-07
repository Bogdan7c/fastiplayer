//! Service-owned обратное преобразование durable reopen payload в typed yt-dlp locator.
//!
//! `reopen.rs` кодирует stable identity ролика в opaque bytes; здесь они
//! декодируются обратно в `YtDlpMediaLocator`, чтобы app мог открыть ОДИН ролик,
//! а не коллекцию, из которой он был импортирован. Грамматику bytes знает только
//! `service-ytdlp`: app передаёт сюда лишь нейтральные поля payload-а.

use thiserror::Error;

use super::reopen::{
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
    YtDlpDurableReopenMaterialKind,
};
use crate::{YtDlpMediaLocator, parse_yt_dlp_media_locator};

/// Named borrowed поля нейтрального durable payload-а (без зависимости от `playlist-core`).
pub struct YtDlpDurableReopenPayloadInput<'payload> {
    /// Owner discriminator, записанный в payload при импорте.
    pub service_owner: &'payload str,
    /// Версия грамматики bytes.
    pub payload_version: u16,
    /// Категория stable identity.
    pub material_kind: YtDlpDurableReopenMaterialKind,
    /// Exact bytes payload-а.
    pub payload_bytes: &'payload [u8],
}

/// Почему payload нельзя превратить в ссылку на отдельный ролик.
///
/// Тексты безопасны для UI/логов: ни bytes, ни идентификаторы в них не попадают.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum YtDlpDurableReopenDecodeError {
    /// Payload записал другой сервис.
    #[error("Ссылка на ролик принадлежит другому сервису и не может быть открыта через yt-dlp")]
    ForeignOwner,
    /// Версия грамматики неизвестна этой сборке.
    #[error("Ссылка на ролик сохранена в неподдерживаемом формате (версия {version})")]
    UnsupportedVersion {
        /// Версия из payload-а.
        version: u16,
    },
    /// Сохранён только внутренний идентификатор экстрактора, URL-формы нет.
    #[error(
        "У ролика не сохранилась собственная ссылка, только внутренний идентификатор; \
         откройте плейлист по исходной ссылке заново"
    )]
    ExtractorIdentityWithoutUrl,
    /// Сохранённые bytes не являются допустимой ссылкой.
    #[error("Сохранённая ссылка на ролик повреждена")]
    InvalidStoredLocator,
}

/// Превращает stable identity ролика обратно в typed locator для открытия.
///
/// Extractor identity (`[key][id]`) намеренно НЕ превращается в URL: без знания
/// конкретного экстрактора это была бы догадка, а не reopen.
pub fn decode_yt_dlp_durable_reopen_payload(
    input: YtDlpDurableReopenPayloadInput<'_>,
) -> Result<YtDlpMediaLocator, YtDlpDurableReopenDecodeError> {
    if input.service_owner != YT_DLP_DURABLE_REOPEN_SERVICE_OWNER {
        return Err(YtDlpDurableReopenDecodeError::ForeignOwner);
    }
    if input.payload_version != YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION {
        return Err(YtDlpDurableReopenDecodeError::UnsupportedVersion {
            version: input.payload_version,
        });
    }
    match input.material_kind {
        YtDlpDurableReopenMaterialKind::StableWebpageIdentity
        | YtDlpDurableReopenMaterialKind::StableOriginalIdentity => {
            let exact_url = std::str::from_utf8(input.payload_bytes)
                .map_err(|_| YtDlpDurableReopenDecodeError::InvalidStoredLocator)?;
            parse_yt_dlp_media_locator(exact_url)
                .map_err(|_| YtDlpDurableReopenDecodeError::InvalidStoredLocator)
        }
        YtDlpDurableReopenMaterialKind::StableExtractorIdentity => {
            Err(YtDlpDurableReopenDecodeError::ExtractorIdentityWithoutUrl)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::reopen::{
        YtDlpDurableReopenIdentityInput, classify_yt_dlp_delegation_reopen_target,
        classify_yt_dlp_durable_reopen_identity,
    };

    fn input_from(
        payload: &crate::topology::reopen::YtDlpDurableReopenPayload,
    ) -> YtDlpDurableReopenPayloadInput<'_> {
        YtDlpDurableReopenPayloadInput {
            service_owner: YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
            payload_version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
            material_kind: payload.material_kind(),
            payload_bytes: payload.expose_payload_for_persistence(),
        }
    }

    #[test]
    fn webpage_identity_round_trips_to_exact_locator() {
        let webpage = parse_yt_dlp_media_locator("https://www.example.invalid/watch?v=abc&t=5")
            .expect("webpage fixture");
        let payload = classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
            extractor_id: Some("abc"),
            extractor_key: Some("Example"),
            webpage_locator: Some(&webpage),
            original_locator: None,
        })
        .expect("payload");

        let decoded = decode_yt_dlp_durable_reopen_payload(input_from(&payload)).expect("decode");

        assert_eq!(
            decoded.expose_secret_for_persistence(),
            webpage.expose_secret_for_persistence()
        );
    }

    #[test]
    fn original_identity_round_trips_to_exact_locator() {
        let original = parse_yt_dlp_media_locator("https://orig.example.invalid/v/1?k=2")
            .expect("original fixture");
        let payload = classify_yt_dlp_delegation_reopen_target(&original).expect("payload");
        assert_eq!(
            payload.material_kind(),
            YtDlpDurableReopenMaterialKind::StableOriginalIdentity
        );

        let decoded = decode_yt_dlp_durable_reopen_payload(input_from(&payload)).expect("decode");

        assert_eq!(
            decoded.expose_secret_for_persistence(),
            original.expose_secret_for_persistence()
        );
    }

    #[test]
    fn extractor_identity_is_refused_without_url_guess() {
        let payload = classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
            extractor_id: Some("video-42"),
            extractor_key: Some("Example"),
            webpage_locator: None,
            original_locator: None,
        })
        .expect("payload");

        let error = decode_yt_dlp_durable_reopen_payload(input_from(&payload))
            .expect_err("extractor identity has no URL form");

        assert_eq!(
            error,
            YtDlpDurableReopenDecodeError::ExtractorIdentityWithoutUrl
        );
        assert!(!error.to_string().contains("video-42"));
    }

    #[test]
    fn foreign_owner_version_and_corrupt_bytes_are_typed_refusals() {
        let base = YtDlpDurableReopenPayloadInput {
            service_owner: YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
            payload_version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
            material_kind: YtDlpDurableReopenMaterialKind::StableWebpageIdentity,
            payload_bytes: b"https://ok.invalid/v",
        };
        assert!(decode_yt_dlp_durable_reopen_payload(base).is_ok());

        let foreign = YtDlpDurableReopenPayloadInput {
            service_owner: "other-service",
            payload_version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
            material_kind: YtDlpDurableReopenMaterialKind::StableWebpageIdentity,
            payload_bytes: b"https://ok.invalid/v",
        };
        assert_eq!(
            decode_yt_dlp_durable_reopen_payload(foreign).expect_err("foreign"),
            YtDlpDurableReopenDecodeError::ForeignOwner
        );
        let future = YtDlpDurableReopenPayloadInput {
            service_owner: YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
            payload_version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION + 1,
            material_kind: YtDlpDurableReopenMaterialKind::StableWebpageIdentity,
            payload_bytes: b"https://ok.invalid/v",
        };
        assert_eq!(
            decode_yt_dlp_durable_reopen_payload(future).expect_err("version"),
            YtDlpDurableReopenDecodeError::UnsupportedVersion {
                version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION + 1
            }
        );
        for corrupt in [&b"\xff\xfe"[..], &b"not a url"[..]] {
            let input = YtDlpDurableReopenPayloadInput {
                service_owner: YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
                payload_version: YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
                material_kind: YtDlpDurableReopenMaterialKind::StableOriginalIdentity,
                payload_bytes: corrupt,
            };
            assert_eq!(
                decode_yt_dlp_durable_reopen_payload(input).expect_err("corrupt"),
                YtDlpDurableReopenDecodeError::InvalidStoredLocator
            );
        }
    }
}
