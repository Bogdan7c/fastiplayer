//! Validation секции `[subtitles]`: диапазоны лимитов, цвета и список языков.
//!
//! Все ошибки называют полный путь поля (`subtitles.<field>`), сообщения английские.

use crate::{
    AppConfig, ConfigResult, HEX_COLOR_LEN, MAX_BACKGROUND_OPACITY_PERCENT,
    MAX_BLUR_SCRATCH_RETAINED_MB, MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
    MAX_EMBEDDED_FEED_CAPACITY, MAX_FONT_LOOKUP_CACHE_CHARS, MAX_MAX_ATTACHED_FONT_MB,
    MAX_MAX_ATTACHED_FONTS_TOTAL_MB, MAX_MAX_CHARS_PER_CUE, MAX_MAX_CUES_ON_SCREEN,
    MAX_MAX_CUES_PER_FILE, MAX_MAX_FILE_SIZE_MB, MAX_POSITION_HORIZONTAL_PERCENT,
    MAX_POSITION_VERTICAL_PERCENT, MAX_PREFERRED_LANGUAGE_COUNT, MAX_PREFERRED_LANGUAGES_LEN,
    MAX_RENDER_CACHE_MB, MAX_SIDECAR_MAX_CANDIDATES, MAX_SIDECAR_MAX_ENTRIES_PER_FOLDER,
    MAX_TEXT_SIZE_PERCENT, MIN_BACKGROUND_OPACITY_PERCENT, MIN_BLUR_SCRATCH_RETAINED_MB,
    MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS, MIN_EMBEDDED_FEED_CAPACITY, MIN_FONT_LOOKUP_CACHE_CHARS,
    MIN_MAX_ATTACHED_FONT_MB, MIN_MAX_ATTACHED_FONTS_TOTAL_MB, MIN_MAX_CHARS_PER_CUE,
    MIN_MAX_CUES_ON_SCREEN, MIN_MAX_CUES_PER_FILE, MIN_MAX_FILE_SIZE_MB,
    MIN_POSITION_HORIZONTAL_PERCENT, MIN_POSITION_VERTICAL_PERCENT, MIN_RENDER_CACHE_MB,
    MIN_SIDECAR_MAX_CANDIDATES, MIN_SIDECAR_MAX_ENTRIES_PER_FOLDER, MIN_TEXT_SIZE_PERCENT,
    SubtitlesConfig,
};

use super::subtitles_ass::validate_ass_limits;
use super::subtitles_prerender::validate_prerender_settings;
use super::{invalid_value, validate_u16_range, validate_u32_range};

/// Длина одного языкового тега: от двух букв (`uk`) до 35 символов (BCP 47 в разумных пределах).
const MIN_LANGUAGE_TAG_LEN: usize = 2;
const MAX_LANGUAGE_TAG_LEN: usize = 35;

/// Проверяет всю секцию: сначала внешний вид, потом язык, потом лимиты.
pub(super) fn validate_subtitles_section(config: &AppConfig) -> ConfigResult<()> {
    let subtitles = &config.subtitles;
    validate_u16_range(
        "subtitles.text_size_percent",
        subtitles.text_size_percent,
        MIN_TEXT_SIZE_PERCENT,
        MAX_TEXT_SIZE_PERCENT,
    )?;
    validate_hex_color("subtitles.text_color", &subtitles.text_color)?;
    validate_hex_color("subtitles.background_color", &subtitles.background_color)?;
    validate_u16_range(
        "subtitles.background_opacity_percent",
        u16::from(subtitles.background_opacity_percent),
        u16::from(MIN_BACKGROUND_OPACITY_PERCENT),
        u16::from(MAX_BACKGROUND_OPACITY_PERCENT),
    )?;
    validate_u16_range(
        "subtitles.position_horizontal_percent",
        u16::from(subtitles.position_horizontal_percent),
        u16::from(MIN_POSITION_HORIZONTAL_PERCENT),
        u16::from(MAX_POSITION_HORIZONTAL_PERCENT),
    )?;
    validate_vertical_position(subtitles.position_vertical_percent)?;
    validate_preferred_languages(subtitles)?;
    validate_subtitle_limits(subtitles)
}

