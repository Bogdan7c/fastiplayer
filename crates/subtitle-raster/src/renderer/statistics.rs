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
    pub(super) dropped_ass_events_over_frame_limit: u64,
    pub(super) rejected_canvas: u64,
    pub(super) skipped_glyphs: u64,
    pub(super) skipped_borders: u64,
    pub(super) invalid_ass_drawings: u64,
    pub(super) skipped_oversized_bitmaps: u64,
    pub(super) failed_blur_layers: u64,
    pub(super) rejected_pictures_too_large: u64,
    pub(super) failed_pictures: u64,
    pub(super) damaged_pictures: u64,
    pub(super) dropped_pictures_over_frame_limit: u64,
}

/// Снимок статистики растеризатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterStatistics {
    /// Кэш готовых реплик: попадания, промахи, вытеснения, байты масок.
    pub cue_cache: CacheStatistics,
    /// Кэш подготовленных лиц шрифта. Попадания считаются по обращениям стадий конвейера
    /// (одна реплика обращается к лицу несколько раз), поэтому их число — не число реплик.
    pub face_cache: FaceCacheStatistics,
    /// Кэш контуров глифов, рисунков и обводок (libass: `outline_cache`). Поля `hits` и `misses`
    /// считают обращения, `current_bytes` и `evictions` - ШТУКИ (кэш считается в записях, а не
    /// в байтах), `entries` - число записей сейчас. Повторы одного глифа внутри реплики тоже
    /// считаются попаданиями.
    pub outline_cache: CacheStatistics,
    /// Кэш масок отдельных глифов (libass: `bitmap_cache`). Промах - это одна растеризация
    /// контура; движение реплики на целое число пикселей не должно давать промахов.
    /// `current_bytes` - байты масок и служебные расходы записей.
    pub glyph_bitmap_cache: CacheStatistics,
    /// Кэш склеек участков (libass: `composite_cache`): промах - сложение, размытие и сдвиг тени
    /// для одного участка. `current_bytes` - байты масок и служебные расходы записей.
    pub composite_cache: CacheStatistics,
    /// Кэш результатов шейпинга участков текста (один на все лица). Промах - один вызов
    /// `harfrust`; `current_bytes` - учтённые байты, не больше `shaping_cache_bytes`.
    pub shaping_cache: CacheStatistics,
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
    /// Реплик простых форматов отброшено сверх `max_cues_per_frame` (суммарно по собранным кадрам).
    pub dropped_cues_over_frame_limit: u64,
    /// ASS-событий отброшено сверх `max_ass_events_per_frame` (суммарно по собранным кадрам).
    pub dropped_ass_events_over_frame_limit: u64,
    /// Кадров с холстом больше `max_canvas_side` (результат пустой).
    pub rejected_canvas: u64,
    /// Глифов, не нарисованных из-за ошибки чтения контура (считается при растеризации реплики,
    /// не при выдаче из кэша).
    pub skipped_glyphs: u64,
    /// Глифов, нарисованных без обводки из-за сбоя её построения.
    pub skipped_borders: u64,
    /// Рисунков ASS (`\p`), не давших ни одного контура: команды не распознаны (рисунок
    /// отвергнут, в нём нет перемещения `m` или оно без чисел) либо координата вне допустимого
    /// диапазона. Рисунок остаётся пустым псевдоглифом, остальной текст реплики рисуется
    /// (считается при растеризации реплики, не при выдаче из кэша). Сюда же попадает отвергнутый
    /// рисунок векторного клипа `\clip(...)`: клип тогда не применяется, как в libass.
    pub invalid_ass_drawings: u64,
    /// Глифов, пропущенных, потому что битмап после преобразования (3D-перспектива у плоскости
    /// камеры) шире `RasterInputLimits::max_canvas_side` (считается при растеризации реплики);
    /// а также слоёв, которые не удалось размыть из-за этого предела (слой остаётся резким).
    /// Маска векторного клипа сюда не попадает: она растеризуется только в границах холста.
    pub skipped_oversized_bitmaps: u64,
    /// Слоёв (заливка, обводка), оставшихся неразмытыми из-за нарушенного внутреннего инварианта
    /// размытия (недопустимая дисперсия или несогласованная геометрия битмапа). Не путать с
    /// `skipped_oversized_bitmaps`: там штатная защита по размеру, здесь ошибка, которой в
    /// исправной сборке не бывает (считается при растеризации реплики).
    pub failed_blur_layers: u64,
    /// Растровых реплик (PGS) пропущено целиком: размер автора или картинки больше
    /// `max_canvas_side`. Остальные реплики кадра рисуются.
    pub rejected_pictures_too_large: u64,
    /// Растровых реплик пропущено из-за сбоя распаковки (неизвестная кодировка, нарушенная
    /// согласованность размеров). Остальные реплики кадра рисуются.
    pub failed_pictures: u64,
    /// Растровых реплик с повреждённым потоком RLE (обрезан или с лишними пикселями): картинка
    /// нарисована, недостающее дополнено прозрачным (считается при растеризации, не при выдаче
    /// из кэша).
    pub damaged_pictures: u64,
    /// Растровых реплик отброшено сверх `max_cues_per_frame` (суммарно по собранным кадрам).
    pub dropped_pictures_over_frame_limit: u64,
    /// Лиц, признанных непригодными для рисования и исключённых из выбора шрифта.
    pub unusable_faces: u64,
    /// Лиц, которые не удалось разобрать как шрифт.
    pub unreadable_faces: u64,
    /// Байты масок ТЕКУЩЕГО кадра (то, что вернул последний `render`). Эти маски не входят в
    /// `cue_cache.current_bytes`, если реплика не помещается в кэш: кадр держит их сам,
    /// и их объём ограничен одним кадром.
    pub current_frame_bytes: usize,
}
