//! Лимиты и бюджеты субтитров: пользовательские значения берутся из `SubtitlesConfig`
//! (секция `[subtitles]` конфига), здесь только перевод единиц и внутренние инварианты.
//!
//! # Что в конфиге, а что нет
//! В конфиге лежит то, что пользователь может осмысленно менять (размер файла, число реплик,
//! кэши, поиск рядом). Константы ниже НЕ вынесены в конфиг сознательно: это инварианты
//! реализации, неверное значение которых ломает работу, а не «настраивает» её.

use std::time::Duration;

use fastiplayer_config::SubtitlesConfig;
use subtitle_formats::{AssParseLimits, EmbeddedCueLimits, SubtitleParseLimits};
use subtitle_raster::{
    AttachedFontLimits, FontLookupCacheLimits, RasterCacheLimits, RasterInputLimits, RasterLimits,
};
use subtitle_sidecar::SidecarSearchLimits;

mod ass;

use self::ass::ass_parse_limits_from_config;

/// Байтов в мегабайте конфига (МБ = МиБ, как в прежней константе «8 МиБ»).
const BYTES_PER_MEGABYTE: usize = 1024 * 1024;

/// Сколько проблем разбора попадает в отчёт (остальные только считаются). Отчёт нужен
/// диагностике, а не пользователю, поэтому значение не настраивается.
const SUBTITLE_FILE_MAX_REPORTED_ISSUES: usize = 64;

/// Глубина поиска рядом с видео: папка видео, `Subs/`, `Subs/<Movie>/` (решение владельца 13).
/// Это сама форма правил поиска, а не лимит ресурсов, поэтому в конфиг не выносится.
const SIDECAR_SEARCH_MAX_SUBDIRECTORY_DEPTH: u8 = 2;

/// Сколько подготовленных лиц шрифта держит растеризатор. Внутренняя настройка кэша
/// форм-шейпера: на практике хватает нескольких лиц, а лишние только занимают память.
const RASTER_SHAPER_FACES: usize = 16;

/// Максимальная сторона холста: заведомо больше любого реального экрана. Защита от
/// мусорного размера картинки, а не пользовательская настройка.
const RASTER_MAX_CANVAS_SIDE: u32 = 8_192;

/// Сколько лиц суммарно принимает библиотека шрифтов из вложений одного видео. Защита от
/// коллекций `.ttc` с тысячами лиц: размер файла лимитирован конфигом, а число лиц — нет,
/// поэтому это инвариант реализации, а не пользовательская настройка.
const ATTACHED_FONT_MAX_FACES: usize = 256;

/// Худший случай UTF-8: байт на один символ (для предела размера пакета).
const MAX_BYTES_PER_CHAR: usize = 4;

/// Запас пакета на служебные поля: девять полей строки ASS, блоки `styl` mov_text, теги
/// разметки. Если текст пакета и так укладывается в `max_chars`, лишнее отсечёт второй предел.
const EMBEDDED_PAYLOAD_OVERHEAD_BYTES: usize = 4_096;

/// Всё, что нужно фоновой задаче, чтобы найти, прочитать и разобрать файл.
///
/// Копируется в задачу целиком в момент её запуска: смена настроек во время работы задачи
/// её не затрагивает, а действует со следующей загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LoadLimits {
    /// Лимиты поиска файлов рядом с видео.
    pub(super) search: SidecarSearchLimits,
    /// Лимиты разбора текстового файла (байты, реплики, длина отчёта).
    pub(super) parse: SubtitleParseLimits,
    /// Максимальный размер файла в байтах (тот же, что в `parse.max_input_bytes`).
    pub(super) file_max_bytes: usize,
    /// Размер кэша поиска «символ -> лицо» библиотеки шрифтов.
    pub(super) font_lookup_cache: FontLookupCacheLimits,
}

impl LoadLimits {
    /// Строит лимиты загрузки из конфига; поля конфига уже проверены `validate()`.
    pub(super) fn from_config(config: &SubtitlesConfig) -> Self {
        let file_max_bytes =
            usize::from(config.max_file_size_mb).saturating_mul(BYTES_PER_MEGABYTE);
        Self {
            search: SidecarSearchLimits {
                max_directory_entries_scanned_per_directory: widen(
                    config.sidecar_max_entries_per_folder,
                ),
                max_candidates: usize::from(config.sidecar_max_candidates),
                max_subdirectory_depth: SIDECAR_SEARCH_MAX_SUBDIRECTORY_DEPTH,
            },
            // Без лимитов ASS загрузчик отказал бы `.ass/.ssa` (`AssLimitsMissing`).
            parse: SubtitleParseLimits::new(
                file_max_bytes,
                widen(config.max_cues_per_file),
                SUBTITLE_FILE_MAX_REPORTED_ISSUES,
            )
            .with_ass_limits(ass_parse_limits_from_config(config)),
            file_max_bytes,
            font_lookup_cache: FontLookupCacheLimits {
                max_cached_chars: widen(config.font_lookup_cache_chars),
            },
        }
    }
}

