//! Фасад YtDlp/extractor open orchestration внутри `app-egui`.
//!
//! Сама orchestration (candidate → transport → demux composition, HDS/Smooth
//! подготовка, content probe и fallback, extractor source state) живёт в
//! `media-source-open::web_media_open` (session-05 выноса web-media). Здесь
//! остаются:
//! - re-export-ы, сохраняющие прежние пути `crate::web_media_open::*` у
//!   startup/queue/settings потребителей (guardrail S27 проверяет, что они
//!   идут через единый `crate::web_media_open::prepare_yt_dlp_web_media(`);
//! - сквозные playback-тесты `content_probe_tests`: им нужен GPU/render/FFmpeg
//!   стек, который `media-source-open` запрещён guardrail-ом, и их helper-ы
//!   переиспользуют вертикальные тесты `media_open/web/tests`.

#[cfg(test)]
pub(crate) mod content_probe_tests;

pub(crate) use media_source_open::web_media_open::{
    ComponentVariantFinalizationError, ExtractorMediaSourceState, YtDlpCandidateOpenIntent,
    prepare_yt_dlp_web_media,
};
// Результат подготовки по имени нужен только сквозным тестам `content_probe_tests`.
#[cfg(test)]
pub(crate) use media_source_open::web_media_open::PreparedYtDlpWebMedia;
