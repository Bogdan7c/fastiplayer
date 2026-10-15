//! Счётчики растеризатора.

use crate::cache::CacheStatistics;
use crate::face_cache::FaceCacheStatistics;

/// Внутренние счётчики работы (владелец — растеризатор).
#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) rasterized_cues: u64,
    pub(super) failed_cues: u64,
    pub(super) rendered_frames: u64,
    pub(super) unchanged_frames: u64,
    pub(super) rejected_cues_too_long: u64,
    pub(super) dropped_cues_over_frame_limit: u64,
    pub(super) rejected_canvas: u64,
    pub(super) skipped_glyphs: u64,
    pub(super) skipped_borders: u64,
}

/// Снимок статистики растеризатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterStatistics {
    /// Кэш готовых реплик: попадания, промахи, вытеснения, байты масок.
    pub cue_cache: CacheStatistics,
    /// Кэш подготовленных лиц шрифта. Попадания считаются по обращениям стадий конвейера
    /// (одна реплика обращается к лицу несколько раз), поэтому их число — не число реплик.
    pub face_cache: FaceCacheStatistics,
    /// Сколько реплик реально растеризовано (промахи кэша без ошибок).
    pub rasterized_cues: u64,
    /// Сколько реплик пропущено из-за ошибки рисования.
    pub failed_cues: u64,
    /// Сколько кадров собрано заново.
    pub rendered_frames: u64,
    /// Сколько кадров выдано быстрым путём «ничего не изменилось» (без работы).
    pub unchanged_frames: u64,
    /// Сколько раз для символа не нашлось глифа ни в одном шрифте (рисуется `.notdef`).
    pub missing_glyph_lookups: u64,
    /// Реплик пропущено целиком: текст длиннее `max_chars_per_cue`.
    pub rejected_cues_too_long: u64,
    /// Реплик отброшено сверх `max_cues_per_frame` (суммарно по собранным кадрам).
    pub dropped_cues_over_frame_limit: u64,
    /// Кадров с холстом больше `max_canvas_side` (результат пустой).
    pub rejected_canvas: u64,
    /// Глифов, не нарисованных из-за ошибки чтения контура (считается при растеризации реплики,
    /// не при выдаче из кэша).
    pub skipped_glyphs: u64,
    /// Глифов, нарисованных без обводки из-за сбоя её построения.
    pub skipped_borders: u64,
    /// Лиц, признанных непригодными для рисования и исключённых из выбора шрифта.
    pub unusable_faces: u64,
    /// Лиц, которые не удалось разобрать как шрифт.
    pub unreadable_faces: u64,
    /// Байты масок ТЕКУЩЕГО кадра (то, что вернул последний `render`). Эти маски не входят в
    /// `cue_cache.current_bytes`, если реплика не помещается в кэш: кадр держит их сам,
    /// и их объём ограничен одним кадром.
    pub current_frame_bytes: usize,
}
