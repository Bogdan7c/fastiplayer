//! Секция `[subtitles]`: показ, язык, стиль и лимиты ресурсов субтитров (схема v23).
//!
//! Решение владельца (субтитры, сессия 04): лимиты и внешний вид живут в `config.toml`,
//! значения по умолчанию равны прежним константам приложения, поэтому старый файл без
//! секции ведёт себя как раньше. Текст `*_ru` в metadata английский: UI только английский.

use serde::{Deserialize, Serialize};

use super::subtitles_ass::{
    AssPositionMode, DEFAULT_MAX_ASS_EVENTS_ON_SCREEN, DEFAULT_MAX_ASS_HEADER_KB,
    DEFAULT_MAX_ASS_OVERRIDE_TAGS_PER_EVENT, DEFAULT_MAX_ASS_STYLES,
};
use super::subtitles_glyph_cache::*;
use super::subtitles_limits::*;
use super::subtitles_pgs::*;
use super::subtitles_stream::SubtitleStreamConfig;
use super::subtitles_style::*;

/// Показ субтитров по умолчанию выключен: включает пользователь (кнопка CC / клавиша C).
pub const DEFAULT_SUBTITLES_ENABLED: bool = false;

/// Чем подчёркивается текст субтитров над видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleDecorationPreference {
    /// Тёмная полупрозрачная плашка за текстом.
    Box,
    /// Обводка и мягкая тень без плашки.
    Outline,
}

