//! Трёхуровневый кэш глифов (`crate::glyph_cache`) через публичный `SubtitleRasterizer`:
//! движение на целые пиксели не растеризует буквы заново, склейки и контуры переиспользуются,
//! лимиты и смена шрифтов соблюдаются, а картинки не зависят от прогретости кэшей.
//!
//! Проверяется результат: счётчики статистики растеризатора и сами пиксели картинок кадра.

use std::sync::Arc;

use subtitle_core::SubtitleTime;

use super::ass::{fresh_source, glyph_ink};
use super::*;
use crate::ass::AssRenderOptions;
use crate::fonts::test_fonts::{noto_regular_bytes, remap_letter_a};
use crate::fonts::{AttachedFontLimits, TextLanguagePreference};
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, load_ass_cues};

/// Реплики стиля `Default` (10 секунд) с полем `Effect`; тексты - тела `Dialogue`.
pub(super) fn cues_with_effect(effect: &str, texts: &[&str]) -> Vec<AssTestCue> {
    let lines: Vec<String> = texts
        .iter()
        .map(|text| format!("Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,{effect},{text}"))
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &refs);
    load_ass_cues(&file, fresh_source())
}

pub(super) fn cues_of(texts: &[&str]) -> Vec<AssTestCue> {
    cues_with_effect("", texts)
}

pub(super) fn render_at(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    moment_ms: i64,
) -> SubtitleOverlay {
    let list: Vec<RenderableCue<'_>> = cues.iter().map(AssTestCue::renderable).collect();
    let appearance = SubtitleAppearance::youtube_like();
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::from_millis(moment_ms),
            cues: &list,
            canvas: canvas(),
            appearance: &appearance,
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

/// Всё, что видно в картинке: положение, размеры, шаг, цвет, вид и каждый байт маски.
pub(super) type Picture = (
    i32,
    i32,
    u32,
    u32,
    usize,
    [u8; 4],
    SubtitleBitmapKind,
    Vec<u8>,
);

pub(super) fn pictures(overlay: &SubtitleOverlay) -> Vec<Picture> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| {
            let color = bitmap.color();
            (
                bitmap.origin().x,
                bitmap.origin().y,
                bitmap.width(),
                bitmap.height(),
                bitmap.stride(),
                [color.red, color.green, color.blue, color.alpha],
                bitmap.kind(),
                bitmap.coverage().to_vec(),
            )
        })
        .collect()
}

/// Движение на 100 px/с: моменты 0, 250, 500 мс дают положения 100, 200, 300 - целые пиксели.
const WHOLE_PIXEL_MOVE: &str = r"{\an7\bord3\shad2\move(100,100,500,100,0,1000)}HHH";

