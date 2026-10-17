//! Дополнительные проверки кэшей глифов по итогам ревью: картинки у края кадра делят маску с
//! кэшем (окно в общем буфере), а счётчики статистики и пиксели не зависят от прогретости.

use super::ass_glyph_caches::{cues_of, pictures, render_at};
use super::*;
use crate::overlay::{OverlayChange, detect_overlay_change};

/// Надпись, нижняя часть которой уходит за нижний край кадра 1280x720 (кегль 48, обводка 3).
const BOTTOM_EDGE: &str = r"{\an7\bord3\shad0\move(100,690,140,690,0,1000)}HHH";

#[test]
fn text_cut_by_the_canvas_edge_moves_without_replacing_the_mask() {
    let cues = cues_of(&[BOTTOM_EDGE]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Моменты 0 и 500 мс: сдвиг ровно на 20 целых пикселей (40 px за секунду), срез по нижнему краю тот же.
    let first = render_at(&mut rasterizer, &cues, 0);
    let second = render_at(&mut rasterizer, &cues, 500);
    assert!(!first.bitmaps().is_empty());

    for (before, after) in first.bitmaps().iter().zip(second.bitmaps()) {
        // Картинка обрезана по холсту: окно в общей маске, а не копия.
        assert!(
            before.origin().y + before.height() as i32 == HEIGHT as i32,
            "the picture must be cut by the bottom edge"
        );
        assert!(before.stride() > before.width() as usize);
        assert!(before.shows_same_pixels_as(after));
        assert!(std::sync::Arc::ptr_eq(
            before.shared_coverage(),
            after.shared_coverage()
        ));
        assert_eq!(after.origin().x - before.origin().x, 20);
    }
    assert_eq!(
        detect_overlay_change(first.bitmaps(), second.bitmaps()),
        OverlayChange::Moved
    );
}

#[test]
fn text_cut_by_the_canvas_edge_has_the_pixels_of_the_uncut_text() {
    // Те же буквы целиком внутри кадра, на целое число пикселей выше: срезанная картинка
    // обязана совпасть с верхними строками целой.
    let cut_cues = cues_of(&[r"{\an7\bord3\shad0\pos(100,690)}HHH"]);
    let whole_cues = cues_of(&[r"{\an7\bord3\shad0\pos(100,300)}HHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let cut = render_at(&mut rasterizer, &cut_cues, 0);
    let whole = render_at(&mut rasterizer, &whole_cues, 0);
    assert_eq!(cut.bitmaps().len(), whole.bitmaps().len());
    for (cut_image, whole_image) in cut.bitmaps().iter().zip(whole.bitmaps()) {
        assert!(cut_image.height() < whole_image.height());
        assert_eq!(cut_image.width(), whole_image.width());
        for row in 0..cut_image.height() {
            assert_eq!(
                cut_image.coverage_row(row),
                whole_image.coverage_row(row),
                "row {row} of {:?}",
                cut_image.kind()
            );
        }
    }
}

#[test]
fn horizontal_edge_cut_picture_matches_a_cold_render_pixel_for_pixel() {
    // Нижний край: холодный растеризатор (без кэшей) и прогретый дают одни и те же пиксели
    // окна; смещённая слева картинка (отрицательная x) тоже режется окном.
    let cues = cues_of(&[r"{\an7\bord3\move(-40,690,40,690,0,1000)}HHH"]);
    let mut warm = rasterizer(ROOMY_LIMITS);
    for moment in [0, 100, 300] {
        render_at(&mut warm, &cues, moment);
    }
    let warm_picture = pictures(&render_at(&mut warm, &cues, 500));
    let mut cold = rasterizer(ROOMY_LIMITS);
    let cold_picture = pictures(&render_at(&mut cold, &cues, 500));
    assert!(!cold_picture.is_empty());
    assert_eq!(warm_picture, cold_picture);
}

use crate::renderer::RasterStatistics;

/// Счётчики работы, которые не должны зависеть от прогретости кэшей глифов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkCounters {
    skipped_oversized_bitmaps: u64,
    skipped_borders: u64,
    failed_blur_layers: u64,
    invalid_ass_drawings: u64,
    skipped_glyphs: u64,
}

impl WorkCounters {
    fn of(statistics: &RasterStatistics) -> Self {
        Self {
            skipped_oversized_bitmaps: statistics.skipped_oversized_bitmaps,
            skipped_borders: statistics.skipped_borders,
            failed_blur_layers: statistics.failed_blur_layers,
            invalid_ass_drawings: statistics.invalid_ass_drawings,
            skipped_glyphs: statistics.skipped_glyphs,
        }
    }

    fn minus(self, earlier: Self) -> Self {
        Self {
            skipped_oversized_bitmaps: self.skipped_oversized_bitmaps
                - earlier.skipped_oversized_bitmaps,
            skipped_borders: self.skipped_borders - earlier.skipped_borders,
            failed_blur_layers: self.failed_blur_layers - earlier.failed_blur_layers,
            invalid_ass_drawings: self.invalid_ass_drawings - earlier.invalid_ass_drawings,
            skipped_glyphs: self.skipped_glyphs - earlier.skipped_glyphs,
        }
    }
}

/// Сцена, в которой срабатывают защиты: огромный масштаб (глиф шире предела стороны), огромная
/// обводка, огромное размытие и битый рисунок. Каждая строка двигается на целые пиксели, чтобы
/// реплика перерисовывалась каждый кадр, а кэши глифов оставались тёплыми.
const GUARDED_SCENE: &[&str] = &[
    r"{\an5\move(300,300,700,300,0,1000)\bord0\fscx9000\fscy300}HH",
    r"{\an5\move(300,100,500,100,0,1000)\bord100000000}H",
    r"{\an5\move(300,500,700,500,0,1000)\fs500\bord2\blur100}H",
    r"{\an7\move(100,600,300,600,0,1000)\p1}n 1 1 l 5 5",
];

fn tight_rasterizer() -> SubtitleRasterizer {
    rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        // Предел стороны битмапа равен ширине кадра: огромные слои не помещаются.
        input: RasterInputLimits {
            max_canvas_side: WIDTH,
            ..ROOMY_INPUT
        },
    })
}

