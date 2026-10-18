//! Повторное извлечение ссылок на субтитры у yt-dlp, когда подписи протухли.

use fastiplayer_config::YtDlpConfig;
use service_ytdlp::{
    YtDlpExtractorAdapter, YtDlpMediaLocator, YtDlpServiceError, YtDlpSubtitleInventory,
};
use source_core::CancellationToken;
use web_media_core::{ExtractionGeneration, ExtractorInvocationReason};

use super::types::WebSubtitleFetchError;

/// Поколение извлечения для свежего снимка: снимок живёт только ради ссылок.
const REFRESH_EXTRACTION_GENERATION: u64 = 1;

/// Источник свежих ссылок. Отдельный трейт нужен, чтобы граница загрузки не знала
/// про конкретный процесс yt-dlp и чтобы её можно было проверять заглушкой.
pub(crate) trait SubtitleSourceRefresher: Send + Sync {
    /// Заново получает инвентарь субтитров. Блокирующий вызов.
    fn refresh(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<YtDlpSubtitleInventory, WebSubtitleFetchError>;
}

/// Обновление через тот же адаптер, locator и настройки, что открыли видео.
pub(crate) struct YtDlpSubtitleRefresher {
    /// Исходная ссылка видео (хранится byte-for-byte, Debug показывает только host).
    locator: YtDlpMediaLocator,
    /// Адаптер запуска процесса (в тестах подменяется launcher).
    extractor_adapter: YtDlpExtractorAdapter,
    /// Настройки yt-dlp, с которыми открывалось видео.
    yt_dlp_config: YtDlpConfig,
}

impl YtDlpSubtitleRefresher {
    /// Запоминает всё, что нужно для одного повторного извлечения.
    pub(crate) fn new(
        locator: YtDlpMediaLocator,
        extractor_adapter: YtDlpExtractorAdapter,
        yt_dlp_config: YtDlpConfig,
    ) -> Self {
        Self {
            locator,
            extractor_adapter,
            yt_dlp_config,
        }
    }
}

impl SubtitleSourceRefresher for YtDlpSubtitleRefresher {
    fn refresh(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<YtDlpSubtitleInventory, WebSubtitleFetchError> {
        let source = crate::web_media_open::next_source_identity()
            .map_err(|_| WebSubtitleFetchError::RefreshFailed)?;
        let snapshot = self
            .extractor_adapter
            .resolve_candidate_snapshot_with_cancellation(
                &self.locator,
                source,
                ExtractionGeneration::new(REFRESH_EXTRACTION_GENERATION),
                &self.yt_dlp_config,
                ExtractorInvocationReason::ExtractorBackedRecovery,
                &|| cancellation.is_cancelled(),
            )
            .map_err(|error| match error {
                YtDlpServiceError::Cancellation => WebSubtitleFetchError::Cancelled,
                other => {
                    // Текст ошибки сервиса redacted: без URL и вывода процесса.
                    tracing::warn!(error = %other, "subtitle link refresh via yt-dlp failed");
                    WebSubtitleFetchError::RefreshFailed
                }
            })?;
        Ok(snapshot.subtitle_inventory().clone())
    }
}
