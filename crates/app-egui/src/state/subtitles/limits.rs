//! Все лимиты и бюджеты субтитров в одном месте.
//!
//! ВРЕМЕННО: в сессии 04 плана субтитров эти значения переедут в конфиг приложения
//! (схема конфига в сессии 03 сознательно не меняется). Пока они именованные константы,
//! чтобы ни один лимит не прятался числом внутри кода загрузки или рисования.

use std::time::Duration;

use subtitle_formats::SubtitleParseLimits;
use subtitle_raster::{FontLookupCacheLimits, RasterCacheLimits, RasterInputLimits, RasterLimits};
use subtitle_sidecar::SidecarSearchLimits;

/// Максимальный размер файла субтитров в байтах. Файл больше не читается вовсе:
/// дорожка помечается недоступной, воспроизведение не страдает.
pub(super) const SUBTITLE_FILE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Максимум реплик в одном файле (защита от враждебного файла).
const SUBTITLE_FILE_MAX_EVENTS: usize = 100_000;

/// Сколько проблем разбора попадает в отчёт (остальные только считаются).
const SUBTITLE_FILE_MAX_REPORTED_ISSUES: usize = 64;

/// Лимиты разбора текстового файла субтитров.
pub(super) const SUBTITLE_PARSE_LIMITS: SubtitleParseLimits = SubtitleParseLimits::new(
    SUBTITLE_FILE_MAX_BYTES,
    SUBTITLE_FILE_MAX_EVENTS,
    SUBTITLE_FILE_MAX_REPORTED_ISSUES,
);

/// Лимиты поиска файлов рядом с видео: каталог видео, `Subs/`, `Subs/<Movie>/`.
pub(super) const SIDECAR_SEARCH_LIMITS: SidecarSearchLimits = SidecarSearchLimits {
    max_directory_entries_scanned_per_directory: 5_000,
    max_candidates: 32,
    max_subdirectory_depth: 2,
};

/// Кэш готовых реплик растеризатора: байты масок.
const RASTER_CUE_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Сколько подготовленных лиц шрифта держит растеризатор.
const RASTER_SHAPER_FACES: usize = 16;

/// Максимум символов в одной реплике; длиннее — реплика пропускается.
const RASTER_MAX_CHARS_PER_CUE: usize = 2_000;

/// Максимум одновременно показанных реплик на кадре.
const RASTER_MAX_CUES_PER_FRAME: usize = 16;

/// Максимальная сторона холста: заведомо больше любого реального экрана.
const RASTER_MAX_CANVAS_SIDE: u32 = 8_192;

/// Лимиты растеризатора (кэши и вход).
pub(super) const RASTER_LIMITS: RasterLimits = RasterLimits {
    cache: RasterCacheLimits {
        cue_bitmaps_bytes: RASTER_CUE_CACHE_BYTES,
        shaper_faces: RASTER_SHAPER_FACES,
    },
    input: RasterInputLimits {
        max_chars_per_cue: RASTER_MAX_CHARS_PER_CUE,
        max_cues_per_frame: RASTER_MAX_CUES_PER_FRAME,
        max_canvas_side: RASTER_MAX_CANVAS_SIDE,
    },
};

/// Размер кэша поиска «символ -> лицо» библиотеки шрифтов.
pub(super) const FONT_LOOKUP_CACHE_LIMITS: FontLookupCacheLimits = FontLookupCacheLimits {
    max_cached_chars: 65_536,
};

/// Бюджет одной растеризации на потоке UI. Растеризация идёт синхронно перед рендером и
/// только при смене реплик/холста/стиля; дольше бюджета — `tracing::warn` (без текста реплик),
/// чтобы в логах было видно, когда пора выносить её в фон.
pub(super) const RASTERIZATION_TIME_BUDGET: Duration = Duration::from_millis(8);