#[test]
fn guard_counters_do_not_depend_on_warm_glyph_caches() {
    let cues = cues_of(GUARDED_SCENE);
    // Холодный: кадр на 500 мс с нуля.
    let mut cold = tight_rasterizer();
    render_at(&mut cold, &cues, 500);
    let cold_work = WorkCounters::of(&cold.statistics());
    // Тёплый: тот же кадр после других кадров, наполнивших кэши (в том числе записи неудач).
    let mut warm = tight_rasterizer();
    for moment in [0, 250, 750] {
        render_at(&mut warm, &cues, moment);
    }
    let before = WorkCounters::of(&warm.statistics());
    render_at(&mut warm, &cues, 500);
    let warm_work = WorkCounters::of(&warm.statistics()).minus(before);
    assert_eq!(warm_work, cold_work);
    // Сцена вообще задевает защиты, иначе сравнение ничего не доказывает.
    // (`skipped_borders`, `failed_blur_layers` и `skipped_glyphs` тегами не вызвать: это сбои
    // обводчика, внутреннего инварианта и загрузки контура; они входят в сравнение нулями.)
    assert!(cold_work.skipped_oversized_bitmaps >= 3, "{cold_work:?}");
    assert_eq!(cold_work.invalid_ass_drawings, 1, "{cold_work:?}");
}

// --- Испытание анимированных реплик в кэше реплик (через публичный растеризатор) ---

const STATIC_TEXT: &str = r"{\an7\pos(100,100)\bord2}Static";
/// Двигается на целые пиксели: на каждом кадре новый ключ кэша реплик (мгновенное состояние).
const ANIMATED_TEXT: &str = r"{\an7\move(100,300,500,300,0,1000)\bord2}Moving";

/// Размеры записей кэша реплик: статичной и анимированной (замеряются в щедром кэше).
fn cue_entry_sizes() -> (usize, usize) {
    let mut roomy = rasterizer(ROOMY_LIMITS);
    render_at(&mut roomy, &cues_of(&[STATIC_TEXT]), 0);
    let static_bytes = roomy.statistics().cue_cache.current_bytes;
    render_at(&mut roomy, &cues_of(&[ANIMATED_TEXT]), 100);
    let animated_bytes = roomy.statistics().cue_cache.current_bytes - static_bytes;
    assert!(static_bytes > 0 && animated_bytes > 0);
    (static_bytes, animated_bytes)
}

/// Растеризатор, в кэш реплик которого помещается статичная реплика и три анимированных.
fn rasterizer_for_one_static_and_three_animated() -> SubtitleRasterizer {
    let (static_bytes, animated_bytes) = cue_entry_sizes();
    rasterizer(RasterCacheLimits {
        cue_bitmaps_bytes: static_bytes + 3 * animated_bytes + animated_bytes / 2,
        ..ROOMY_LIMITS
    })
}

#[test]
fn animated_cue_entries_are_evicted_before_a_static_cue() {
    let mut rasterizer = rasterizer_for_one_static_and_three_animated();
    let fixed = cues_of(&[STATIC_TEXT]);
    let animated = cues_of(&[ANIMATED_TEXT]);
    render_at(&mut rasterizer, &fixed, 0);
    // Поток разных состояний анимации заполняет кэш и вытесняет только самих себя.
    for moment in [100, 200, 300, 400, 500, 600] {
        render_at(&mut rasterizer, &animated, moment);
    }
    let before = rasterizer.statistics().cue_cache;
    assert!(
        before.evictions >= 3,
        "the flood must overflow the cache: {before:?}"
    );
    // Статичная реплика осталась в кэше: повторная сборка кадра берёт её оттуда.
    render_at(&mut rasterizer, &fixed, 700);
    let after = rasterizer.statistics().cue_cache;
    assert_eq!(after.hits, before.hits + 1, "the static cue was evicted");
    assert_eq!(after.misses, before.misses);
}