#[test]
fn move_by_whole_pixels_does_not_rasterize_glyph_bitmaps_again() {
    let cues = cues_of(&[WHOLE_PIXEL_MOVE]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 0);
    let after_first = rasterizer.statistics();
    assert!(
        after_first.glyph_bitmap_cache.misses > 0,
        "the first frame must rasterize the letters"
    );
    let second = render_at(&mut rasterizer, &cues, 250);
    let third = render_at(&mut rasterizer, &cues, 500);
    let after_third = rasterizer.statistics();

    // Растеризаций (промахов кэша масок) больше нет, все обращения - попадания.
    assert_eq!(
        after_third.glyph_bitmap_cache.misses,
        after_first.glyph_bitmap_cache.misses,
    );
    assert!(after_third.glyph_bitmap_cache.hits > after_first.glyph_bitmap_cache.hits);
    // Картинка при этом действительно едет: на 100 px за каждые 250 мс.
    let (left_first, left_second, left_third) = (
        glyph_ink(&first).0,
        glyph_ink(&second).0,
        glyph_ink(&third).0,
    );
    assert_eq!(left_second - left_first, 100);
    assert_eq!(left_third - left_second, 100);
    // Маски те же: сдвиг не меняет ни одного пикселя заливки.
    let masks = |overlay: &SubtitleOverlay| {
        of_kind(overlay, SubtitleBitmapKind::Glyph)
            .iter()
            .map(|bitmap| bitmap.coverage().to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(masks(&first), masks(&third));
}

#[test]
fn moving_run_is_served_from_the_composite_cache() {
    let cues = cues_of(&[WHOLE_PIXEL_MOVE]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 0);
    let after_first = rasterizer.statistics().composite_cache;
    assert!(after_first.misses > 0, "the first frame builds the run");
    render_at(&mut rasterizer, &cues, 250);
    render_at(&mut rasterizer, &cues, 500);
    let after_third = rasterizer.statistics().composite_cache;
    assert_eq!(after_third.misses, after_first.misses);
    assert_eq!(after_third.hits, after_first.hits + 2);
}

#[test]
fn banner_scroll_reuses_glyph_bitmaps_and_runs() {
    // 10 мс на пиксель: за секунду ровно 100 px.
    let cues = cues_with_effect("Banner;10;0;0", &[r"{\bord2\fs60}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 1000);
    let after_first = rasterizer.statistics();
    let second = render_at(&mut rasterizer, &cues, 2000);
    let third = render_at(&mut rasterizer, &cues, 3500);
    let after_third = rasterizer.statistics();
    assert_eq!(
        after_third.glyph_bitmap_cache.misses,
        after_first.glyph_bitmap_cache.misses
    );
    assert_eq!(
        after_third.composite_cache.misses,
        after_first.composite_cache.misses
    );
    assert_eq!(glyph_ink(&first).0 - glyph_ink(&second).0, 100);
    assert_eq!(glyph_ink(&second).0 - glyph_ink(&third).0, 150);
}

#[test]
fn fractional_move_rasterizes_again_but_keeps_outlines() {
    // Полпикселя за шаг: субпиксельная ошибка (1/8 px) меняется, маски растеризуются заново,
    // а контуры букв берутся из кэша контуров.
    let cues = cues_of(&[r"{\an7\bord0\move(100,100,100.5,100,0,1000)}HHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 0);
    let after_first = rasterizer.statistics();
    render_at(&mut rasterizer, &cues, 500);
    let after_second = rasterizer.statistics();
    assert!(after_second.glyph_bitmap_cache.misses > after_first.glyph_bitmap_cache.misses);
    assert_eq!(
        after_second.outline_cache.misses, after_first.outline_cache.misses,
        "glyph outlines must come from the outline cache"
    );
}

#[test]
fn same_text_in_another_cue_shares_the_outlines() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = cues_of(&[r"{\an7\bord2\pos(100,100)}Share"]);
    render_at(&mut rasterizer, &first, 0);
    let after_first = rasterizer.statistics().outline_cache;
    // Другая реплика (другой ключ, другое место), те же буквы и стиль.
    let second = cues_of(&[r"{\an7\bord2\pos(300,300)}Share"]);
    render_at(&mut rasterizer, &second, 0);
    let after_second = rasterizer.statistics().outline_cache;
    assert_eq!(after_second.misses, after_first.misses);
    assert!(after_second.hits > after_first.hits);
}

#[test]
fn invalid_drawing_is_cached_but_still_counted_for_every_cue() {
    // Рисунок без `m` отвергнут; движение заставляет рисовать реплику заново на каждом кадре.
    let cues = cues_of(&[r"{\an7\move(100,100,300,100,0,1000)\p1}n 1 1 l 5 5"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 0);
    let after_first = rasterizer.statistics();
    assert_eq!(after_first.invalid_ass_drawings, 1);
    render_at(&mut rasterizer, &cues, 100);
    let after_second = rasterizer.statistics();
    // Счётчик не зависит от прогретости кэша, а разбор не повторялся.
    assert_eq!(after_second.invalid_ass_drawings, 2);
    assert_eq!(
        after_second.outline_cache.misses,
        after_first.outline_cache.misses
    );
    assert!(after_second.outline_cache.hits > after_first.outline_cache.hits);
}

/// Сцена с рисунком, обводкой, тенью, размытием, `\be`, поворотом и плашкой.
const RICH_SCENE: &[&str] = &[
    r"{\an5\pos(400,200)\bord4\shad3\blur2\be1\frz10\t(0,1000,\fscx130)}Rich blur",
    r"{\an7\pos(100,400)\bord2\fs60\fsp3\1a&H40&}Spaced\Nlines",
    r"{\an1\pos(50,650)\p1\bord2\shad2}m 0 0 l 60 0 60 60 0 60 c",
    r"{\an9\move(1200,50,1100,80,0,2000)\bord1\blur1}Move me",
];

#[test]
fn cold_and_warm_caches_produce_identical_pictures() {
    let cues = cues_of(RICH_SCENE);
    // Прогретый: за плечами кадры, которые наполнили все три кэша.
    let mut warm = rasterizer(ROOMY_LIMITS);
    for moment in [0, 100, 250, 700, 1500] {
        render_at(&mut warm, &cues, moment);
    }
    let warm_picture = pictures(&render_at(&mut warm, &cues, 400));
    // Холодный: один кадр с нуля.
    let mut cold = rasterizer(ROOMY_LIMITS);
    let cold_picture = pictures(&render_at(&mut cold, &cues, 400));
    assert!(!cold_picture.is_empty());
    assert_eq!(warm_picture, cold_picture);
    // Другая история (обратный порядок) даёт те же пиксели.
    let mut reversed = rasterizer(ROOMY_LIMITS);
    for moment in [1500, 700, 250, 100, 0] {
        render_at(&mut reversed, &cues, moment);
    }
    assert_eq!(
        pictures(&render_at(&mut reversed, &cues, 400)),
        cold_picture
    );
}

#[test]
fn tiny_limits_still_give_identical_pictures_and_cut_at_frame_start() {
    let cues = cues_of(RICH_SCENE);
    let mut roomy = rasterizer(ROOMY_LIMITS);
    let expected = pictures(&render_at(&mut roomy, &cues, 400));

    let tiny = RasterCacheLimits {
        glyph_bitmaps_bytes: 4 * 1024,
        glyph_composites_bytes: 4 * 1024,
        glyph_outline_items: 8,
        ..ROOMY_LIMITS
    };
    let mut cramped = rasterizer(tiny);
    // Внутри кадра вытеснения нет, поэтому картинка не страдает от маленьких лимитов.
    assert_eq!(pictures(&render_at(&mut cramped, &cues, 400)), expected);
    for moment in [500, 600, 700] {
        render_at(&mut cramped, &cues, moment);
    }
    // Пустой кадр: лимиты применяются в его начале, до любой работы.
    render_at(&mut cramped, &[], 800);
    let statistics = cramped.statistics();
    assert!(statistics.outline_cache.current_bytes <= 8);
    assert!(statistics.glyph_bitmap_cache.current_bytes <= 4 * 1024);
    assert!(statistics.composite_cache.current_bytes <= 4 * 1024);
    assert!(statistics.outline_cache.evictions > 0);
    assert!(statistics.glyph_bitmap_cache.evictions > 0);
    assert!(statistics.composite_cache.evictions > 0);
    // После вытеснения картинка остаётся прежней.
    assert_eq!(pictures(&render_at(&mut cramped, &cues, 400)), expected);
}

#[test]
fn attaching_a_font_drops_cached_glyphs_of_the_old_faces() {
    let mut bytes = noto_regular_bytes();
    let private_char = remap_letter_a(&mut bytes);
    let text = format!(r"{{\an7\bord2\pos(100,100)}}x{private_char}y");
    let cues = cues_of(&[text.as_str()]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let bare = pictures(&render_at(&mut rasterizer, &cues, 0));
    let populated = rasterizer.statistics();
    assert!(populated.outline_cache.entries > 0);
    assert!(populated.glyph_bitmap_cache.entries > 0);
    assert!(populated.composite_cache.entries > 0);

    let limits = AttachedFontLimits {
        max_font_bytes: 16 * 1024 * 1024,
        max_total_bytes: 64 * 1024 * 1024,
        max_faces: 8,
    };
    let registration = rasterizer
        .register_attached_font(Arc::from(bytes), &limits)
        .expect("font registers");
    assert_eq!(
        registration.status,
        crate::fonts::AttachedFontStatus::Registered
    );
    // Сразу после регистрации в кэшах глифов пусто (счётчики сохраняются).
    let cleared = rasterizer.statistics();
    assert_eq!(cleared.outline_cache.entries, 0);
    assert_eq!(cleared.glyph_bitmap_cache.entries, 0);
    assert_eq!(cleared.composite_cache.entries, 0);

    // Первый кадр после регистрации не берёт ничего из старых записей и рисует иначе.
    let attached = pictures(&render_at(&mut rasterizer, &cues, 0));
    let rebuilt = rasterizer.statistics();
    assert_eq!(
        rebuilt.glyph_bitmap_cache.hits,
        cleared.glyph_bitmap_cache.hits
    );
    assert_eq!(rebuilt.composite_cache.hits, cleared.composite_cache.hits);
    assert_ne!(attached, bare, "the attached glyph must change the picture");
}
