//! Секция `[subtitles]`: показ, язык, стиль и лимиты ресурсов субтитров (схема v16).
//!
//! Решение владельца (субтитры, сессия 04): лимиты и внешний вид живут в `config.toml`,
//! значения по умолчанию равны прежним константам приложения, поэтому старый файл без
//! секции ведёт себя как раньше. Текст `*_ru` в metadata английский: UI только английский.

use serde::{Deserialize, Serialize};

/// Показ субтитров по умолчанию выключен: включает пользователь (кнопка CC / клавиша C).
pub const DEFAULT_SUBTITLES_ENABLED: bool = false;

/// Минимальная длина списка языков (пустой список = язык системы).
pub const MIN_PREFERRED_LANGUAGES_LEN: usize = 0;
/// Максимальная длина строки со списком языков.
pub const MAX_PREFERRED_LANGUAGES_LEN: usize = 120;
/// Сколько языков можно перечислить.
pub const MAX_PREFERRED_LANGUAGE_COUNT: usize = 8;

/// Размер текста по умолчанию, % (100 = стандартный пресет).
pub const DEFAULT_TEXT_SIZE_PERCENT: u16 = 100;
/// Минимальный размер текста, %.
pub const MIN_TEXT_SIZE_PERCENT: u16 = 50;
/// Максимальный размер текста, %.
pub const MAX_TEXT_SIZE_PERCENT: u16 = 250;

/// Длина цвета `#RRGGBB` в символах.
pub const HEX_COLOR_LEN: usize = 7;
/// Цвет текста по умолчанию.
pub const DEFAULT_TEXT_COLOR: &str = "#FFFFFF";
/// Цвет плашки по умолчанию: совпадает с `SubtitleAppearance::youtube_like()` (8, 8, 8).
pub const DEFAULT_BACKGROUND_COLOR: &str = "#080808";

/// Непрозрачность плашки по умолчанию: альфа 191 из 255 = 75 %, как в `youtube_like()`.
pub const DEFAULT_BACKGROUND_OPACITY_PERCENT: u8 = 75;
/// Минимальная непрозрачность плашки (полностью прозрачная).
pub const MIN_BACKGROUND_OPACITY_PERCENT: u8 = 0;
/// Максимальная непрозрачность плашки.
pub const MAX_BACKGROUND_OPACITY_PERCENT: u8 = 100;

/// Положение по горизонтали по умолчанию: центр картинки, %.
pub const DEFAULT_POSITION_HORIZONTAL_PERCENT: u8 = 50;
/// Минимум положения по горизонтали (левый край картинки), %.
pub const MIN_POSITION_HORIZONTAL_PERCENT: u8 = 0;
/// Максимум положения по горизонтали (правый край картинки), %.
pub const MAX_POSITION_HORIZONTAL_PERCENT: u8 = 100;

/// Положение по вертикали по умолчанию: низ текста на 6 % высоты картинки выше её низа.
///
/// ВАЖНО: число обязано равняться нижнему отступу пресета растеризатора
/// (`CanvasFraction` 0.06 в `subtitle-raster`): приложение считает смещение как
/// `value/100 - отступ_пресета`, и при значении по умолчанию оно ровно нулевое.
/// Это закреплено тестом в `app-egui`.
pub const DEFAULT_POSITION_VERTICAL_PERCENT: i16 = 6;
/// Минимум положения по вертикали: отрицательное значение уводит текст на чёрную полосу под картинкой, %.
pub const MIN_POSITION_VERTICAL_PERCENT: i16 = -40;
/// Максимум положения по вертикали (почти верх картинки), %.
pub const MAX_POSITION_VERTICAL_PERCENT: i16 = 95;

/// Размер файла субтитров по умолчанию, МБ (прежняя константа 8 МиБ).
pub const DEFAULT_MAX_FILE_SIZE_MB: u16 = 8;
/// Минимальный лимит размера файла, МБ.
pub const MIN_MAX_FILE_SIZE_MB: u16 = 1;
/// Максимальный лимит размера файла, МБ.
pub const MAX_MAX_FILE_SIZE_MB: u16 = 64;

