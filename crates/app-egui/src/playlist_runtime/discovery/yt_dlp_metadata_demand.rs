//! Сборка demand-ов yt-dlp metadata enrichment для видимых строк очереди.
//!
//! Владеет единственным вопросом: у какого URL спрашивать метаданные. Ответ — у
//! собственной ссылки ролика ([`operational_open_locator`]), а не у корня коллекции,
//! из которой строка импортирована: иначе yt-dlp резолвил бы весь плейлист.

use playlist_core::PlaylistItemId;

use super::yt_dlp_metadata::YtDlpMetadataDemand;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::operational_open::operational_open_locator;
use crate::url_service_adapter::{
    PlaylistUrlMetadataSource, StartupUrlClassification, classify_playlist_url,
};

/// Строит demand-ы только для строк без заголовка, чью ссылку поддерживает yt-dlp.
pub(super) fn yt_dlp_metadata_demands(
    controller: &PlaylistController,
    item_ids: &[PlaylistItemId],
    yt_dlp_config: &fastiplayer_config::YtDlpConfig,
) -> Vec<YtDlpMetadataDemand> {
    item_ids
        .iter()
        .filter_map(|item_id| {
            let item = controller.queue().item(*item_id)?;
            if item
                .cached_metadata()
                .title()
                .is_some_and(|title| !title.trim().is_empty())
            {
                return None;
            }
            // Строка без отдельной ссылки (extractor-only) не обогащается: откат на корень
            // коллекции запустил бы резолв всего плейлиста. Это штатный пропуск, не ошибка:
            // причину пользователю покажет попытка открыть строку.
            let open_locator = operational_open_locator(item).ok()?;
            let secret_url = open_locator.as_secret_url()?;
            let StartupUrlClassification::Supported(locator) = classify_playlist_url(secret_url)
            else {
                return None;
            };
            let PlaylistUrlMetadataSource::YtDlp(yt_dlp_locator) =
                locator.playlist_metadata_source()?;
            Some(YtDlpMetadataDemand::new(
                *item_id,
                // Stale-guard сверяет identity строки, а не ссылку, по которой идёт запрос.
                item.locator().clone(),
                yt_dlp_locator,
                yt_dlp_config.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;