/// Вертикальное положение знаковое (`i16`), поэтому проверяется отдельно от беззнаковых помощников.
fn validate_vertical_position(value: i16) -> ConfigResult<()> {
    if (MIN_POSITION_VERTICAL_PERCENT..=MAX_POSITION_VERTICAL_PERCENT).contains(&value) {
        return Ok(());
    }
    Err(invalid_value(
        "subtitles.position_vertical_percent",
        format!(
            "the value must be between {MIN_POSITION_VERTICAL_PERCENT} and {MAX_POSITION_VERTICAL_PERCENT}; got {value}"
        ),
    ))
}

/// Проверяет числовые лимиты ресурсов.
fn validate_subtitle_limits(subtitles: &SubtitlesConfig) -> ConfigResult<()> {
    validate_u16_range(
        "subtitles.max_file_size_mb",
        subtitles.max_file_size_mb,
        MIN_MAX_FILE_SIZE_MB,
        MAX_MAX_FILE_SIZE_MB,
    )?;
    validate_u32_range(
        "subtitles.max_cues_per_file",
        subtitles.max_cues_per_file,
        MIN_MAX_CUES_PER_FILE,
        MAX_MAX_CUES_PER_FILE,
    )?;
    validate_u32_range(
        "subtitles.sidecar_max_entries_per_folder",
        subtitles.sidecar_max_entries_per_folder,
        MIN_SIDECAR_MAX_ENTRIES_PER_FOLDER,
        MAX_SIDECAR_MAX_ENTRIES_PER_FOLDER,
    )?;
    validate_u16_range(
        "subtitles.sidecar_max_candidates",
        subtitles.sidecar_max_candidates,
        MIN_SIDECAR_MAX_CANDIDATES,
        MAX_SIDECAR_MAX_CANDIDATES,
    )?;
    validate_u16_range(
        "subtitles.render_cache_mb",
        subtitles.render_cache_mb,
        MIN_RENDER_CACHE_MB,
        MAX_RENDER_CACHE_MB,
    )?;
    validate_u16_range(
        "subtitles.blur_scratch_retained_mb",
        subtitles.blur_scratch_retained_mb,
        MIN_BLUR_SCRATCH_RETAINED_MB,
        MAX_BLUR_SCRATCH_RETAINED_MB,
    )?;
    validate_u32_range(
        "subtitles.font_lookup_cache_chars",
        subtitles.font_lookup_cache_chars,
        MIN_FONT_LOOKUP_CACHE_CHARS,
        MAX_FONT_LOOKUP_CACHE_CHARS,
    )?;
    validate_u32_range(
        "subtitles.max_chars_per_cue",
        subtitles.max_chars_per_cue,
        MIN_MAX_CHARS_PER_CUE,
        MAX_MAX_CHARS_PER_CUE,
    )?;
    validate_u16_range(
        "subtitles.max_cues_on_screen",
        subtitles.max_cues_on_screen,
        MIN_MAX_CUES_ON_SCREEN,
        MAX_MAX_CUES_ON_SCREEN,
    )?;
    validate_embedded_track_limits(subtitles)
}

/// Лимиты встроенных дорожек: вложенные шрифты, труба пакетов и время показа реплики.
fn validate_embedded_track_limits(subtitles: &SubtitlesConfig) -> ConfigResult<()> {
    validate_u16_range(
        "subtitles.max_attached_font_mb",
        subtitles.max_attached_font_mb,
        MIN_MAX_ATTACHED_FONT_MB,
        MAX_MAX_ATTACHED_FONT_MB,
    )?;
    validate_u16_range(
        "subtitles.max_attached_fonts_total_mb",
        subtitles.max_attached_fonts_total_mb,
        MIN_MAX_ATTACHED_FONTS_TOTAL_MB,
        MAX_MAX_ATTACHED_FONTS_TOTAL_MB,
    )?;
    // Общий лимит меньше лимита одного вложения бессмысленен: ни одно вложение «до лимита» не влезло бы.
    if subtitles.max_attached_fonts_total_mb < subtitles.max_attached_font_mb {
        return Err(invalid_value(
            "subtitles.max_attached_fonts_total_mb",
            format!(
                "the total limit must not be smaller than subtitles.max_attached_font_mb ({}); got {}",
                subtitles.max_attached_font_mb, subtitles.max_attached_fonts_total_mb
            ),
        ));
    }
    validate_u32_range(
        "subtitles.embedded_feed_capacity",
        subtitles.embedded_feed_capacity,
        MIN_EMBEDDED_FEED_CAPACITY,
        MAX_EMBEDDED_FEED_CAPACITY,
    )?;
    validate_u32_range(
        "subtitles.embedded_cue_max_open_duration_ms",
        subtitles.embedded_cue_max_open_duration_ms,
        MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
        MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
    )?;
    validate_ass_limits(subtitles)?;
    validate_prerender_settings(subtitles)
}

