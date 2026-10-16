//! Комментарии к секции `[subtitles]` в сгенерированном default TOML.
//!
//! Отдельный модуль: ключ `enabled` встречается и в других секциях, поэтому
//! поиск строки здесь ограничен хвостом документа после заголовка `[subtitles]`.

/// Заголовок секции субтитров в сгенерированном TOML.
const SUBTITLES_HEADER: &str = "[subtitles]";

/// Пары «ключ -> комментарий» в порядке следования полей в `SubtitlesConfig`.
const SUBTITLES_KEY_COMMENTS: &[(&str, &str)] = &[
    (
        "enabled",
        "# Показывать субтитры автоматически, когда они есть; кнопка CC и клавиша C меняют это.",
    ),
    (
        "preferred_languages",
        "# Языки по приоритету через запятую, например \"uk, ru, en\"; пусто — язык системы.",
    ),
    (
        "decoration",
        "# Чем подчёркивается текст: box (тёмная плашка) или outline (обводка и тень).",
    ),
    (
        "text_size_percent",
        "# Размер текста в процентах от стандартного (50..=250).",
    ),
    ("text_color", "# Цвет текста в формате #RRGGBB."),
    ("background_color", "# Цвет плашки в формате #RRGGBB."),
    (
        "background_opacity_percent",
        "# Непрозрачность плашки в процентах (0..=100).",
    ),
    (
        "position_horizontal_percent",
        "# Центр субтитров по горизонтали в процентах ширины картинки (0..=100; 50 — по центру).",
    ),
    (
        "position_vertical_percent",
        "# Высота низа субтитров над низом картинки в процентах высоты (-40..=95); минус — на полосу под картинкой.",
    ),
    (
        "ass_force_user_style",
        "# Применять свои цвета и обводку также к ASS/SSA-субтитрам (работает, когда доступен ASS-рендер).",
    ),
    (
        "max_file_size_mb",
        "# Файлы субтитров больше этого размера (МБ) не загружаются.",
    ),
    (
        "max_cues_per_file",
        "# Файлы субтитров с большим числом реплик не загружаются.",
    ),
    (
        "sidecar_max_entries_per_folder",
        "# Сколько записей папки просматривается при поиске субтитров рядом с видео.",
    ),
    (
        "sidecar_max_candidates",
        "# Максимум найденных рядом файлов субтитров, предлагаемых как дорожки.",
    ),
    (
        "render_cache_mb",
        "# Память под кэш уже нарисованных реплик, МБ.",
    ),
    (
        "font_lookup_cache_chars",
        "# Сколько символов запоминает кэш поиска шрифта.",
    ),
    (
        "max_chars_per_cue",
        "# Реплики длиннее этого числа символов пропускаются.",
    ),
    (
        "max_cues_on_screen",
        "# Сколько реплик максимум показывается одновременно.",
    ),
    (
        "max_attached_font_mb",
        "# Вложенные в видео шрифты больше этого размера (МБ) не загружаются.",
    ),
    (
        "max_attached_fonts_total_mb",
        "# Сколько МБ вложенных шрифтов загружается из одного видео (не меньше лимита одного шрифта).",
    ),
    (
        "embedded_feed_capacity",
        "# Сколько пакетов встроенных субтитров ждёт интерфейс; лишние отбрасываются (действует при запуске плеера).",
    ),
    (
        "embedded_cue_max_open_duration_ms",
        "# Сколько мс показывается встроенная реплика без длительности.",
    ),
];

/// Добавляет комментарии перед ключами секции `[subtitles]`.
///
/// Если секции нет (не должно случаться для документа из `AppConfig`), текст не меняется;
/// золотой файл `current_schema_v17.toml` тогда провалит тест стабильности схемы.
pub(super) fn document_subtitles_defaults(toml_text: &mut String) {
    let Some(header_offset) = toml_text.find(&format!("\n{SUBTITLES_HEADER}\n")) else {
        return;
    };
    // Работаем только с хвостом от заголовка: ключи других секций не затрагиваются.
    let section_start = header_offset + 1;
    let mut section_text = toml_text.split_off(section_start);

    for (key, comment) in SUBTITLES_KEY_COMMENTS {
        if section_text.contains(comment) {
            continue;
        }
        let key_prefix = format!("\n{key} = ");
        if let Some(key_offset) = section_text.find(&key_prefix) {
            section_text.insert_str(key_offset + 1, &format!("{comment}\n"));
        }
    }

    toml_text.push_str(&section_text);
}