/// Настройки субтитров.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct SubtitlesConfig {
    /// Показывать субтитры автоматически, когда они есть (кнопка CC и клавиша C меняют это и запоминают).
    #[setting(
        id = "subtitles.enabled",
        path = "subtitles.enabled",
        section = "subtitles",
        group = "general",
        surface = "main-settings-window",
        label_id = "settings.subtitles.enabled.label",
        label_ru = "Show subtitles",
        description_id = "settings.subtitles.enabled.description",
        description_ru = "Show subtitles automatically when a video has them. The CC button and the C key change this and it is remembered.",
        editor = "toggle",
        apply = "subtitles.apply"
    )]
    pub enabled: bool,

    /// Список предпочитаемых языков через запятую (порядок = приоритет); пусто — язык системы.
    #[setting(
        id = "subtitles.preferred_languages",
        path = "subtitles.preferred_languages",
        section = "subtitles",
        group = "general",
        surface = "main-settings-window",
        label_id = "settings.subtitles.preferred_languages.label",
        label_ru = "Preferred languages",
        description_id = "settings.subtitles.preferred_languages.description",
        description_ru = "Language codes in order of preference, separated by commas, for example: uk, ru, en. Leave empty to use the system language.",
        editor = "text",
        min_len = crate::MIN_PREFERRED_LANGUAGES_LEN,
        max_len = crate::MAX_PREFERRED_LANGUAGES_LEN,
        apply = "subtitles.apply"
    )]
    pub preferred_languages: String,

    /// Чем подчёркивается текст: плашкой или обводкой с тенью.
    #[setting(
        id = "subtitles.decoration",
        path = "subtitles.decoration",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.decoration.label",
        label_ru = "Text background",
        description_id = "settings.subtitles.decoration.description",
        description_ru = "How subtitle text is made readable over the video.",
        editor = "select",
        options(
            option(id = "box", label_id = "settings.subtitles.decoration.box", label_ru = "Dark box", value = SubtitleDecorationPreference::Box),
            option(id = "outline", label_id = "settings.subtitles.decoration.outline", label_ru = "Outline", value = SubtitleDecorationPreference::Outline)
        ),
        apply = "subtitles.apply"
    )]
    pub decoration: SubtitleDecorationPreference,

    /// Размер текста в процентах от стандартного.
    #[setting(
        id = "subtitles.text_size_percent",
        path = "subtitles.text_size_percent",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.text_size_percent.label",
        label_ru = "Text size",
        description_id = "settings.subtitles.text_size_percent.description",
        description_ru = "Subtitle text size relative to the default size.",
        editor = "integer",
        min = crate::MIN_TEXT_SIZE_PERCENT,
        max = crate::MAX_TEXT_SIZE_PERCENT,
        step = 5,
        unit = "%",
        apply = "subtitles.apply"
    )]
    pub text_size_percent: u16,

    /// Цвет текста `#RRGGBB`.
    #[setting(
        id = "subtitles.text_color",
        path = "subtitles.text_color",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.text_color.label",
        label_ru = "Text color",
        description_id = "settings.subtitles.text_color.description",
        description_ru = "Subtitle text color as #RRGGBB, for example #FFFFFF.",
        editor = "text",
        min_len = crate::HEX_COLOR_LEN,
        max_len = crate::HEX_COLOR_LEN,
        apply = "subtitles.apply"
    )]
    pub text_color: String,

    /// Цвет плашки `#RRGGBB`.
    #[setting(
        id = "subtitles.background_color",
        path = "subtitles.background_color",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.background_color.label",
        label_ru = "Box color",
        description_id = "settings.subtitles.background_color.description",
        description_ru = "Color of the dark box behind the text as #RRGGBB, for example #080808.",
        editor = "text",
        min_len = crate::HEX_COLOR_LEN,
        max_len = crate::HEX_COLOR_LEN,
        apply = "subtitles.apply"
    )]
    pub background_color: String,

    /// Непрозрачность плашки в процентах.
    #[setting(
        id = "subtitles.background_opacity_percent",
        path = "subtitles.background_opacity_percent",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.background_opacity_percent.label",
        label_ru = "Box opacity",
        description_id = "settings.subtitles.background_opacity_percent.description",
        description_ru = "How opaque the box behind the text is; 0 makes it invisible.",
        editor = "integer",
        min = crate::MIN_BACKGROUND_OPACITY_PERCENT,
        max = crate::MAX_BACKGROUND_OPACITY_PERCENT,
        step = 5,
        unit = "%",
        apply = "subtitles.apply"
    )]
    pub background_opacity_percent: u8,

    /// Центр слоя субтитров по горизонтали, % ширины картинки.
    #[setting(
        id = "subtitles.position_horizontal_percent",
        path = "subtitles.position_horizontal_percent",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.position_horizontal_percent.label",
        label_ru = "Horizontal position",
        description_id = "settings.subtitles.position_horizontal_percent.description",
        description_ru = "Where the center of the subtitles sits across the picture: 0 is the left edge, 50 the middle, 100 the right edge.",
        editor = "integer",
        min = crate::MIN_POSITION_HORIZONTAL_PERCENT,
        max = crate::MAX_POSITION_HORIZONTAL_PERCENT,
        step = 1,
        unit = "%",
        apply = "subtitles.apply"
    )]
    pub position_horizontal_percent: u8,

    /// Низ субтитров над нижним краем картинки, % высоты (отрицательное = на полосе под картинкой).
    #[setting(
        id = "subtitles.position_vertical_percent",
        path = "subtitles.position_vertical_percent",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.position_vertical_percent.label",
        label_ru = "Vertical position",
        description_id = "settings.subtitles.position_vertical_percent.description",
        description_ru = "Height of the subtitles above the bottom edge of the picture. Negative values move them onto the black bar below the picture.",
        editor = "integer",
        min = crate::MIN_POSITION_VERTICAL_PERCENT,
        max = crate::MAX_POSITION_VERTICAL_PERCENT,
        step = 1,
        unit = "%",
        apply = "subtitles.apply"
    )]
    pub position_vertical_percent: i16,

    /// Применять свои цвета и обводку также к ASS/SSA-субтитрам.
    #[setting(
        id = "subtitles.ass_force_user_style",
        path = "subtitles.ass_force_user_style",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.ass_force_user_style.label",
        label_ru = "Override ASS/SSA styling",
        description_id = "settings.subtitles.ass_force_user_style.description",
        description_ru = "Use your colors and outline for ASS/SSA subtitles too. Takes effect when ASS rendering is available.",
        editor = "toggle",
        apply = "subtitles.apply"
    )]
    pub ass_force_user_style: bool,

    /// Как размещать ASS/SSA: поднимать диалог над панелью (надписи на месте), оставить как у
    /// автора или двигать слой целиком, как обычные субтитры.
    #[setting(
        id = "subtitles.ass_position_mode",
        path = "subtitles.ass_position_mode",
        section = "subtitles",
        group = "style",
        surface = "main-settings-window",
        label_id = "settings.subtitles.ass_position_mode.label",
        label_ru = "ASS/SSA position",
        description_id = "settings.subtitles.ass_position_mode.description",
        description_ru = "Lift dialogue moves only the main spoken lines above the controls while they are visible; signs and other positioned text stay where the author put them, and your position offset is not applied. Keep author placement moves nothing. Move the whole layer lifts everything like plain subtitles and applies your position offset. Subtitles never leave the picture.",
        editor = "select",
        apply = "subtitles.apply",
        options(
            option(id = "main_lines_avoid_controls", label_id = "settings.subtitles.ass_position_mode.main_lines_avoid_controls", label_ru = "Lift dialogue above controls (signs stay)", value = AssPositionMode::MainLinesAvoidControls),
            option(id = "author", label_id = "settings.subtitles.ass_position_mode.author", label_ru = "Keep author placement", value = AssPositionMode::Author),
            option(id = "whole_layer", label_id = "settings.subtitles.ass_position_mode.whole_layer", label_ru = "Move the whole layer", value = AssPositionMode::WholeLayer)
        )
    )]
    pub ass_position_mode: AssPositionMode,

    /// Максимальный размер файла субтитров, МБ.
    #[setting(
        id = "subtitles.max_file_size_mb",
        path = "subtitles.max_file_size_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_file_size_mb.label",
        label_ru = "Largest subtitle file",
        description_id = "settings.subtitles.max_file_size_mb.description",
        description_ru = "Subtitle files larger than this are not loaded.",
        editor = "integer",
        min = crate::MIN_MAX_FILE_SIZE_MB,
        max = crate::MAX_MAX_FILE_SIZE_MB,
        step = 1,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub max_file_size_mb: u16,

    /// Максимум реплик в одном файле.
    #[setting(
        id = "subtitles.max_cues_per_file",
        path = "subtitles.max_cues_per_file",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_cues_per_file.label",
        label_ru = "Most cues per file",
        description_id = "settings.subtitles.max_cues_per_file.description",
        description_ru = "Subtitle files with more lines than this are not loaded.",
        editor = "integer",
        min = crate::MIN_MAX_CUES_PER_FILE,
        max = crate::MAX_MAX_CUES_PER_FILE,
        step = 1000,
        unit = "cues",
        apply = "subtitles.apply"
    )]
    pub max_cues_per_file: u32,

    /// Сколько записей папки просматривается при поиске файлов рядом с видео.
    #[setting(
        id = "subtitles.sidecar_max_entries_per_folder",
        path = "subtitles.sidecar_max_entries_per_folder",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.sidecar_max_entries_per_folder.label",
        label_ru = "Folder entries to scan",
        description_id = "settings.subtitles.sidecar_max_entries_per_folder.description",
        description_ru = "How many entries of a folder are checked when looking for subtitle files next to the video.",
        editor = "integer",
        min = crate::MIN_SIDECAR_MAX_ENTRIES_PER_FOLDER,
        max = crate::MAX_SIDECAR_MAX_ENTRIES_PER_FOLDER,
        step = 100,
        unit = "entries",
        apply = "subtitles.apply"
    )]
    pub sidecar_max_entries_per_folder: u32,

    /// Максимум найденных рядом файлов субтитров.
    #[setting(
        id = "subtitles.sidecar_max_candidates",
        path = "subtitles.sidecar_max_candidates",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.sidecar_max_candidates.label",
        label_ru = "Subtitle files to offer",
        description_id = "settings.subtitles.sidecar_max_candidates.description",
        description_ru = "Most subtitle files found next to a video that are offered as tracks.",
        editor = "integer",
        min = crate::MIN_SIDECAR_MAX_CANDIDATES,
        max = crate::MAX_SIDECAR_MAX_CANDIDATES,
        step = 1,
        unit = "files",
        apply = "subtitles.apply"
    )]
    pub sidecar_max_candidates: u16,

    /// Кэш готовых картинок реплик, МБ.
    #[setting(
        id = "subtitles.render_cache_mb",
        path = "subtitles.render_cache_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.render_cache_mb.label",
        label_ru = "Subtitle image cache",
        description_id = "settings.subtitles.render_cache_mb.description",
        description_ru = "Memory used to keep already drawn subtitle images.",
        editor = "integer",
        min = crate::MIN_RENDER_CACHE_MB,
        max = crate::MAX_RENDER_CACHE_MB,
        step = 4,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub render_cache_mb: u16,

    /// Размер кэша поиска шрифта для символа.
    #[setting(
        id = "subtitles.font_lookup_cache_chars",
        path = "subtitles.font_lookup_cache_chars",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.font_lookup_cache_chars.label",
        label_ru = "Font lookup cache",
        description_id = "settings.subtitles.font_lookup_cache_chars.description",
        description_ru = "How many characters remember which font draws them.",
        editor = "integer",
        min = crate::MIN_FONT_LOOKUP_CACHE_CHARS,
        max = crate::MAX_FONT_LOOKUP_CACHE_CHARS,
        step = 1024,
        unit = "chars",
        apply = "subtitles.apply"
    )]
    pub font_lookup_cache_chars: u32,

    /// Максимум символов в одной реплике.
    #[setting(
        id = "subtitles.max_chars_per_cue",
        path = "subtitles.max_chars_per_cue",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_chars_per_cue.label",
        label_ru = "Longest cue",
        description_id = "settings.subtitles.max_chars_per_cue.description",
        description_ru = "Cues with more characters than this are skipped.",
        editor = "integer",
        min = crate::MIN_MAX_CHARS_PER_CUE,
        max = crate::MAX_MAX_CHARS_PER_CUE,
        step = 100,
        unit = "chars",
        apply = "subtitles.apply"
    )]
    pub max_chars_per_cue: u32,

    /// Максимум одновременно показанных реплик.
    #[setting(
        id = "subtitles.max_cues_on_screen",
        path = "subtitles.max_cues_on_screen",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_cues_on_screen.label",
        label_ru = "Most cues on screen",
        description_id = "settings.subtitles.max_cues_on_screen.description",
        description_ru = "Most subtitle cues shown at the same time.",
        editor = "integer",
        min = crate::MIN_MAX_CUES_ON_SCREEN,
        max = crate::MAX_MAX_CUES_ON_SCREEN,
        step = 1,
        unit = "cues",
        apply = "subtitles.apply"
    )]
    pub max_cues_on_screen: u16,

    /// Максимальный размер одного вложенного в видео файла (шрифта), МБ.
    #[setting(
        id = "subtitles.max_attached_font_mb",
        path = "subtitles.max_attached_font_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_attached_font_mb.label",
        label_ru = "Largest attached font",
        description_id = "settings.subtitles.max_attached_font_mb.description",
        description_ru = "Fonts attached inside a video file (MKV) that are larger than this are not loaded.",
        editor = "integer",
        min = crate::MIN_MAX_ATTACHED_FONT_MB,
        max = crate::MAX_MAX_ATTACHED_FONT_MB,
        step = 1,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub max_attached_font_mb: u16,

    /// Суммарный размер вложенных в одно видео файлов (шрифтов), МБ.
    #[setting(
        id = "subtitles.max_attached_fonts_total_mb",
        path = "subtitles.max_attached_fonts_total_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_attached_fonts_total_mb.label",
        label_ru = "Attached fonts per video",
        description_id = "settings.subtitles.max_attached_fonts_total_mb.description",
        description_ru = "Total size of fonts loaded from one video file. Must not be smaller than the largest attached font.",
        editor = "integer",
        min = crate::MIN_MAX_ATTACHED_FONTS_TOTAL_MB,
        max = crate::MAX_MAX_ATTACHED_FONTS_TOTAL_MB,
        step = 10,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub max_attached_fonts_total_mb: u16,

    /// Сколько пакетов встроенных субтитров помещается в трубу от плеера к интерфейсу.
    #[setting(
        id = "subtitles.embedded_feed_capacity",
        path = "subtitles.embedded_feed_capacity",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.embedded_feed_capacity.label",
        label_ru = "Embedded subtitle buffer",
        description_id = "settings.subtitles.embedded_feed_capacity.description",
        description_ru = "How many embedded subtitle packets can wait for the interface. Extra packets are dropped. Applies the next time the player starts.",
        editor = "integer",
        min = crate::MIN_EMBEDDED_FEED_CAPACITY,
        max = crate::MAX_EMBEDDED_FEED_CAPACITY,
        step = 256,
        unit = "packets",
        apply = "subtitles.apply"
    )]
    pub embedded_feed_capacity: u32,

    /// Сколько показывать встроенную реплику, у которой в файле нет длительности, мс.
    #[setting(
        id = "subtitles.embedded_cue_max_open_duration_ms",
        path = "subtitles.embedded_cue_max_open_duration_ms",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.embedded_cue_max_open_duration_ms.label",
        label_ru = "Embedded cue display time",
        description_id = "settings.subtitles.embedded_cue_max_open_duration_ms.description",
        description_ru = "How long an embedded subtitle stays on screen when the file gives it no duration.",
        editor = "integer",
        min = crate::MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
        max = crate::MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
        step = 500,
        unit = "ms",
        apply = "subtitles.apply"
    )]
    pub embedded_cue_max_open_duration_ms: u32,

    /// Размер заголовка ASS (стили и параметры), больше которого дорожка читается как простой текст, КБ.
    #[setting(
        id = "subtitles.max_ass_header_kb",
        path = "subtitles.max_ass_header_kb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_ass_header_kb.label",
        label_ru = "ASS header size limit",
        description_id = "settings.subtitles.max_ass_header_kb.description",
        description_ru = "Larger ASS/SSA headers are not parsed; the subtitles are shown as plain text instead. Applies to the next loaded file or video.",
        editor = "integer",
        min = crate::MIN_MAX_ASS_HEADER_KB,
        max = crate::MAX_MAX_ASS_HEADER_KB,
        step = 64,
        unit = "KB",
        apply = "subtitles.apply"
    )]
    pub max_ass_header_kb: u32,

    /// Сколько стилей читается из одного ASS-заголовка.
    #[setting(
        id = "subtitles.max_ass_styles",
        path = "subtitles.max_ass_styles",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_ass_styles.label",
        label_ru = "ASS style limit",
        description_id = "settings.subtitles.max_ass_styles.description",
        description_ru = "How many styles are read from one ASS/SSA header. Applies to the next loaded file or video.",
        editor = "integer",
        min = crate::MIN_MAX_ASS_STYLES,
        max = crate::MAX_MAX_ASS_STYLES,
        step = 10,
        unit = "styles",
        apply = "subtitles.apply"
    )]
    pub max_ass_styles: u32,

    /// Сколько тегов оформления читается из одной ASS-реплики.
    #[setting(
        id = "subtitles.max_ass_override_tags_per_event",
        path = "subtitles.max_ass_override_tags_per_event",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_ass_override_tags_per_event.label",
        label_ru = "ASS tags per line limit",
        description_id = "settings.subtitles.max_ass_override_tags_per_event.description",
        description_ru = "How many override tags are read from one ASS/SSA line; extra tags are ignored. Applies to the next loaded file or video.",
        editor = "integer",
        min = crate::MIN_MAX_ASS_OVERRIDE_TAGS_PER_EVENT,
        max = crate::MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT,
        step = 32,
        unit = "tags",
        apply = "subtitles.apply"
    )]
    pub max_ass_override_tags_per_event: u32,

    /// Сколько ASS-событий одновременно рисуется на кадре (лишние отбрасываются с верхних слоёв).
    #[setting(
        id = "subtitles.max_ass_events_on_screen",
        path = "subtitles.max_ass_events_on_screen",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.max_ass_events_on_screen.label",
        label_ru = "ASS lines on screen limit",
        description_id = "settings.subtitles.max_ass_events_on_screen.description",
        description_ru = "How many ASS/SSA lines are drawn at the same time; when there are more, the ones on the highest layers are skipped. Applies to the next frame.",
        editor = "integer",
        min = crate::MIN_MAX_ASS_EVENTS_ON_SCREEN,
        max = crate::MAX_MAX_ASS_EVENTS_ON_SCREEN,
        step = 16,
        unit = "lines",
        apply = "subtitles.apply"
    )]
    pub max_ass_events_on_screen: u32,

    /// На сколько кадров вперёд поток отрисовки готовит картинки субтитров (0 = только текущий).
    #[setting(
        id = "subtitles.prerender_frames",
        path = "subtitles.prerender_frames",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.prerender_frames.label",
        label_ru = "Subtitle prerender lookahead",
        description_id = "settings.subtitles.prerender_frames.description",
        description_ru = "How many frames ahead the subtitle render thread prepares pictures; 0 prepares only the current frame. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_PRERENDER_FRAMES,
        max = crate::MAX_PRERENDER_FRAMES,
        step = 1,
        unit = "frames",
        apply = "subtitles.apply"
    )]
    pub prerender_frames: u32,

    /// Размер очереди готовых картинок субтитров, ожидающих показа.
    #[setting(
        id = "subtitles.prepared_overlays",
        path = "subtitles.prepared_overlays",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.prepared_overlays.label",
        label_ru = "Prepared subtitle pictures",
        description_id = "settings.subtitles.prepared_overlays.description",
        description_ru = "Size of the queue of ready subtitle pictures waiting to be shown. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_PREPARED_OVERLAYS,
        max = crate::MAX_PREPARED_OVERLAYS,
        step = 1,
        unit = "pictures",
        apply = "subtitles.apply"
    )]
    pub prepared_overlays: u32,

    /// Сколько МБ рабочей памяти размытия растеризатор оставляет себе между кадрами.
    #[setting(
        id = "subtitles.blur_scratch_retained_mb",
        path = "subtitles.blur_scratch_retained_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.blur_scratch_retained_mb.label",
        label_ru = "Subtitle blur scratch memory",
        description_id = "settings.subtitles.blur_scratch_retained_mb.description",
        description_ru = "Working memory of the subtitle blur (\\blur, \\be) kept between frames; larger blurs free it after the frame. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_BLUR_SCRATCH_RETAINED_MB,
        max = crate::MAX_BLUR_SCRATCH_RETAINED_MB,
        step = 8,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub blur_scratch_retained_mb: u16,

    /// Общий объём кэшей масок глифов (маски букв и склейки участков), МБ.
    #[setting(
        id = "subtitles.glyph_cache_mb",
        path = "subtitles.glyph_cache_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.glyph_cache_mb.label",
        label_ru = "Subtitle glyph cache",
        description_id = "settings.subtitles.glyph_cache_mb.description",
        description_ru = "Memory of the subtitle glyph mask caches (two thirds letter masks, one third merged run masks); moving, fading and karaoke subtitles reuse them between frames. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_GLYPH_CACHE_MB,
        max = crate::MAX_GLYPH_CACHE_MB,
        step = 8,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub glyph_cache_mb: u16,

    /// Сколько контуров глифов, рисунков и обводок хранит кэш контуров.
    #[setting(
        id = "subtitles.glyph_outline_cache_items",
        path = "subtitles.glyph_outline_cache_items",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.glyph_outline_cache_items.label",
        label_ru = "Subtitle outline cache entries",
        description_id = "settings.subtitles.glyph_outline_cache_items.description",
        description_ru = "How many glyph outlines, drawings and borders the subtitle outline cache keeps. This counts entries, not memory: the limit is independent of the glyph cache size, and entries with very large drawings take more memory. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_GLYPH_OUTLINE_CACHE_ITEMS,
        max = crate::MAX_GLYPH_OUTLINE_CACHE_ITEMS,
        step = 256,
        unit = "entries",
        apply = "subtitles.apply"
    )]
    pub glyph_outline_cache_items: u32,

    /// Память кэша результатов шейпинга текста (участок -> глифы), МБ.
    #[setting(
        id = "subtitles.shaping_cache_mb",
        path = "subtitles.shaping_cache_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.shaping_cache_mb.label",
        label_ru = "Subtitle text shaping cache",
        description_id = "settings.subtitles.shaping_cache_mb.description",
        description_ru = "Memory of the cache of shaped text runs; moving, fading and karaoke subtitles with unchanged text skip shaping. When full, the least recently used runs are dropped. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_SHAPING_CACHE_MB,
        max = crate::MAX_SHAPING_CACHE_MB,
        step = 1,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub shaping_cache_mb: u16,

    /// Бюджет кэша показываемой дорожки PGS (картинки), МБ.
    #[setting(
        id = "subtitles.pgs_displayed_track_cache_mb",
        path = "subtitles.pgs_displayed_track_cache_mb",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.pgs_displayed_track_cache_mb.label",
        label_ru = "PGS shown track cache",
        description_id = "settings.subtitles.pgs_displayed_track_cache_mb.description",
        description_ru = "Memory for the picture subtitles (PGS) of the track being shown; rewinding shows them from the cache. When full, pictures farthest from the playback position are dropped. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_PGS_DISPLAYED_TRACK_CACHE_MB,
        max = crate::MAX_PGS_DISPLAYED_TRACK_CACHE_MB,
        step = 8,
        unit = "MB",
        apply = "subtitles.apply"
    )]
    pub pgs_displayed_track_cache_mb: u16,

    /// Сколько секунд перед позицией хранят фоновые дорожки PGS.
    #[setting(
        id = "subtitles.pgs_background_track_window_s",
        path = "subtitles.pgs_background_track_window_s",
        section = "subtitles",
        group = "limits",
        surface = "main-settings-window",
        label_id = "settings.subtitles.pgs_background_track_window_s.label",
        label_ru = "PGS other tracks window",
        description_id = "settings.subtitles.pgs_background_track_window_s.description",
        description_ru = "How many seconds before the playback position picture subtitle (PGS) tracks that are not shown keep, so switching to them shows the picture at once. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_PGS_BACKGROUND_TRACK_WINDOW_S,
        max = crate::MAX_PGS_BACKGROUND_TRACK_WINDOW_S,
        step = 10,
        unit = "s",
        apply = "subtitles.apply"
    )]
    pub pgs_background_track_window_s: u16,

    /// Лимиты субтитров потоковых источников (HLS и другие): окна, размеры, сбои, история live.
    #[serde(default)]
    #[setting(nested)]
    pub stream: SubtitleStreamConfig,
}

