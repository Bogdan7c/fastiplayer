//! Validation ASS/SSA-лимитов секции `[subtitles]` (схема v18).
//!
//! Вынесено из `validation/subtitles.rs`, чтобы тот файл не рос. Ошибки называют полный
//! путь поля, сообщения английские.

use crate::{
    ConfigResult, MAX_MAX_ASS_EVENTS_ON_SCREEN, MAX_MAX_ASS_HEADER_KB,
    MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT, MAX_MAX_ASS_STYLES, MIN_MAX_ASS_EVENTS_ON_SCREEN,
    MIN_MAX_ASS_HEADER_KB, MIN_MAX_ASS_OVERRIDE_TAGS_PER_EVENT, MIN_MAX_ASS_STYLES,
    SubtitlesConfig,
};

use super::validate_u32_range;

/// Проверяет четыре лимита разбора ASS; режим позиции — enum, вне набора serde его не примет.
pub(super) fn validate_ass_limits(subtitles: &SubtitlesConfig) -> ConfigResult<()> {
    validate_u32_range(
        "subtitles.max_ass_header_kb",
        subtitles.max_ass_header_kb,
        MIN_MAX_ASS_HEADER_KB,
        MAX_MAX_ASS_HEADER_KB,
    )?;
    validate_u32_range(
        "subtitles.max_ass_styles",
        subtitles.max_ass_styles,
        MIN_MAX_ASS_STYLES,
        MAX_MAX_ASS_STYLES,
    )?;
    validate_u32_range(
        "subtitles.max_ass_override_tags_per_event",
        subtitles.max_ass_override_tags_per_event,
        MIN_MAX_ASS_OVERRIDE_TAGS_PER_EVENT,
        MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT,
    )?;
    validate_u32_range(
        "subtitles.max_ass_events_on_screen",
        subtitles.max_ass_events_on_screen,
        MIN_MAX_ASS_EVENTS_ON_SCREEN,
        MAX_MAX_ASS_EVENTS_ON_SCREEN,
    )
}
