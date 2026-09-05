use std::time::Duration;

use fastiplayer_config::YtDlpConfig;
use web_media_core::ExtractorInvocationReason;

use crate::dto::YtDlpMetadata;
use crate::error::YtDlpServiceError;
use crate::invocation::YtDlpExtractorAdapter;
use crate::locator::YtDlpMediaLocator;
use crate::process::{YtDlpProcessConfig, resolve_yt_dlp_candidate_metadata_with_cancellation};

/// Минимальная service-owned metadata для отображения YtDlp media в плейлисте.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtDlpPlaylistMetadata {
    title: Option<String>,
    duration: Option<Duration>,
}

impl YtDlpPlaylistMetadata {
    /// Строит summary из уже полученного extractor snapshot-а без нового I/O.
    pub(crate) fn from_extractor(title: Option<String>, duration: Option<Duration>) -> Self {
        Self {
            title: normalized_title(title),
            duration,
        }
    }

    /// Нормализует raw seconds из extractor JSON внутри metadata-owned boundary.
    pub(crate) fn from_extractor_seconds(
        title: Option<String>,
        duration_seconds: Option<f64>,
    ) -> Self {
        Self::from_extractor(title, duration_from_seconds(duration_seconds))
    }

    /// Возвращает непустое нормализованное название ролика.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает длительность VOD, если extractor смог её определить.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

/// Разрешает только playlist metadata, не открывая media bytes и не выбирая decoder stream.
pub fn resolve_yt_dlp_playlist_metadata_with_config(
    locator: &YtDlpMediaLocator,
    yt_dlp_config: &YtDlpConfig,
    is_cancelled: impl Fn() -> bool,
) -> Result<YtDlpPlaylistMetadata, YtDlpServiceError> {
    YtDlpExtractorAdapter::default().resolve_playlist_metadata_with_cancellation(
        locator,
        yt_dlp_config,
        ExtractorInvocationReason::CollectionTopologyResolution,
        &is_cancelled,
    )
}

/// Реализует metadata adapter method с explicit reason и injected launcher-ом.
pub(crate) fn resolve_playlist_metadata_with_adapter(
    adapter: &YtDlpExtractorAdapter,
    locator: &YtDlpMediaLocator,
    yt_dlp_config: &YtDlpConfig,
    invocation_reason: ExtractorInvocationReason,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<YtDlpPlaylistMetadata, YtDlpServiceError> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpServiceError::AdapterDisabled);
    }

    let process_config = YtDlpProcessConfig::from_yt_dlp_config_with_invocation(
        yt_dlp_config,
        adapter.process_launcher(),
        invocation_reason,
    )?;
    let metadata = resolve_yt_dlp_candidate_metadata_with_cancellation(
        locator,
        &process_config,
        &is_cancelled,
    )?;
    ensure_single_item(&metadata)?;

    Ok(YtDlpPlaylistMetadata::from_extractor_seconds(
        metadata.title,
        metadata.duration,
    ))
}

/// Отделяет metadata-only single item от collection без playback admission policy.
fn ensure_single_item(metadata: &YtDlpMetadata) -> Result<(), YtDlpServiceError> {
    let collection_type = metadata
        .entry_type
        .as_deref()
        .is_some_and(|entry_type| matches!(entry_type, "playlist" | "multi_video"));
    if collection_type || metadata.entries.is_some() {
        return Err(YtDlpServiceError::CollectionUrl);
    }
    Ok(())
}

/// Убирает только внешние пробелы и не подменяет настоящее service title.
fn normalized_title(title: Option<String>) -> Option<String> {
    title
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
}

/// Принимает только конечную неотрицательную длительность от extractor-а.
fn duration_from_seconds(seconds: Option<f64>) -> Option<Duration> {
    let seconds = seconds.filter(|seconds| seconds.is_finite() && *seconds >= 0.0)?;
    Duration::try_from_secs_f64(seconds).ok()
}

#[cfg(test)]
mod tests {
    use super::{duration_from_seconds, normalized_title};

    #[test]
    fn title_normalization_preserves_content_and_rejects_blank_value() {
        assert_eq!(
            normalized_title(Some("  Настоящее название  ".to_string())),
            Some("Настоящее название".to_string())
        );
        assert_eq!(normalized_title(Some(" \n\t ".to_string())), None);
        assert_eq!(normalized_title(None), None);
    }

    #[test]
    fn duration_normalization_rejects_invalid_values() {
        assert_eq!(
            duration_from_seconds(Some(61.5)),
            Some(std::time::Duration::from_millis(61_500))
        );
        assert_eq!(duration_from_seconds(Some(-1.0)), None);
        assert_eq!(duration_from_seconds(Some(f64::NAN)), None);
        assert_eq!(duration_from_seconds(Some(f64::MAX)), None);
    }
}