#[test]
fn repeated_animated_state_is_promoted_and_survives_the_flood() {
    let mut rasterizer = rasterizer_for_one_static_and_three_animated();
    let animated = cues_of(&[ANIMATED_TEXT]);
    render_at(&mut rasterizer, &animated, 100);
    render_at(&mut rasterizer, &animated, 200);
    // Состояние 100 мс встретилось снова: попадание делает запись обычной.
    let before_repeat = rasterizer.statistics().cue_cache;
    render_at(&mut rasterizer, &animated, 100);
    assert_eq!(
        rasterizer.statistics().cue_cache.hits,
        before_repeat.hits + 1
    );
    for moment in [300, 400, 500, 600, 700] {
        render_at(&mut rasterizer, &animated, moment);
    }
    let before = rasterizer.statistics().cue_cache;
    render_at(&mut rasterizer, &animated, 100);
    let promoted = rasterizer.statistics().cue_cache;
    assert_eq!(
        promoted.hits,
        before.hits + 1,
        "the promoted state survived"
    );
    // Непродвинутое состояние давно вытеснено.
    render_at(&mut rasterizer, &animated, 200);
    let forgotten = rasterizer.statistics().cue_cache;
    assert_eq!(forgotten.misses, promoted.misses + 1);
}

#[test]
fn static_cue_with_warm_glyph_caches_but_cold_cue_cache_renders_identically() {
    let fixed = cues_of(&[r"{\an5\pos(400,300)\bord3\shad2\blur1\be1\fscx120}Static text"]);
    // Эталон: полностью холодный растеризатор.
    let mut cold = rasterizer(ROOMY_LIMITS);
    let expected = pictures(&render_at(&mut cold, &fixed, 0));
    // Кэши глифов тёплые, кэш реплик пуст (запись реплик вытеснена крошечным лимитом).
    let mut warm = rasterizer(RasterCacheLimits {
        cue_bitmaps_bytes: 1,
        ..ROOMY_LIMITS
    });
    render_at(&mut warm, &fixed, 0);
    let glyph_misses = warm.statistics().glyph_bitmap_cache.misses;
    render_at(&mut warm, &cues_of(&[]), 100);
    let second = pictures(&render_at(&mut warm, &fixed, 200));
    let statistics = warm.statistics();
    assert_eq!(statistics.cue_cache.entries, 0, "the cue cache stayed cold");
    assert_eq!(
        statistics.glyph_bitmap_cache.misses, glyph_misses,
        "glyph masks came from the warm cache"
    );
    assert_eq!(second, expected);
}

#[test]
fn tiny_shaping_budget_evicts_runs_but_never_changes_the_picture() {
    let texts: Vec<String> = (0..12)
        .map(|index| {
            format!(
                r"{{\an7\pos(100,{})\bord1}}Shaped run number {index}",
                40 + index * 50
            )
        })
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let cues = cues_of(&refs);
    let mut roomy = rasterizer(ROOMY_LIMITS);
    let expected = pictures(&render_at(&mut roomy, &cues, 0));
    assert_eq!(roomy.statistics().shaping_cache.evictions, 0);

    // Бюджет на два-три участка: остальные вытесняются по давности.
    let mut cramped = rasterizer(RasterCacheLimits {
        shaping_cache_bytes: 800,
        ..ROOMY_LIMITS
    });
    assert_eq!(pictures(&render_at(&mut cramped, &cues, 0)), expected);
    let statistics = cramped.statistics().shaping_cache;
    assert!(statistics.evictions > 0, "{statistics:?}");
    assert!(statistics.current_bytes <= 800, "{statistics:?}");
    assert!(
        statistics.entries >= 1,
        "the cache is not cleared as a whole"
    );
}

#[test]
fn fully_visible_picture_shares_the_whole_cached_mask_between_frames() {
    let cues = cues_of(&[r"{\an7\bord2\move(100,100,300,100,0,1000)}HHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 0);
    let second = render_at(&mut rasterizer, &cues, 500);
    for (before, after) in first.bitmaps().iter().zip(second.bitmaps()) {
        // Маска целиком на холсте: картинка показывает весь общий буфер без смещения.
        assert_eq!(before.coverage().len(), before.shared_coverage().len());
        assert!(before.shows_same_pixels_as(after));
        assert_eq!(after.origin().x - before.origin().x, 100);
    }
    assert_eq!(
        detect_overlay_change(first.bitmaps(), second.bitmaps()),
        OverlayChange::Moved
    );
}