/// Цвет обязан быть `#RRGGBB` (регистр не важен): так его разберёт растеризатор без догадок.
fn validate_hex_color(field: &'static str, value: &str) -> ConfigResult<()> {
    let is_valid = value.len() == HEX_COLOR_LEN
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if is_valid {
        return Ok(());
    }
    Err(invalid_value(
        field,
        format!("the color must look like #RRGGBB, for example #FFFFFF; got `{value}`"),
    ))
}

/// Проверяет список языков: пустой допустим (язык системы), иначе до 8 уникальных тегов.
fn validate_preferred_languages(subtitles: &SubtitlesConfig) -> ConfigResult<()> {
    const FIELD: &str = "subtitles.preferred_languages";
    let raw_list = &subtitles.preferred_languages;
    if raw_list.chars().count() > MAX_PREFERRED_LANGUAGES_LEN {
        return Err(invalid_value(
            FIELD,
            format!("the list must be at most {MAX_PREFERRED_LANGUAGES_LEN} characters long"),
        ));
    }
    if raw_list.trim().is_empty() {
        return Ok(());
    }

    let language_tags: Vec<&str> = raw_list.split(',').map(str::trim).collect();
    if language_tags.len() > MAX_PREFERRED_LANGUAGE_COUNT {
        return Err(invalid_value(
            FIELD,
            format!(
                "at most {MAX_PREFERRED_LANGUAGE_COUNT} languages are allowed; got {}",
                language_tags.len()
            ),
        ));
    }
    for (tag_index, language_tag) in language_tags.iter().enumerate() {
        if let Err(reason) = check_language_tag(language_tag) {
            return Err(invalid_value(
                FIELD,
                format!("language #{} `{language_tag}`: {reason}", tag_index + 1),
            ));
        }
        let is_duplicate = language_tags[..tag_index]
            .iter()
            .any(|earlier_tag| earlier_tag.eq_ignore_ascii_case(language_tag));
        if is_duplicate {
            return Err(invalid_value(
                FIELD,
                format!("language `{language_tag}` is listed more than once"),
            ));
        }
    }
    Ok(())
}

/// Проверяет один тег вида `uk`, `pt-BR`, `zh-Hans-CN`; возвращает причину отказа.
fn check_language_tag(language_tag: &str) -> Result<(), &'static str> {
    if language_tag.is_empty() {
        return Err("must not be empty");
    }
    if !(MIN_LANGUAGE_TAG_LEN..=MAX_LANGUAGE_TAG_LEN).contains(&language_tag.len()) {
        return Err("must be 2 to 35 characters long");
    }
    if !language_tag
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("may contain only ASCII letters, digits and `-`");
    }
    let mut subtags = language_tag.split('-');
    // Первый подтег — сам язык: 2-3 буквы (ISO 639) или 5-8 (зарезервированный/зарегистрированный).
    let primary_subtag = subtags.next().unwrap_or_default();
    let primary_is_letters = primary_subtag
        .bytes()
        .all(|byte| byte.is_ascii_alphabetic());
    let primary_len_is_valid = matches!(primary_subtag.len(), 2..=3 | 5..=8);
    if !primary_is_letters || !primary_len_is_valid {
        return Err("must start with a language code of 2-3 or 5-8 letters, such as `uk`");
    }
    if subtags.any(str::is_empty) {
        return Err("must not contain empty parts between hyphens");
    }
    Ok(())
}