/// Реплик в файле по умолчанию.
pub const DEFAULT_MAX_CUES_PER_FILE: u32 = 100_000;
/// Минимум реплик в файле.
pub const MIN_MAX_CUES_PER_FILE: u32 = 1_000;
/// Максимум реплик в файле.
pub const MAX_MAX_CUES_PER_FILE: u32 = 1_000_000;

/// Записей папки при поиске файлов рядом, по умолчанию.
pub const DEFAULT_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 5_000;
/// Минимум записей папки при поиске.
pub const MIN_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 100;
/// Максимум записей папки при поиске.
pub const MAX_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 100_000;

/// Кандидатов-файлов рядом с видео, по умолчанию.
pub const DEFAULT_SIDECAR_MAX_CANDIDATES: u16 = 32;
/// Минимум кандидатов.
pub const MIN_SIDECAR_MAX_CANDIDATES: u16 = 1;
/// Максимум кандидатов.
pub const MAX_SIDECAR_MAX_CANDIDATES: u16 = 256;

/// Кэш картинок реплик по умолчанию, МБ.
pub const DEFAULT_RENDER_CACHE_MB: u16 = 32;
/// Минимальный кэш картинок, МБ.
pub const MIN_RENDER_CACHE_MB: u16 = 4;
/// Максимальный кэш картинок, МБ.
pub const MAX_RENDER_CACHE_MB: u16 = 512;

/// Кэш поиска шрифта по символу, по умолчанию.
pub const DEFAULT_FONT_LOOKUP_CACHE_CHARS: u32 = 65_536;
/// Минимум кэша поиска шрифта.
pub const MIN_FONT_LOOKUP_CACHE_CHARS: u32 = 1_024;
/// Максимум кэша поиска шрифта.
pub const MAX_FONT_LOOKUP_CACHE_CHARS: u32 = 1_048_576;

/// Символов в одной реплике, по умолчанию.
pub const DEFAULT_MAX_CHARS_PER_CUE: u32 = 2_000;
/// Минимум символов в реплике.
pub const MIN_MAX_CHARS_PER_CUE: u32 = 100;
/// Максимум символов в реплике.
pub const MAX_MAX_CHARS_PER_CUE: u32 = 20_000;

/// Одновременно показанных реплик, по умолчанию.
pub const DEFAULT_MAX_CUES_ON_SCREEN: u16 = 16;
/// Минимум одновременных реплик.
pub const MIN_MAX_CUES_ON_SCREEN: u16 = 1;
/// Максимум одновременных реплик.
pub const MAX_MAX_CUES_ON_SCREEN: u16 = 64;

/// Лимит одного вложенного шрифта по умолчанию, МБ (прежняя константа 20 МиБ в демуксере).
pub const DEFAULT_MAX_ATTACHED_FONT_MB: u16 = 20;
/// Минимальный лимит одного вложения, МБ.
pub const MIN_MAX_ATTACHED_FONT_MB: u16 = 1;
/// Максимальный лимит одного вложения, МБ.
pub const MAX_MAX_ATTACHED_FONT_MB: u16 = 256;

/// Суммарный лимит вложенных шрифтов одного видео по умолчанию, МБ (прежняя константа 100 МиБ).
pub const DEFAULT_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 100;
/// Минимальный суммарный лимит вложений, МБ.
pub const MIN_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 1;
/// Максимальный суммарный лимит вложений, МБ.
pub const MAX_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 1_024;

/// Ёмкость трубы встроенных субтитров (плеер -> интерфейс), пакетов, по умолчанию.
pub const DEFAULT_EMBEDDED_FEED_CAPACITY: u32 = 4_096;
/// Минимальная ёмкость трубы, пакетов.
pub const MIN_EMBEDDED_FEED_CAPACITY: u32 = 256;
/// Максимальная ёмкость трубы, пакетов.
pub const MAX_EMBEDDED_FEED_CAPACITY: u32 = 65_536;

/// Сколько показывать встроенную реплику без длительности, мс, по умолчанию.
pub const DEFAULT_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 5_000;
/// Минимальное время показа такой реплики, мс.
pub const MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 500;
/// Максимальное время показа такой реплики, мс.
pub const MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 60_000;

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
