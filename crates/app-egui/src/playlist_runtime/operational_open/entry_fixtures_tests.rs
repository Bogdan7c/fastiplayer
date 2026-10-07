//! Общие фикстуры: строки очереди в том виде, как их материализует импорт коллекции.

use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistItemDraft, PlaylistMediaKind,
    PlaylistSingleDurablePayload, SecretUrlLocator, ServiceReopenMaterialKind,
};
use service_ytdlp::{
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
    YtDlpDurableReopenIdentityInput, YtDlpDurableReopenMaterialKind,
    classify_yt_dlp_durable_reopen_identity, parse_yt_dlp_media_locator,
};

/// Ссылка на корень коллекции: именно её хранит `item.locator()` после импорта.
pub(crate) const COLLECTION_ROOT_URL: &str = "https://www.youtube.com/playlist?list=PLroot";

/// Какую собственную identity ролика несёт durable payload.
pub(crate) enum EntryIdentity {
    /// Страница ролика (`webpage_url`).
    Webpage(&'static str),
    /// Исходная ссылка делегата (`url` flat-записи).
    Original(&'static str),
    /// Только внутренний идентификатор экстрактора, без URL-формы.
    ExtractorOnly {
        extractor_key: &'static str,
        extractor_id: &'static str,
    },
}

/// Строит черновик строки так же, как `into_queue_draft` для service-ребёнка:
/// operational locator = корень коллекции, identity ролика лежит в payload.
pub(crate) fn imported_entry_draft(label: &str, identity: &EntryIdentity) -> PlaylistItemDraft {
    let parse = |url: &str| parse_yt_dlp_media_locator(url).expect("fixture URL");
    let (webpage, original, key, id);
    let input = match identity {
        EntryIdentity::Webpage(url) => {
            webpage = parse(url);
            YtDlpDurableReopenIdentityInput {
                extractor_id: None,
                extractor_key: None,
                webpage_locator: Some(&webpage),
                original_locator: None,
            }
        }
        EntryIdentity::Original(url) => {
            original = parse(url);
            YtDlpDurableReopenIdentityInput {
                extractor_id: None,
                extractor_key: None,
                webpage_locator: None,
                original_locator: Some(&original),
            }
        }
        EntryIdentity::ExtractorOnly {
            extractor_key,
            extractor_id,
        } => {
            key = *extractor_key;
            id = *extractor_id;
            YtDlpDurableReopenIdentityInput {
                extractor_id: Some(id),
                extractor_key: Some(key),
                webpage_locator: None,
                original_locator: None,
            }
        }
    };
    let service_payload =
        classify_yt_dlp_durable_reopen_identity(input).expect("classified identity");
    let material_kind = match service_payload.material_kind() {
        YtDlpDurableReopenMaterialKind::StableWebpageIdentity => {
            ServiceReopenMaterialKind::StableWebpageIdentity
        }
        YtDlpDurableReopenMaterialKind::StableOriginalIdentity => {
            ServiceReopenMaterialKind::StableOriginalIdentity
        }
        YtDlpDurableReopenMaterialKind::StableExtractorIdentity => {
            ServiceReopenMaterialKind::StableExtractorIdentity
        }
    };
    let reopen_locator = DurableReopenLocator::from_service_payload(
        YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
        YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
        material_kind,
        service_payload.into_payload_for_persistence(),
    )
    .expect("service payload admitted");
    let root = SecretUrlLocator::from_reopenable_url(COLLECTION_ROOT_URL).expect("root URL");
    let durable_payload = PlaylistSingleDurablePayload::new(
        reopen_locator,
        None,
        Vec::new(),
        PlaylistImportProvenance::new(
            DurableReopenLocator::url(root.clone()),
            PlaylistImportSourceKind::Service,
            None,
        ),
        PlaylistImportAvailability::Available,
    )
    .expect("durable payload");
    PlaylistItemDraft::url(
        root,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
    .with_durable_payload(durable_payload)
}
