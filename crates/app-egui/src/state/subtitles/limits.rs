//! Лимиты и бюджеты субтитров: пользовательские значения берутся из `SubtitlesConfig`
//! (секция `[subtitles]` конфига v13), здесь только перевод единиц и внутренние инварианты.
//!
//! # Что в конфиге, а что нет
//! В конфиге лежит то, что пользователь может осмысленно менять (размер файла, число реплик,
//! кэши, поиск рядом). Константы ниже НЕ вынесены в конфиг сознательно: это инварианты
//! реализации, неверное значение которых ломает работу, а не «настраивает» её.

use std::time::Duration;

use fastiplayer_config::SubtitlesConfig;
use subtitle_formats::SubtitleParseLimits;
use subtitle_raster::{FontLookupCacheLimits, RasterCacheLimits, RasterInputLimits, RasterLimits};
use subtitle_sidecar::SidecarSearchLimits;

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

/// Бюджет одной растеризации на потоке UI. Растеризация идёт синхронно перед рендером и
/// только при смене реплик/холста/стиля; дольше бюджета — `tracing::warn` (без текста реплик),
/// чтобы в логах было видно, когда пора выносить её в фон. Это порог диагностики.
pub(super) const RASTERIZATION_TIME_BUDGET: Duration = Duration::from_millis(8);

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
            parse: SubtitleParseLimits::new(
                file_max_bytes,
                widen(config.max_cues_per_file),
                SUBTITLE_FILE_MAX_REPORTED_ISSUES,
            ),
            file_max_bytes,
            font_lookup_cache: FontLookupCacheLimits {
                max_cached_chars: widen(config.font_lookup_cache_chars),
            },
        }
    }
}

/// Лимиты растеризатора из конфига. Действуют с создания растеризатора (см.
/// `SubtitleOwner::apply_config`: смена этих значений пересоздаёт его со следующим media).
pub(super) fn raster_limits_from_config(config: &SubtitlesConfig) -> RasterLimits {
    RasterLimits {
        cache: RasterCacheLimits {
            cue_bitmaps_bytes: usize::from(config.render_cache_mb)
                .saturating_mul(BYTES_PER_MEGABYTE),
            shaper_faces: RASTER_SHAPER_FACES,
        },
        input: RasterInputLimits {
            max_chars_per_cue: widen(config.max_chars_per_cue),
            max_cues_per_frame: usize::from(config.max_cues_on_screen),
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
        assert_eq!(raster.input.max_chars_per_cue, 2_000);
        assert_eq!(raster.input.max_cues_per_frame, 16);
    }

    #[test]
    fn changed_config_values_reach_the_limits() {
        let config = SubtitlesConfig {
            max_file_size_mb: 2,
            max_cues_per_file: 1_500,
            sidecar_max_candidates: 3,
            render_cache_mb: 4,
            max_cues_on_screen: 2,
            ..SubtitlesConfig::default()
        };
        let load = LoadLimits::from_config(&config);
        assert_eq!(load.file_max_bytes, 2 * 1024 * 1024);
        assert_eq!(load.parse.max_events, 1_500);
        assert_eq!(load.search.max_candidates, 3);
        let raster = raster_limits_from_config(&config);
        assert_eq!(raster.cache.cue_bitmaps_bytes, 4 * 1024 * 1024);
        assert_eq!(raster.input.max_cues_per_frame, 2);
    }
}