/// Лимиты встроенных дорожек: вложенные шрифты и время показа реплики без длительности.
///
/// Строится из конфига в момент, когда владелец начинает работать с новым media; смена
/// настроек во время воспроизведения действует со следующего media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EmbeddedLimits {
    /// Лимиты регистрации вложенных шрифтов в растеризатор.
    pub(super) attached_fonts: AttachedFontLimits,
    /// Сколько показывать реплику встроенной дорожки, у которой в пакете нет длительности.
    pub(super) cue_max_open_duration: Duration,
    /// Пределы разбора одного пакета (защита от враждебных пакетов).
    pub(super) cue_parse: EmbeddedCueLimits,
    /// Сколько реплик хранит кэш одной встроенной дорожки (`max_cues_per_file`).
    pub(super) max_cues_per_track: usize,
    /// Пределы разбора ASS: заголовка дорожки и тегов реплики.
    pub(super) ass_parse: AssParseLimits,
}

impl EmbeddedLimits {
    /// Строит лимиты из конфига; поля уже проверены `validate()`.
    pub(super) fn from_config(config: &SubtitlesConfig) -> Self {
        Self {
            attached_fonts: AttachedFontLimits {
                max_font_bytes: megabytes_to_bytes(config.max_attached_font_mb),
                max_total_bytes: megabytes_to_bytes(config.max_attached_fonts_total_mb),
                max_faces: ATTACHED_FONT_MAX_FACES,
            },
            cue_max_open_duration: Duration::from_millis(u64::from(
                config.embedded_cue_max_open_duration_ms,
            )),
            cue_parse: embedded_cue_limits(widen(config.max_chars_per_cue)),
            max_cues_per_track: widen(config.max_cues_per_file),
            ass_parse: ass_parse_limits_from_config(config),
        }
    }
}

/// Пределы разбора пакета: символы — из конфига, байты — с запасом на кодировку и служебные
/// поля (UTF-8 до 4 байт на символ; ASS-строка и `styl` mov_text добавляют свою разметку).
fn embedded_cue_limits(max_chars: usize) -> EmbeddedCueLimits {
    let max_payload_bytes = max_chars
        .saturating_mul(MAX_BYTES_PER_CHAR)
        .saturating_add(EMBEDDED_PAYLOAD_OVERHEAD_BYTES);
    EmbeddedCueLimits::new(max_payload_bytes, max_chars)
}

/// Мегабайты конфига -> байты (насыщающее умножение, без паники).
fn megabytes_to_bytes(megabytes: u16) -> usize {
    usize::from(megabytes).saturating_mul(BYTES_PER_MEGABYTE)
}

/// Байты кэша масок отдельных глифов: две трети общего объёма (как деление 2:1 в libass).
fn glyph_bitmaps_share(glyph_cache_mb: u16) -> usize {
    megabytes_to_bytes(glyph_cache_mb) / 3 * 2
}

/// Байты кэша склеек участков: остаток общего объёма (треть, округление в его пользу, чтобы
/// сумма долей равнялась объёму из конфига).
fn glyph_composites_share(glyph_cache_mb: u16) -> usize {
    megabytes_to_bytes(glyph_cache_mb) - glyph_bitmaps_share(glyph_cache_mb)
}

/// Лимиты растеризатора из конфига. Действуют с создания растеризатора (см.
/// `SubtitleOwner::apply_config`: смена этих значений пересоздаёт его со следующим media).
pub(super) fn raster_limits_from_config(config: &SubtitlesConfig) -> RasterLimits {
    RasterLimits {
        cache: RasterCacheLimits {
            cue_bitmaps_bytes: usize::from(config.render_cache_mb)
                .saturating_mul(BYTES_PER_MEGABYTE),
            shaper_faces: RASTER_SHAPER_FACES,
            blur_scratch_retained_bytes: usize::from(config.blur_scratch_retained_mb)
                .saturating_mul(BYTES_PER_MEGABYTE),
            glyph_bitmaps_bytes: glyph_bitmaps_share(config.glyph_cache_mb),
            glyph_composites_bytes: glyph_composites_share(config.glyph_cache_mb),
            glyph_outline_items: widen(config.glyph_outline_cache_items),
            shaping_cache_bytes: megabytes_to_bytes(config.shaping_cache_mb),
        },
        input: RasterInputLimits {
            max_chars_per_cue: widen(config.max_chars_per_cue),
            max_cues_per_frame: usize::from(config.max_cues_on_screen),
            max_ass_events_per_frame: widen(config.max_ass_events_on_screen),
            max_canvas_side: RASTER_MAX_CANVAS_SIDE,
        },
    }
}