impl Default for SubtitlesConfig {
    /// Значения равны прежним константам приложения и пресету `youtube_like()`.
    fn default() -> Self {
        Self {
            enabled: DEFAULT_SUBTITLES_ENABLED,
            preferred_languages: String::new(),
            decoration: SubtitleDecorationPreference::Box,
            text_size_percent: DEFAULT_TEXT_SIZE_PERCENT,
            text_color: DEFAULT_TEXT_COLOR.to_string(),
            background_color: DEFAULT_BACKGROUND_COLOR.to_string(),
            background_opacity_percent: DEFAULT_BACKGROUND_OPACITY_PERCENT,
            position_horizontal_percent: DEFAULT_POSITION_HORIZONTAL_PERCENT,
            position_vertical_percent: DEFAULT_POSITION_VERTICAL_PERCENT,
            ass_force_user_style: false,
            ass_position_mode: AssPositionMode::MainLinesAvoidControls,
            max_file_size_mb: DEFAULT_MAX_FILE_SIZE_MB,
            max_cues_per_file: DEFAULT_MAX_CUES_PER_FILE,
            sidecar_max_entries_per_folder: DEFAULT_SIDECAR_MAX_ENTRIES_PER_FOLDER,
            sidecar_max_candidates: DEFAULT_SIDECAR_MAX_CANDIDATES,
            render_cache_mb: DEFAULT_RENDER_CACHE_MB,
            font_lookup_cache_chars: DEFAULT_FONT_LOOKUP_CACHE_CHARS,
            max_chars_per_cue: DEFAULT_MAX_CHARS_PER_CUE,
            max_cues_on_screen: DEFAULT_MAX_CUES_ON_SCREEN,
            max_attached_font_mb: DEFAULT_MAX_ATTACHED_FONT_MB,
            max_attached_fonts_total_mb: DEFAULT_MAX_ATTACHED_FONTS_TOTAL_MB,
            embedded_feed_capacity: DEFAULT_EMBEDDED_FEED_CAPACITY,
            embedded_cue_max_open_duration_ms: DEFAULT_EMBEDDED_CUE_MAX_OPEN_DURATION_MS,
            max_ass_header_kb: DEFAULT_MAX_ASS_HEADER_KB,
            max_ass_styles: DEFAULT_MAX_ASS_STYLES,
            max_ass_override_tags_per_event: DEFAULT_MAX_ASS_OVERRIDE_TAGS_PER_EVENT,
            max_ass_events_on_screen: DEFAULT_MAX_ASS_EVENTS_ON_SCREEN,
            prerender_frames: DEFAULT_PRERENDER_FRAMES,
            prepared_overlays: DEFAULT_PREPARED_OVERLAYS,
            blur_scratch_retained_mb: DEFAULT_BLUR_SCRATCH_RETAINED_MB,
            glyph_cache_mb: DEFAULT_GLYPH_CACHE_MB,
            glyph_outline_cache_items: DEFAULT_GLYPH_OUTLINE_CACHE_ITEMS,
            shaping_cache_mb: DEFAULT_SHAPING_CACHE_MB,
            pgs_displayed_track_cache_mb: DEFAULT_PGS_DISPLAYED_TRACK_CACHE_MB,
            pgs_background_track_window_s: DEFAULT_PGS_BACKGROUND_TRACK_WINDOW_S,
            stream: SubtitleStreamConfig::default(),
        }
    }
}

impl SubtitlesConfig {
    /// Предпочитаемые языки по порядку приоритета: обрезанные коды без пустых элементов.
    ///
    /// Формат уже проверен `validate()`; здесь только разбор. Пустая строка даёт пустой
    /// список — вызывающий код тогда берёт язык системы.
    pub fn preferred_language_codes(&self) -> Vec<&str> {
        self.preferred_languages
            .split(',')
            .map(str::trim)
            .filter(|language_code| !language_code.is_empty())
            .collect()
    }
}
