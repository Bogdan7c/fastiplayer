//! Минимальные typed DTO metadata-only extractor boundary.

use serde::Deserialize;

/// Поля общего `yt-dlp --dump-single-json`, нужные metadata enrichment-у.
#[derive(Debug, Deserialize)]
pub(crate) struct YtDlpMetadata {
    /// Extractor topology discriminator, например `playlist` или `multi_video`.
    #[serde(rename = "_type")]
    pub(crate) entry_type: Option<String>,
    /// Наличие `entries` само по себе доказывает collection topology.
    pub(crate) entries: Option<Vec<serde_json::Value>>,
    /// Пользовательский title без участия playback selection.
    pub(crate) title: Option<String>,
    /// Duration в секундах из extractor metadata.
    pub(crate) duration: Option<f64>,
}