/// `u32` -> `usize` без потерь на поддерживаемых платформах (на 16-битных насыщается).
fn widen(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_gives_the_former_hard_coded_limits() {
        let config = SubtitlesConfig::default();
        let load = LoadLimits::from_config(&config);
        assert_eq!(load.file_max_bytes, 8 * 1024 * 1024);
        assert_eq!(load.parse.max_input_bytes, 8 * 1024 * 1024);
        assert_eq!(load.parse.max_events, 100_000);
        assert_eq!(load.search.max_candidates, 32);
        assert_eq!(
            load.search.max_directory_entries_scanned_per_directory,
            5_000
        );
        assert_eq!(load.font_lookup_cache.max_cached_chars, 65_536);

        let raster = raster_limits_from_config(&config);
        assert_eq!(raster.cache.cue_bitmaps_bytes, 32 * 1024 * 1024);
        // Дефолт конфига равен прежней константе растеризатора (32 МиБ).
        assert_eq!(raster.cache.blur_scratch_retained_bytes, 32 * 1024 * 1024);
        // Кэши глифов: 64 МБ делятся 2:1, контуры считаются штуками (как в libass).
        assert_eq!(
            raster.cache.glyph_bitmaps_bytes + raster.cache.glyph_composites_bytes,
            64 * 1024 * 1024
        );
        assert_eq!(raster.cache.glyph_bitmaps_bytes, 44_739_242);
        assert_eq!(raster.cache.glyph_composites_bytes, 22_369_622);
        assert_eq!(raster.cache.glyph_outline_items, 10_000);
        assert_eq!(raster.cache.shaping_cache_bytes, 4 * 1024 * 1024);
        assert_eq!(raster.input.max_chars_per_cue, 2_000);
        assert_eq!(raster.input.max_cues_per_frame, 16);
        assert_eq!(raster.input.max_ass_events_per_frame, 256);
    }

    #[test]
    fn changed_config_values_reach_the_limits() {
        let config = SubtitlesConfig {
            max_file_size_mb: 2,
            max_cues_per_file: 1_500,
            sidecar_max_candidates: 3,
            render_cache_mb: 4,
            blur_scratch_retained_mb: 7,
            glyph_cache_mb: 9,
            glyph_outline_cache_items: 512,
            shaping_cache_mb: 3,
            max_cues_on_screen: 2,
            max_ass_events_on_screen: 32,
            ..SubtitlesConfig::default()
        };
        let load = LoadLimits::from_config(&config);
        assert_eq!(load.file_max_bytes, 2 * 1024 * 1024);
        assert_eq!(load.parse.max_events, 1_500);
        assert_eq!(load.search.max_candidates, 3);
        let raster = raster_limits_from_config(&config);
        assert_eq!(raster.cache.cue_bitmaps_bytes, 4 * 1024 * 1024);
        assert_eq!(raster.cache.blur_scratch_retained_bytes, 7 * 1024 * 1024);
        assert_eq!(raster.cache.glyph_bitmaps_bytes, 6 * 1024 * 1024);
        assert_eq!(raster.cache.glyph_composites_bytes, 3 * 1024 * 1024);
        assert_eq!(raster.cache.glyph_outline_items, 512);
        assert_eq!(raster.cache.shaping_cache_bytes, 3 * 1024 * 1024);
        assert_eq!(raster.input.max_cues_per_frame, 2);
        assert_eq!(raster.input.max_ass_events_per_frame, 32);
    }

    #[test]
    fn default_config_gives_former_attachment_limits_and_cue_duration() {
        let limits = EmbeddedLimits::from_config(&SubtitlesConfig::default());

        assert_eq!(limits.attached_fonts.max_font_bytes, 20 * 1024 * 1024);
        assert_eq!(limits.attached_fonts.max_total_bytes, 100 * 1024 * 1024);
        assert_eq!(limits.attached_fonts.max_faces, ATTACHED_FONT_MAX_FACES);
        assert_eq!(limits.cue_max_open_duration, Duration::from_millis(5_000));
        assert_eq!(limits.cue_parse.max_chars, 2_000);
        assert_eq!(limits.cue_parse.max_payload_bytes, 2_000 * 4 + 4_096);
        assert_eq!(limits.max_cues_per_track, 100_000);
    }

    #[test]
    fn changed_embedded_config_values_reach_the_limits() {
        let config = SubtitlesConfig {
            max_attached_font_mb: 2,
            max_attached_fonts_total_mb: 6,
            embedded_cue_max_open_duration_ms: 1_500,
            ..SubtitlesConfig::default()
        };

        let limits = EmbeddedLimits::from_config(&config);

        assert_eq!(limits.attached_fonts.max_font_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.attached_fonts.max_total_bytes, 6 * 1024 * 1024);
        assert_eq!(limits.cue_max_open_duration, Duration::from_millis(1_500));
    }

    #[test]
    fn embedded_limits_do_not_touch_file_loading_limits() {
        let config = SubtitlesConfig {
            max_attached_font_mb: 2,
            ..SubtitlesConfig::default()
        };

        assert_eq!(
            LoadLimits::from_config(&config),
            LoadLimits::from_config(&SubtitlesConfig::default())
        );
    }
}
