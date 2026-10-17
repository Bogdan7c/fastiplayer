//! Validation настроек упреждающей отрисовки субтитров `[subtitles]` (схема v19).
//!
//! Ошибки называют полный путь поля, сообщения английские.

use crate::{
    ConfigResult, MAX_PREPARED_OVERLAYS, MAX_PRERENDER_FRAMES, MIN_PREPARED_OVERLAYS,
    MIN_PRERENDER_FRAMES, SubtitlesConfig,
};

use super::validate_u32_range;

/// Проверяет упреждение (0 допустим: только текущий кадр) и размер очереди готовых картинок.
pub(super) fn validate_prerender_settings(subtitles: &SubtitlesConfig) -> ConfigResult<()> {
    validate_u32_range(
        "subtitles.prerender_frames",
        subtitles.prerender_frames,
        MIN_PRERENDER_FRAMES,
        MAX_PRERENDER_FRAMES,
    )?;
    validate_u32_range(
        "subtitles.prepared_overlays",
        subtitles.prepared_overlays,
        MIN_PREPARED_OVERLAYS,
        MAX_PREPARED_OVERLAYS,
    )
}
